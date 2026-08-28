//! Headless Neovim daemons: one per Herdr tab. They are started detached
//! (`setsid`) so they outlive the plugin command that spawned them and the
//! sidebar clients that attach to them.

use crate::config::Config;
use crate::herdr::Herdr;
use crate::state::{now_unix, state_dir, DaemonRecord, StateFile};
use anyhow::{bail, Context as _, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

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
    let output = Command::new("ps")
        .args(["-o", "lstart=,args=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
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
    if !pid_alive(record.pid) {
        return false;
    }
    ps_line(record.pid)
        .map(|line| ps_matches(&line, &record.socket, record.ps_lstart.as_deref()))
        .unwrap_or(false)
}

pub fn socket_ok(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

pub fn is_running(record: &DaemonRecord) -> bool {
    !record.starting && pid_is_ours(record) && socket_ok(&record.socket)
}

fn log_path(session_key: &str, tab_id: &str) -> PathBuf {
    state_dir().join("logs").join(format!(
        "{}-{}.log",
        short_hash(session_key),
        sanitize(tab_id)
    ))
}

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

/// Evaluate an expression in the daemon and return its printed result.
pub(crate) fn remote_expr(cfg: &Config, socket: &Path, expr: &str) -> Option<String> {
    let mut child = Command::new(&cfg.nvim)
        .arg("--server")
        .arg(socket)
        .args(["--remote-expr", expr])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut out);
                }
                return status.success().then_some(out.trim().to_string());
            }
            Ok(None) if Instant::now() < deadline => sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
}

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

/// `[[events]]` entrypoint: `tab.closed`, `workspace.closed`, `pane.closed`.
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
        for (key, tabs) in state.sessions.iter_mut() {
            let mut drop_tabs = Vec::new();
            for (tab, record) in tabs.iter_mut() {
                if record.starting {
                    if record.age_secs() >= STARTING_STALE_SECS {
                        drop_tabs.push(tab.clone());
                    }
                    continue;
                }
                if !is_running(record) {
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

    println!(
        "gc: stopped {} daemon(s){}, forgot {} dead record(s){}, reconciled {} sidebar id(s){}{}",
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
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn viml_strings_and_event_names() {
        assert_eq!(viml_string("it's"), "'it''s'");
        let payload = serde_json::json!({"event": "tab_closed", "data": {"type": "tab_closed", "tab_id": "w1:t1"}});
        assert_eq!(event_name(&payload), "tab.closed");
        let payload = serde_json::json!({"data": {"type": "pane_agent_status_changed"}});
        assert_eq!(event_name(&payload), "pane.agent_status_changed");
    }
}
