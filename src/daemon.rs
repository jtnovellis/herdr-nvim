//! Headless Neovim daemons: one per Herdr tab. They are started detached
//! (`setsid`) so they outlive the plugin command that spawned them and the
//! sidebar clients that attach to them.

use crate::config::Config;
use crate::herdr::Herdr;
use crate::state::{now_unix, state_dir, DaemonRecord, State, StateFile};
use anyhow::{bail, Context as _, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

const SOCKET_PATH_MAX: usize = 96;
const SPAWN_TIMEOUT: Duration = Duration::from_secs(10);
const STARTING_STALE_SECS: u64 = 20;

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions.
    unsafe { libc::getuid() }
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn short_hash(input: &str) -> String {
    // FNV-1a; only used to keep file names unique per session.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in input.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:06x}", hash & 0xff_ffff)
}

/// Directory for daemon sockets. Unix socket paths are limited to ~104 bytes
/// on macOS, so prefer short locations.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        let dir = PathBuf::from(dir).join("herdr-nvim");
        if dir.as_os_str().len() < SOCKET_PATH_MAX - 24 {
            return dir;
        }
    }
    let tmp = env::temp_dir().join(format!("herdr-nvim-{}", uid()));
    if tmp.as_os_str().len() < SOCKET_PATH_MAX - 24 {
        return tmp;
    }
    PathBuf::from(format!("/tmp/herdr-nvim-{}", uid()))
}

pub fn socket_path(session_key: &str, tab_id: &str) -> PathBuf {
    let name = format!("{}-{}.sock", short_hash(session_key), sanitize(tab_id));
    let path = runtime_dir().join(&name);
    if path.as_os_str().len() <= SOCKET_PATH_MAX {
        path
    } else {
        PathBuf::from(format!("/tmp/herdr-nvim-{}", uid())).join(name)
    }
}

/// Create the socket directory and refuse to use one we do not own.
fn prepare_runtime_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        bail!("{} exists but is not a directory", dir.display());
    }
    if meta.uid() != uid() {
        bail!(
            "{} is owned by another user; refusing to place sockets there",
            dir.display()
        );
    }
    if meta.mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("cannot restrict permissions on {}", dir.display()))?;
    }
    Ok(())
}

pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: signal 0 only checks for existence/permission.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// `lstart` (5 tokens) and args of a process, as `ps -o lstart=,args=` prints them.
fn ps_line(pid: u32) -> Option<String> {
    ps_lines(&[pid]).remove(&pid)
}

/// Split `ps -o pid=,lstart=,args=` output into `pid -> "<lstart> <args>"`.
/// Kept separate from the process call so it can be tested without forking.
fn parse_ps_table(text: &str) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((pid, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if let Ok(pid) = pid.trim().parse::<u32>() {
            out.insert(pid, rest.trim().to_string());
        }
    }
    out
}

/// Number of pids past which one `ps` for the whole list beats one call each.
///
/// Counter-intuitively, BSD `ps` is far slower with a pid *list* than with a
/// single pid -- measured on macOS: 3.3 ms for `-p ONE` but 18.9 ms for
/// `-p A,B`, flat in the number of pids, because the list form walks the whole
/// process table. Most users have a handful of tabs, so ask per pid until the
/// arithmetic flips.
const PS_BATCH_THRESHOLD: usize = 6;

/// `ps` output for each of `pids`, keyed by pid.
fn ps_lines(pids: &[u32]) -> HashMap<u32, String> {
    let pids: Vec<u32> = {
        let mut v: Vec<u32> = pids.iter().copied().filter(|p| *p != 0).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    if pids.is_empty() {
        return HashMap::new();
    }
    if pids.len() < PS_BATCH_THRESHOLD {
        return pids
            .iter()
            .filter_map(|pid| {
                ps_query(&pid.to_string()).and_then(|t| parse_ps_table(&t).into_iter().next())
            })
            .collect();
    }
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    ps_query(&list)
        .map(|t| parse_ps_table(&t))
        .unwrap_or_default()
}

/// Raw `ps -o pid=,lstart=,args= -p <spec>` output.
fn ps_query(spec: &str) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", "pid=,lstart=,args=", "-p", spec])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    // `ps` exits non-zero when none of the pids exist; an empty table is the
    // correct answer either way, so the status is not consulted.
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Split a `ps -o lstart=,args=` line into (lstart, args).
fn split_ps_line(line: &str) -> Option<(String, String)> {
    let mut tokens = line.split_whitespace();
    let lstart: Vec<&str> = tokens.by_ref().take(5).collect();
    if lstart.len() < 5 {
        return None;
    }
    let args: Vec<&str> = tokens.collect();
    Some((lstart.join(" "), args.join(" ")))
}

/// Does this `ps` line describe our daemon listening on `socket`, started
/// at `lstart` (when known)? Guards against pid reuse.
pub fn ps_matches(line: &str, socket: &Path, lstart: Option<&str>) -> bool {
    let Some((seen_lstart, args)) = split_ps_line(line) else {
        return false;
    };
    let socket = socket.to_string_lossy();
    if !args.contains("--listen") || !args.contains(socket.as_ref()) {
        return false;
    }
    match lstart {
        Some(expected) => expected.split_whitespace().collect::<Vec<_>>().join(" ") == seen_lstart,
        None => true,
    }
}

pub fn capture_lstart(pid: u32) -> Option<String> {
    ps_line(pid)
        .and_then(|l| split_ps_line(&l))
        .map(|(lstart, _)| lstart)
}

/// True only when the pid is alive *and* `ps` proves it is our daemon.
pub fn pid_is_ours(record: &DaemonRecord) -> bool {
    pid_is_ours_with(record, ps_line(record.pid).as_deref())
}

/// `pid_is_ours` against an already-fetched `ps` line, for callers that
/// snapshot many records at once with [`ps_snapshot`].
pub fn pid_is_ours_with(record: &DaemonRecord, ps: Option<&str>) -> bool {
    if !pid_alive(record.pid) {
        return false;
    }
    ps.map(|line| ps_matches(line, &record.socket, record.ps_lstart.as_deref()))
        .unwrap_or(false)
}

pub fn socket_ok(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

pub fn is_running(record: &DaemonRecord) -> bool {
    is_running_with(record, ps_line(record.pid).as_deref())
}

/// `is_running` against an already-fetched `ps` line. Scanning every record in
/// the registry used to fork `ps` once per record.
pub fn is_running_with(record: &DaemonRecord, ps: Option<&str>) -> bool {
    !record.starting && pid_is_ours_with(record, ps) && socket_ok(&record.socket)
}

/// One `ps` covering every pid in `records`, keyed by pid.
pub fn ps_snapshot<'a>(records: impl Iterator<Item = &'a DaemonRecord>) -> HashMap<u32, String> {
    ps_lines(&records.map(|r| r.pid).collect::<Vec<_>>())
}

fn log_path(session_key: &str, tab_id: &str) -> PathBuf {
    state_dir().join("logs").join(format!(
        "{}-{}.log",
        short_hash(session_key),
        sanitize(tab_id)
    ))
}

/// The plugin checkout holding the Lua half.
///
/// Herdr sets `HERDR_PLUGIN_ROOT` for everything it invokes, and that is the
/// authority: it stays right when the binary has been copied or symlinked
/// somewhere else, which the path-shape guess below cannot survive. The guess
/// is the fallback for the calls that do not come from Herdr -- the Lua side
/// shelling out to `ask`, or a developer running the binary by hand.
pub fn plugin_root() -> Option<PathBuf> {
    plugin_root_from(
        env::var_os("HERDR_PLUGIN_ROOT").as_deref(),
        env::current_exe().ok().as_deref(),
    )
}

/// The resolution rule, without reading the environment, so it can be tested
/// without mutating process-global state that other tests share.
fn plugin_root_from(reported: Option<&OsStr>, exe: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = reported
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|root| has_bundled_lua(root))
    {
        return Some(root);
    }
    plugin_root_of(exe?)
}

/// The checkout containing a binary at `<root>/target/<profile>/herdr-nvim`.
/// `None` when the binary was moved somewhere without that shape, in which
/// case the daemon simply gets no bundled Lua.
pub fn plugin_root_of(exe: &Path) -> Option<PathBuf> {
    let root = exe.parent()?.parent()?.parent()?;
    has_bundled_lua(root).then(|| root.to_path_buf())
}

fn has_bundled_lua(root: &Path) -> bool {
    root.join("lua")
        .join("herdr-nvim")
        .join("init.lua")
        .is_file()
}

/// Load the Lua half we ship with, but only if the user has not installed it
/// themselves. Runs after their config, so a lazy.nvim (or any other) install
/// always wins and there is never a second copy on the runtimepath. This is
/// what makes `herdr plugin install` alone enough: without it the daemon has
/// no :HerdrAsk, no annotations, and `pick-file` fails after gathering.
const BUNDLED_LUA_BOOTSTRAP: &str = "lua \
    if not pcall(require, 'herdr-nvim') then \
      local root = vim.env.HERDR_NVIM_PLUGIN_ROOT \
      if root and root ~= '' then \
        vim.opt.rtp:append(root) \
        pcall(vim.cmd, 'runtime plugin/herdr-nvim.lua') \
      end \
    end";

fn spawn(
    cfg: &Config,
    session_key: &str,
    tab_id: &str,
    socket: &Path,
    cwd: &Path,
    workspace_id: Option<&str>,
) -> Result<(u32, Option<String>)> {
    if let Some(dir) = socket.parent() {
        prepare_runtime_dir(dir)?;
    }
    let log = log_path(session_key, tab_id);
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
    }
    let log_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log)
        .with_context(|| format!("cannot open {}", log.display()))?;

    let mut cmd = Command::new(&cfg.nvim);
    cmd.arg("--headless")
        .arg("--listen")
        .arg(socket)
        .args(&cfg.daemon_args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file.try_clone()?))
        .stderr(Stdio::from(log_file));

    // Context that only made sense for the command that started us.
    for key in [
        "HERDR_PANE_ID",
        "HERDR_PLUGIN_ACTION_ID",
        "HERDR_PLUGIN_CONTEXT_JSON",
        "HERDR_PLUGIN_EVENT",
        "HERDR_PLUGIN_EVENT_JSON",
        "HERDR_PLUGIN_ENTRYPOINT_ID",
        "HERDR_PLUGIN_CLICKED_URL",
        "HERDR_PLUGIN_LINK_HANDLER_ID",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("HERDR_NVIM_DAEMON", "1")
        .env("HERDR_NVIM_TAB_ID", tab_id)
        .env("HERDR_NVIM_SOCKET", socket)
        .env("HERDR_TAB_ID", tab_id)
        .env("HERDR_NVIM_STATE_DIR", state_dir());
    if let Some(ws) = workspace_id {
        cmd.env("HERDR_WORKSPACE_ID", ws);
    }
    if let Some(dir) = crate::config::config_dir() {
        cmd.env("HERDR_NVIM_CONFIG_DIR", dir);
    }
    // Passed by environment rather than interpolated into the -c string: a path
    // needs no Lua escaping and cannot break the command line.
    if let Some(root) = plugin_root() {
        cmd.env("HERDR_NVIM_PLUGIN_ROOT", root);
        cmd.arg("-c").arg(BUNDLED_LUA_BOOTSTRAP);
    }
    if let Ok(exe) = env::current_exe() {
        cmd.env("HERDR_NVIM_BIN", exe);
    }
    // SAFETY: setsid is async-signal-safe and has no preconditions here.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to start `{}`", cfg.nvim))?;

    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        if socket_ok(socket) {
            break;
        }
        if let Some(status) = child.try_wait()? {
            bail!("nvim daemon exited early ({status}); see {}", log.display());
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!(
                "timed out waiting for nvim daemon socket {}",
                socket.display()
            );
        }
        sleep(Duration::from_millis(40));
    }
    let pid = child.id();
    Ok((pid, capture_lstart(pid)))
}

/// Per-tab lock: spawning/stopping a tab's daemon is serialised without
/// holding the global state lock across the (slow) spawn.
pub(crate) struct TabLock {
    _file: fs::File,
}

pub(crate) fn tab_lock(session: &str, tab: &str) -> Result<TabLock> {
    let dir = state_dir().join("locks");
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(format!("{}-{}.lock", short_hash(session), sanitize(tab)));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let deadline = Instant::now() + SPAWN_TIMEOUT * 3;
    loop {
        // SAFETY: flock on a valid, owned file descriptor.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(TabLock { _file: file });
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            bail!("cannot lock {}: {err}", path.display());
        }
        if Instant::now() >= deadline {
            bail!("another herdr-nvim command is still starting the daemon for {tab}");
        }
        sleep(Duration::from_millis(50));
    }
}

/// Return the running daemon for a tab, starting one when needed. Owns all
/// locking: the global state lock is never held while spawning or stopping.
pub fn ensure(
    cfg: &Config,
    herdr: &Herdr,
    session: &str,
    tab: &str,
    cwd: &Path,
    ws: Option<&str>,
) -> Result<(DaemonRecord, bool)> {
    let _guard = tab_lock(session, tab)?;

    let previous = {
        let file = StateFile::open()?;
        let state = file.load()?;
        state.get(session, tab).cloned()
    };
    if let Some(rec) = &previous {
        if is_running(rec) {
            return Ok((rec.clone(), false));
        }
        // Never orphan a live daemon we merely cannot reach.
        if pid_is_ours(rec) {
            stop(cfg, herdr, rec, tab);
        }
    }

    let socket = socket_path(session, tab);
    let _ = fs::remove_file(&socket);
    let (pid, lstart) = spawn(cfg, session, tab, &socket, cwd, ws)?;

    let file = StateFile::open()?;
    let mut state = file.load()?;
    let current = state.get(session, tab).cloned();
    let record = DaemonRecord {
        pid,
        socket,
        cwd: cwd.to_path_buf(),
        sidebar_pane_id: current.as_ref().and_then(|r| r.sidebar_pane_id.clone()),
        sidebar_terminal_id: current.as_ref().and_then(|r| r.sidebar_terminal_id.clone()),
        workspace_id: ws.map(str::to_string),
        ps_lstart: lstart,
        starting: false,
        started_unix: now_unix(),
        layout: current.as_ref().and_then(|r| r.layout.clone()),
    };
    state.insert(session, tab, record.clone());
    file.save(&state)?;
    Ok((record, true))
}

/// Evaluate a VimL expression in the daemon.
///
/// This used to spawn `nvim --server <sock> --remote-expr <expr>` and poll for
/// the child: a whole Neovim start plus the poll interval, ~61 ms per call, on
/// the `edit`, `title` and picker-open paths. Speaking msgpack-rpc to the
/// socket the daemon already listens on is ~0.07 ms.
///
/// `cfg` is no longer needed to find an `nvim` binary, but stays in the
/// signature because callers hold one and the timeout may become configurable.
pub(crate) fn remote_expr(_cfg: &Config, socket: &Path, expr: &str) -> Option<String> {
    crate::msgpack::eval(socket, expr, REMOTE_EXPR_TIMEOUT)
}

/// Bound on a single daemon round trip. Generous: an expression that runs
/// `:wall` over many buffers legitimately takes a while, and the old
/// spawn-based path allowed five seconds.
const REMOTE_EXPR_TIMEOUT: Duration = Duration::from_secs(5);

pub fn remote_execute(cfg: &Config, socket: &Path, commands: &[String]) -> Option<String> {
    let list: Vec<String> = commands.iter().map(|c| viml_string(c)).collect();
    remote_expr(cfg, socket, &format!("execute([{}])", list.join(",")))
}

/// A VimL single-quoted string literal.
pub fn viml_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn modified_count(cfg: &Config, socket: &Path) -> Option<usize> {
    remote_expr(
        cfg,
        socket,
        "len(filter(getbufinfo({'buflisted':1}),'v:val.changed'))",
    )
    .and_then(|s| s.trim().parse().ok())
}

fn wait_dead(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        sleep(Duration::from_millis(50));
    }
    !pid_alive(pid)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    Gone,
    StillAlive,
    NotOurs,
}

/// Stop a daemon: save or report unsaved buffers, then escalate
/// `:qall` → `:qall!` → SIGTERM → SIGKILL. Signals are only ever sent to a
/// pid that `ps` proves is ours.
pub fn stop(cfg: &Config, herdr: &Herdr, record: &DaemonRecord, label: &str) -> StopOutcome {
    let grace = Duration::from_millis(cfg.grace_ms);
    let pid = record.pid;
    if !pid_alive(pid) {
        let _ = fs::remove_file(&record.socket);
        return StopOutcome::Gone;
    }
    let ours = pid_is_ours(record);
    if !ours && !socket_ok(&record.socket) {
        return StopOutcome::NotOurs;
    }

    if socket_ok(&record.socket) {
        if let Some(modified) = modified_count(cfg, &record.socket).filter(|n| *n > 0) {
            if cfg.save_on_close {
                remote_expr(cfg, &record.socket, "execute('silent! wall')");
                let remaining = modified_count(cfg, &record.socket).unwrap_or(0);
                let saved = modified.saturating_sub(remaining);
                let mut body =
                    format!("{label}: saved {saved} modified buffer(s) before stopping Neovim");
                if remaining > 0 {
                    body.push_str(&format!(
                        "; {remaining} could not be written and were discarded"
                    ));
                }
                herdr.notify("Neovim buffers saved", &body);
            } else {
                herdr.notify(
                    "Neovim buffers discarded",
                    &format!("{label}: {modified} unsaved buffer(s) discarded (HERDR_NVIM_SAVE_ON_CLOSE=0)"),
                );
            }
        }
        for expr in ["execute('qall')", "execute('qall!')"] {
            remote_expr(cfg, &record.socket, expr);
            if wait_dead(pid, grace) {
                break;
            }
        }
    }
    if ours && pid_alive(pid) {
        // SAFETY: pid verified via ps to be our nvim daemon.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if !wait_dead(pid, grace) {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            wait_dead(pid, Duration::from_secs(2));
        }
    }
    if pid_alive(pid) {
        if ours {
            StopOutcome::StillAlive
        } else {
            StopOutcome::NotOurs
        }
    } else {
        let _ = fs::remove_file(&record.socket);
        StopOutcome::Gone
    }
}

/// Stop the daemons of `tabs` and drop their records unless they survived.
fn stop_tabs(
    cfg: &Config,
    herdr: &Herdr,
    session: &str,
    tabs: &[String],
) -> Result<(Vec<String>, Vec<String>)> {
    let mut stopped = Vec::new();
    let mut survived = Vec::new();
    for tab in tabs {
        let record = {
            let file = StateFile::open()?;
            let state = file.load()?;
            state.get(session, tab).cloned()
        };
        let Some(record) = record else { continue };
        let outcome = stop(cfg, herdr, &record, tab);
        let file = StateFile::open()?;
        let mut state = file.load()?;
        match outcome {
            StopOutcome::Gone | StopOutcome::NotOurs => {
                state.remove(session, tab);
                stopped.push(tab.clone());
            }
            StopOutcome::StillAlive => survived.push(tab.clone()),
        }
        file.save(&state)?;
    }
    Ok((stopped, survived))
}

fn event_name(payload: &Value) -> String {
    // Envelopes carry `data.type` in snake_case; HERDR_PLUGIN_EVENT is dotted.
    let from_data = payload
        .pointer("/data/type")
        .and_then(Value::as_str)
        .map(|t| t.replacen('_', ".", 1));
    from_data.unwrap_or_else(|| env::var("HERDR_PLUGIN_EVENT").unwrap_or_default())
}

/// Events that report what an agent is doing, as `event_name` spells them.
/// Named rather than inlined into the match so the routing can be tested: a
/// name that does not match what Herdr sends would fall through to `gc()`,
/// which is silent, expensive, and wrong.
const AGENT_EVENTS: &[&str] = &["pane.agent_status_changed", "pane.agent_detected"];

/// Tell a tab's Neovim what its agent is doing.
///
/// Herdr's event payload names the pane and the workspace but not the tab, so
/// the tab has to be resolved before the daemon registry can be consulted.
/// Both lookups are skipped when this session has no daemons at all: agent
/// status flips constantly in tabs that have never opened a sidebar, and
/// those must cost nothing.
fn push_agent_state(
    cfg: &Config,
    herdr: &Herdr,
    session: &str,
    event: &str,
    data: &Value,
) -> Result<i32> {
    let Some(pane_id) = data.get("pane_id").and_then(Value::as_str) else {
        return Ok(0);
    };
    let file = StateFile::open()?;
    let state = file.load()?;
    if state.tabs_of(session).is_empty() {
        return Ok(0);
    }
    // A released agent reports where it ended up under a different key.
    let status = data
        .get("agent_status")
        .or_else(|| data.get("final_status"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let agent = data
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let Some(pane) = herdr.pane_get(pane_id).ok().flatten() else {
        return Ok(0);
    };
    let Some(tab_id) = pane.tab_id.as_deref() else {
        return Ok(0);
    };
    let Some(record) = state.get(session, tab_id) else {
        return Ok(0);
    };
    if record.sidebar_pane_id.is_none() {
        // The daemon is alive but hidden; nobody is looking at a statusline.
        return Ok(0);
    }
    let payload = serde_json::json!({
        "pane_id": pane_id,
        "agent": agent,
        "status": status,
        "event": event,
    });
    // One escaping layer: the dict crosses as a JSON string that the Lua side
    // decodes, rather than as a VimL dict literal built by hand.
    let expr = format!(
        "luaeval(\"require('herdr-nvim.agent').on_status(_A)\", {})",
        viml_string(&payload.to_string())
    );
    remote_expr(cfg, &record.socket, &expr);
    Ok(0)
}

/// `[[events]]` entrypoint: `tab.closed`, `workspace.closed`, `pane.closed`,
/// `pane.agent_status_changed`, `pane.agent_detected`.
pub fn handle_event() -> Result<i32> {
    let payload: Value = env::var("HERDR_PLUGIN_EVENT_JSON")
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null);
    let data = payload.get("data").cloned().unwrap_or(payload.clone());
    let event = event_name(&payload);

    let cfg = Config::load();
    let herdr = Herdr::from_env();
    let session = herdr.session_key();

    let targets: Vec<String> = match event.as_str() {
        "tab.closed" => data
            .get("tab_id")
            .and_then(Value::as_str)
            .map(|t| vec![t.to_string()])
            .unwrap_or_default(),
        "workspace.closed" => {
            let Some(ws) = data.get("workspace_id").and_then(Value::as_str) else {
                return Ok(0);
            };
            let prefix = format!("{ws}:");
            let file = StateFile::open()?;
            let state = file.load()?;
            state
                .tabs_of(&session)
                .into_iter()
                .filter(|tab| tab.starts_with(&prefix))
                .collect()
        }
        "pane.closed" => {
            // A sidebar closed through Herdr itself: forget the pane, keep the daemon.
            let Some(pane) = data.get("pane_id").and_then(Value::as_str) else {
                return Ok(0);
            };
            let file = StateFile::open()?;
            let mut state = file.load()?;
            let mut cleared = false;
            if let Some(tabs) = state.sessions.get_mut(&session) {
                for record in tabs.values_mut() {
                    if record.sidebar_pane_id.as_deref() == Some(pane) {
                        record.sidebar_pane_id = None;
                        record.sidebar_terminal_id = None;
                        if record
                            .layout
                            .as_ref()
                            .is_some_and(|l| l.phase == crate::state::LayoutPhase::Open)
                        {
                            record.layout = None;
                        }
                        cleared = true;
                    }
                }
            }
            if cleared {
                file.save(&state)?;
                println!("pane.closed: forgot sidebar pane {pane}");
            }
            return Ok(0);
        }
        // Agent lifecycle: push the new state into this tab's Neovim, if it
        // has one. Kept before the wildcard on purpose -- falling through to
        // `gc()` would run a full daemon sweep on every idle/working flip.
        event if AGENT_EVENTS.contains(&event) => {
            return push_agent_state(&cfg, &herdr, &session, event, &data);
        }
        _ => return gc(),
    };

    let (stopped, survived) = stop_tabs(&cfg, &herdr, &session, &targets)?;
    if !survived.is_empty() {
        herdr.notify(
            "herdr-nvim: daemon still running",
            &format!(
                "could not stop the Neovim daemon for {}",
                survived.join(", ")
            ),
        );
    }
    println!(
        "{event}: stopped {} daemon(s){}{}",
        stopped.len(),
        if stopped.is_empty() {
            String::new()
        } else {
            format!(" [{}]", stopped.join(", "))
        },
        if survived.is_empty() {
            String::new()
        } else {
            format!("; still alive: {}", survived.join(", "))
        },
    );
    Ok(0)
}

/// Stop daemons whose tab (or whole session) is gone; forget dead ones;
/// re-validate sidebar pane identities.
/// Nothing ever removed the per-tab logs, lock files, picker handoffs or
/// quarantined state files, so they accumulated for the life of the machine
/// -- including entries from naming schemes the plugin no longer uses.
///
/// Files belonging to a live record are always kept. Everything else goes once
/// it is old enough to be useless: a log is worth keeping for a while after a
/// crash, a handoff for minutes at most.
const HANDOFF_MAX_AGE: Duration = Duration::from_secs(60 * 60);
const LOG_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);
const CORRUPT_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Which of `entries` may be deleted: not named in `keep`, and older than
/// `max_age`. Split out from the filesystem so it can be tested directly.
fn sweepable<'a>(
    entries: &'a [(PathBuf, Duration)],
    keep: &HashSet<String>,
    max_age: Duration,
) -> Vec<&'a PathBuf> {
    entries
        .iter()
        .filter(|(path, age)| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            !keep.contains(&name) && *age > max_age
        })
        .map(|(path, _)| path)
        .collect()
}

/// Age of each file directly inside `dir`, newest-first ordering irrelevant.
fn entries_with_age(dir: &Path, now: SystemTime) -> Vec<(PathBuf, Duration)> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    read.flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            // A clock that went backwards yields zero, i.e. "brand new": never
            // delete something we cannot age.
            Some((path, now.duration_since(modified).unwrap_or_default()))
        })
        .collect()
}

/// Remove state files that no live record needs any more. Returns how many.
fn sweep_state_dir(state: &State) -> usize {
    let dir = state_dir();
    let now = SystemTime::now();

    // Names still in use, so an active tab never loses its log or lock.
    let mut keep_logs = HashSet::new();
    let mut keep_locks = HashSet::new();
    for (session, tabs) in &state.sessions {
        for tab in tabs.keys() {
            let stem = format!("{}-{}", short_hash(session), sanitize(tab));
            keep_logs.insert(format!("{stem}.log"));
            keep_locks.insert(format!("{stem}.lock"));
        }
    }

    let mut removed = 0;
    for (sub, keep, max_age) in [
        ("logs", &keep_logs, LOG_MAX_AGE),
        ("locks", &keep_locks, LOG_MAX_AGE),
        ("handoff", &HashSet::new(), HANDOFF_MAX_AGE),
    ] {
        let path = dir.join(sub);
        for stale in sweepable(&entries_with_age(&path, now), keep, max_age) {
            if fs::remove_file(stale).is_ok() {
                removed += 1;
            }
        }
    }

    // Quarantined registries from a corrupt daemons.json.
    let corrupt: Vec<(PathBuf, Duration)> = entries_with_age(&dir, now)
        .into_iter()
        .filter(|(p, _)| {
            p.file_name()
                .map(|n| n.to_string_lossy().contains("json.corrupt-"))
                .unwrap_or(false)
        })
        .collect();
    for stale in sweepable(&corrupt, &HashSet::new(), CORRUPT_MAX_AGE) {
        if fs::remove_file(stale).is_ok() {
            removed += 1;
        }
    }
    removed
}

pub fn gc() -> Result<i32> {
    let cfg = Config::load();
    let herdr = Herdr::from_env();
    let session = herdr.session_key();
    let live_tabs: Option<HashSet<String>> = herdr.tab_ids().ok().map(|t| t.into_iter().collect());
    let running_sessions: Option<HashSet<String>> = herdr
        .running_session_sockets()
        .map(|s| s.into_iter().collect());

    // Finish any interrupted sidebar maneuver before touching records.
    match crate::sidebar::recover_all(&herdr, &session) {
        Ok(recovered) if !recovered.tabs.is_empty() => {
            println!("gc: restored the layout of {}", recovered.tabs.join(", "));
        }
        Ok(_) => {}
        Err(err) => eprintln!("warning: layout recovery failed: {err:#}"),
    }

    // Pass 1 (locked): classify.
    let mut to_stop: Vec<(String, String)> = Vec::new();
    let mut forgotten = Vec::new();
    let mut reconciled = 0;
    {
        let file = StateFile::open()?;
        let mut state = file.load()?;
        // One `ps` for the whole registry instead of one per record.
        let ps = ps_snapshot(state.sessions.values().flat_map(|tabs| tabs.values()));
        for (key, tabs) in state.sessions.iter_mut() {
            let mut drop_tabs = Vec::new();
            for (tab, record) in tabs.iter_mut() {
                if record.starting {
                    if record.age_secs() >= STARTING_STALE_SECS {
                        drop_tabs.push(tab.clone());
                    }
                    continue;
                }
                if !is_running_with(record, ps.get(&record.pid).map(String::as_str)) {
                    let _ = fs::remove_file(&record.socket);
                    forgotten.push(tab.clone());
                    drop_tabs.push(tab.clone());
                    continue;
                }
                if key == &session {
                    if let Some(live) = &live_tabs {
                        if !live.contains(tab) {
                            to_stop.push((key.clone(), tab.clone()));
                            continue;
                        }
                    }
                    if crate::sidebar::reconcile_sidebar(&herdr, record) {
                        reconciled += 1;
                    }
                } else if let Some(running) = &running_sessions {
                    if !running.contains(key) && !Path::new(key).exists() {
                        to_stop.push((key.clone(), tab.clone()));
                    }
                }
            }
            for tab in drop_tabs {
                tabs.remove(&tab);
            }
        }
        state.prune();
        file.save(&state)?;
    }

    // Pass 2 (unlocked per daemon): stop.
    let mut stopped = Vec::new();
    let mut survived = Vec::new();
    for (key, tab) in to_stop {
        let (s, v) = stop_tabs(&cfg, &herdr, &key, std::slice::from_ref(&tab))?;
        stopped.extend(s);
        survived.extend(v);
    }
    if !survived.is_empty() {
        herdr.notify(
            "herdr-nvim: daemon still running",
            &format!(
                "could not stop the Neovim daemon for {}",
                survived.join(", ")
            ),
        );
    }

    // Registry is settled: drop state files no live record needs any more.
    let swept = match StateFile::open() {
        Ok(file) => file
            .load()
            .map(|state| sweep_state_dir(&state))
            .unwrap_or(0),
        Err(_) => 0,
    };

    println!(
        "gc: stopped {} daemon(s){}, forgot {} dead record(s){}, reconciled {} sidebar id(s){}{}{}",
        stopped.len(),
        if stopped.is_empty() {
            String::new()
        } else {
            format!(" [{}]", stopped.join(", "))
        },
        forgotten.len(),
        if forgotten.is_empty() {
            String::new()
        } else {
            format!(" [{}]", forgotten.join(", "))
        },
        reconciled,
        if survived.is_empty() {
            String::new()
        } else {
            format!("; still alive: {}", survived.join(", "))
        },
        if live_tabs.is_none() {
            " (Herdr unreachable: skipped tab check)"
        } else {
            ""
        },
        if swept == 0 {
            String::new()
        } else {
            format!(", swept {swept} stale state file(s)")
        },
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweepable_keeps_live_names_and_anything_recent() {
        let old = Duration::from_secs(30 * 24 * 60 * 60);
        let new = Duration::from_secs(60);
        let entries = vec![
            (PathBuf::from("/s/logs/live.log"), old),
            (PathBuf::from("/s/logs/dead.log"), old),
            (PathBuf::from("/s/logs/recent.log"), new),
        ];
        let keep: HashSet<String> = ["live.log".to_string()].into_iter().collect();
        let got = sweepable(&entries, &keep, LOG_MAX_AGE);
        assert_eq!(got, vec![&PathBuf::from("/s/logs/dead.log")]);

        // Nothing at all when every file is young.
        let young: Vec<(PathBuf, Duration)> =
            entries.iter().map(|(p, _)| (p.clone(), new)).collect();
        assert!(sweepable(&young, &HashSet::new(), LOG_MAX_AGE).is_empty());
    }

    #[test]
    fn sweep_removes_stale_files_but_spares_the_live_tab() {
        let dir = std::env::temp_dir().join(format!("herdr-nvim-sweep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for sub in ["logs", "locks", "handoff"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let _guard = crate::state::ENV_LOCK.lock().unwrap();
        let saved = std::env::var_os("HERDR_NVIM_STATE_DIR");
        let saved_plugin = std::env::var_os("HERDR_PLUGIN_STATE_DIR");
        std::env::set_var("HERDR_NVIM_STATE_DIR", &dir);
        std::env::remove_var("HERDR_PLUGIN_STATE_DIR");

        let mut state = State::default();
        state.insert(
            "sock",
            "w1:t1",
            DaemonRecord {
                pid: 1,
                socket: PathBuf::from("/tmp/x.sock"),
                cwd: PathBuf::from("/tmp"),
                sidebar_pane_id: None,
                sidebar_terminal_id: None,
                workspace_id: None,
                ps_lstart: None,
                starting: false,
                started_unix: 0,
                layout: None,
            },
        );
        let live_stem = format!("{}-{}", short_hash("sock"), sanitize("w1:t1"));

        let live_log = dir.join("logs").join(format!("{live_stem}.log"));
        let dead_log = dir.join("logs").join("deadbeef-w9-t9.log");
        let handoff = dir.join("handoff").join("old.json");
        for f in [&live_log, &dead_log, &handoff] {
            fs::write(f, b"x").unwrap();
        }
        // Backdate everything well past the thresholds.
        let long_ago = SystemTime::now() - Duration::from_secs(60 * 24 * 60 * 60);
        for f in [&live_log, &dead_log, &handoff] {
            let file = fs::File::options().write(true).open(f).unwrap();
            file.set_modified(long_ago).unwrap();
        }

        let removed = sweep_state_dir(&state);
        assert!(live_log.exists(), "the live tab's log was deleted");
        assert!(!dead_log.exists(), "a stale log survived");
        assert!(!handoff.exists(), "a stale handoff survived");
        assert_eq!(removed, 2);

        // Idempotent: a second sweep has nothing left to do.
        assert_eq!(sweep_state_dir(&state), 0);

        match saved {
            Some(v) => std::env::set_var("HERDR_NVIM_STATE_DIR", v),
            None => std::env::remove_var("HERDR_NVIM_STATE_DIR"),
        }
        if let Some(v) = saved_plugin {
            std::env::set_var("HERDR_PLUGIN_STATE_DIR", v);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ps_table_maps_each_pid_to_its_lstart_and_args() {
        // `ps -o pid=,lstart=,args=` output: leading pid, then the same
        // "<5 lstart tokens> <argv>" shape ps_matches() already expects.
        let text = concat!(
            "  501 Fri Aug 28 14:16:14 2026 nvim --headless --listen /tmp/a.sock\n",
            "67285 Fri Aug 28 14:19:11 2026 nvim --headless --listen /tmp/b.sock\n",
            "\n"
        );
        let table = parse_ps_table(text);
        assert_eq!(table.len(), 2);
        assert!(table[&501].starts_with("Fri Aug 28 14:16:14 2026 nvim"));
        assert!(table[&67285].ends_with("/tmp/b.sock"));
        // A batched line must still satisfy the existing matcher.
        assert!(ps_matches(
            &table[&67285],
            Path::new("/tmp/b.sock"),
            Some("Fri Aug 28 14:19:11 2026")
        ));
        assert!(!ps_matches(
            &table[&67285],
            Path::new("/tmp/other.sock"),
            None
        ));
    }

    #[test]
    fn ps_table_ignores_junk_rows() {
        let table = parse_ps_table("not-a-pid some args\n\n  \n123\n");
        assert!(table.is_empty(), "got {table:?}");
    }

    #[test]
    fn ps_lines_asks_once_for_many_pids_and_skips_pid_zero() {
        let me = std::process::id();
        let table = ps_lines(&[me, 0, me]);
        assert!(table.contains_key(&me), "own pid missing from {table:?}");
        assert!(!table.contains_key(&0));
        assert!(ps_lines(&[]).is_empty());
        assert!(ps_lines(&[0]).is_empty());
    }

    #[test]
    fn socket_paths_are_short_and_distinct_per_session() {
        let a = socket_path("/Users/me/.config/herdr/herdr.sock", "w1:t1");
        let b = socket_path("/Users/me/.config/herdr/sessions/work/herdr.sock", "w1:t1");
        assert_ne!(a, b);
        assert!(a.as_os_str().len() <= SOCKET_PATH_MAX);
        assert!(a.to_string_lossy().ends_with("-w1-t1.sock"));
    }

    #[test]
    fn dead_pid_is_not_alive() {
        assert!(!pid_alive(0));
        assert!(pid_alive(1));
    }

    #[test]
    fn ps_line_matching() {
        let sock = Path::new("/tmp/herdr-nvim-501/9c5d64-w1-t1.sock");
        let line = "Thu Aug 27 01:25:56 2026 nvim --headless --listen /tmp/herdr-nvim-501/9c5d64-w1-t1.sock";
        assert!(ps_matches(line, sock, None));
        assert!(ps_matches(line, sock, Some("Thu Aug 27 01:25:56 2026")));
        assert!(
            ps_matches(line, sock, Some("Thu Aug 27  01:25:56  2026")),
            "whitespace-insensitive"
        );
        assert!(
            !ps_matches(line, sock, Some("Thu Aug 27 01:25:57 2026")),
            "different start time"
        );
        assert!(
            !ps_matches(line, Path::new("/tmp/other.sock"), None),
            "other socket"
        );
        assert!(
            !ps_matches("Thu Aug 27 01:25:56 2026 nvim --embed", sock, None),
            "not a daemon"
        );
        assert!(!ps_matches("", sock, None));
        assert!(!ps_matches("Thu Aug 27", sock, None), "truncated");
    }

    #[test]
    fn stale_starting_records_are_not_running() {
        let rec = DaemonRecord {
            pid: 0,
            socket: PathBuf::from("/tmp/a.sock"),
            cwd: PathBuf::from("/tmp"),
            sidebar_pane_id: None,
            sidebar_terminal_id: None,
            workspace_id: None,
            ps_lstart: None,
            starting: true,
            started_unix: now_unix().saturating_sub(STARTING_STALE_SECS + 1),
            layout: None,
        };
        assert!(rec.age_secs() >= STARTING_STALE_SECS);
        assert!(!is_running(&rec));
    }

    #[test]
    fn plugin_root_is_found_from_the_binary_and_refused_elsewhere() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for profile in ["release", "debug"] {
            let exe = root.join("target").join(profile).join("herdr-nvim");
            assert_eq!(plugin_root_of(&exe).as_deref(), Some(root), "{profile}");
        }
        // A binary copied onto $PATH has no checkout above it, so the daemon
        // gets no bundled Lua rather than a wrong runtimepath entry.
        assert!(plugin_root_of(Path::new("/usr/local/bin/herdr-nvim")).is_none());
    }

    #[test]
    fn herdrs_reported_root_wins_over_the_path_shape_guess() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let stray = Path::new("/usr/local/bin/herdr-nvim");
        // Copied or symlinked out of the checkout, the path shape tells us
        // nothing -- but Herdr still knows where the plugin lives.
        assert_eq!(plugin_root_of(stray), None);
        assert_eq!(
            plugin_root_from(Some(root.as_os_str()), Some(stray)).as_deref(),
            Some(root)
        );
        // A root that does not carry the Lua is not trusted: taking it on faith
        // would silently disable the bundled fallback.
        assert_eq!(
            plugin_root_from(Some(OsStr::new("/nonexistent")), Some(stray)),
            None
        );
        // Empty or absent falls back to the guess.
        let exe = root.join("target").join("release").join("herdr-nvim");
        assert_eq!(
            plugin_root_from(Some(OsStr::new("")), Some(&exe)).as_deref(),
            Some(root)
        );
        assert_eq!(plugin_root_from(None, Some(&exe)).as_deref(), Some(root));
        assert!(plugin_root_of(Path::new("/herdr-nvim")).is_none());
        assert!(plugin_root_of(Path::new("herdr-nvim")).is_none());
    }

    #[test]
    fn bundled_lua_bootstrap_is_one_guarded_line() {
        // The literal relies on `\` line continuations; a stray newline would
        // make nvim treat the tail as a second -c command.
        assert!(!BUNDLED_LUA_BOOTSTRAP.contains('\n'));
        assert!(BUNDLED_LUA_BOOTSTRAP.starts_with("lua "));
        // The guard is the whole point: a user's own install must win.
        assert!(BUNDLED_LUA_BOOTSTRAP.contains("if not pcall(require, 'herdr-nvim') then"));
        assert!(BUNDLED_LUA_BOOTSTRAP.contains("HERDR_NVIM_PLUGIN_ROOT"));
        // No `|`, which vim would read as a command separator.
        assert!(!BUNDLED_LUA_BOOTSTRAP.contains('|'));
    }

    #[test]
    fn viml_strings_and_event_names() {
        assert_eq!(viml_string("it's"), "'it''s'");
        let payload = serde_json::json!({"event": "tab_closed", "data": {"type": "tab_closed", "tab_id": "w1:t1"}});
        assert_eq!(event_name(&payload), "tab.closed");
        let payload = serde_json::json!({"data": {"type": "pane_agent_status_changed"}});
        assert_eq!(event_name(&payload), "pane.agent_status_changed");
    }

    /// Verbatim payloads observed from Herdr 0.8.2. The names must land in
    /// `AGENT_EVENTS`; anything else falls through to `gc()`, so a rename on
    /// either side has to fail here rather than in production.
    #[test]
    fn real_agent_payloads_route_to_the_agent_arm() {
        let status = serde_json::json!({
            "event": "pane_agent_status_changed",
            "data": {"type": "pane_agent_status_changed", "pane_id": "wF:pR",
                     "workspace_id": "wF", "agent_status": "working", "agent": "claude"}
        });
        let detected = serde_json::json!({
            "event": "pane_agent_detected",
            "data": {"type": "pane_agent_detected", "pane_id": "wF:pQ",
                     "workspace_id": "wF", "agent": "claude",
                     "released": true, "final_status": "idle"}
        });
        for payload in [&status, &detected] {
            let name = event_name(payload);
            assert!(
                AGENT_EVENTS.contains(&name.as_str()),
                "{name} would fall through to gc()"
            );
        }
        // `pane.closed` must NOT be treated as an agent event: it has its own
        // arm that forgets the sidebar pane.
        assert!(!AGENT_EVENTS.contains(&"pane.closed"));
    }

    /// A released agent reports where it ended up as `final_status`, not
    /// `agent_status` -- reading only the latter would show a stale state.
    #[test]
    fn released_agent_status_falls_back_to_final_status() {
        let data = serde_json::json!({"released": true, "final_status": "idle"});
        let status = data
            .get("agent_status")
            .or_else(|| data.get("final_status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        assert_eq!(status, "idle");
    }
}
