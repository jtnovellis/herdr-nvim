//! `herdr-nvim pick-file`: the files the agent touched this session, newest
//! first, then the whole repo once you type — shown as a fuzzy picker inside
//! the tab's Neovim sidebar. Candidate gathering adapted from
//! ChmaraX/herdr-nvim (MIT); see THIRD_PARTY.md.

use crate::candidates::{self, BuildInput, Candidate};
use crate::config::Config;
use crate::daemon;
use crate::extract;
use crate::gitscan;
use crate::herdr::{AgentInfo, PaneInfo, PaneScroll};
use crate::send::{self, Resolution};
use crate::sessions;
use crate::sidebar::Host;
use crate::state::state_dir;
use anyhow::{bail, Context as _, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant, UNIX_EPOCH};

struct PickArgs {
    json: bool,
    target: Option<String>,
}

fn parse_args(args: &[String]) -> Result<PickArgs> {
    let mut out = PickArgs {
        json: false,
        target: None,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => out.json = true,
            "--target" => {
                out.target = Some(iter.next().cloned().context("--target needs a value")?)
            }
            other => bail!("unknown pick-file option `{other}`"),
        }
    }
    Ok(out)
}

/// Clamp the scrape depth to what Herdr serves cheaply.
pub fn effective_scan_lines(configured: u32, scroll: Option<PaneScroll>) -> u32 {
    scroll.map_or(configured, |s| configured.min(s.cheap_read_limit().max(1)))
}

enum Target {
    Pane(Box<send::Candidate>),
    NeedsPick(&'static str, Vec<send::Candidate>),
    None,
}

/// The agent whose files we list: the focused pane if it is an agent, else
/// the single agent sharing this tab, else the lone agent in the workspace.
fn choose_target(host: &Host, agents: &[AgentInfo], explicit: Option<&str>, json: bool) -> Target {
    if explicit.is_none() {
        if let Some(pane) = &host.ctx.pane_id {
            if let Some(agent) = agents.iter().find(|a| &a.pane_id == pane) {
                let list = send::candidates(
                    std::slice::from_ref(agent),
                    &crate::context::Context::default(),
                    None,
                    &[],
                );
                if let Some(c) = list.into_iter().next() {
                    return Target::Pane(Box::new(c));
                }
            }
        }
    }
    let list = send::candidates(agents, &host.ctx, None, &[]);
    match send::resolve_target(&list, explicit) {
        Resolution::Target(c) => Target::Pane(c),
        Resolution::Ambiguous(list) => {
            if json {
                Target::NeedsPick("ambiguous", list)
            } else {
                list.into_iter()
                    .next()
                    .map_or(Target::None, |c| Target::Pane(Box::new(c)))
            }
        }
        Resolution::OtherWorkspaces(list) => Target::NeedsPick("no_agent_in_workspace", list),
        Resolution::NoAgents | Resolution::Unknown(_) => Target::None,
    }
}

/// Session log text for the pane's agent, if Herdr tracks one.
fn session_text(info: &PaneInfo, cwd: &Path) -> (String, String) {
    let Some(session) = &info.agent_session else {
        return (String::new(), String::new());
    };
    let agent = session.agent.clone().unwrap_or_default();
    let path = match (session.kind.as_deref(), session.value.as_deref()) {
        (Some("path"), Some(value)) => Some(PathBuf::from(value)),
        (Some("id"), Some(id)) if agent == "claude" => {
            let cwds: Vec<&Path> = vec![cwd];
            sessions::claude_session_path(id, &cwds)
        }
        _ => None,
    };
    match path {
        Some(path) => (agent, sessions::read_session_text(&path)),
        None => (agent, String::new()),
    }
}

fn gather(agent: &str, session_log: &str, scrape_text: &str, cwd: &Path) -> Vec<Candidate> {
    let exists = |p: &Path| p.is_file();
    let exists_str = |p: &str| Path::new(p).is_file();
    let mined = sessions::mine_session(agent, session_log);
    let toplevel = gitscan::toplevel(cwd);
    // The four remaining git queries are independent read-only scans, and each
    // costs a process launch (~12-15 ms here), so run them at once: the picker
    // then waits for the slowest rather than the sum of all four.
    let (git_dirty, git_committed, diff_stats, repo_files) = match toplevel.as_deref() {
        None => (
            HashSet::new(),
            HashSet::new(),
            HashMap::new(),
            Vec::<String>::new(),
        ),
        Some(top) => std::thread::scope(|scope| {
            let dirty = scope.spawn(|| gitscan::dirty_paths(top).unwrap_or_default());
            let committed = scope.spawn(|| match mined.first_op_unix {
                Some(since) => gitscan::committed_since(top, since).unwrap_or_default(),
                None => HashSet::new(),
            });
            let diffs = scope.spawn(|| gitscan::diff_numstat_by_path(top).unwrap_or_default());
            let files = scope.spawn(|| gitscan::list_repo_files(top).unwrap_or_default());
            // A panicked scan degrades to "nothing found", matching what the
            // serial version did with `unwrap_or_default` on an Err.
            (
                dirty.join().unwrap_or_default(),
                committed.join().unwrap_or_default(),
                diffs.join().unwrap_or_default(),
                files.join().unwrap_or_default(),
            )
        }),
    };
    let in_git_worktree = |path: &str| {
        toplevel
            .as_deref()
            .map(|top| Path::new(path).starts_with(top))
            .unwrap_or(false)
    };
    let git_mtime_unix = |path: &str| {
        fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
    };
    let scraped = extract::extract(scrape_text, cwd, &exists);

    let out = candidates::build_candidates(BuildInput {
        mined_touches: &mined.touches,
        first_op_unix: mined.first_op_unix,
        git_dirty: &git_dirty,
        git_committed_in_session: &git_committed,
        in_git_worktree: &in_git_worktree,
        git_mtime_unix: &git_mtime_unix,
        diff_stats: &diff_stats,
        scraped_mentioned: &scraped,
        repo_files: &repo_files,
        exists: &exists_str,
    });
    let out = merge_duplicates(out);
    // Files inside the project first; out-of-tree touches (e.g. plan files)
    // stay reachable but never crowd out the repo.
    let tree = toplevel.clone().unwrap_or_else(|| cwd.to_path_buf());
    let tree = fs::canonicalize(&tree).unwrap_or(tree);
    let mut out = out;
    out.sort_by_key(|c| !Path::new(&c.path).starts_with(&tree));
    out
}

/// Collapse entries that name the same file through different spellings
/// (`/var/...` vs `/private/var/...` on macOS), keeping the richer one.
fn merge_duplicates(list: Vec<Candidate>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::with_capacity(list.len());
    let mut index: std::collections::HashMap<PathBuf, usize> = std::collections::HashMap::new();
    for mut cand in list {
        let canonical = fs::canonicalize(&cand.path).unwrap_or_else(|_| PathBuf::from(&cand.path));
        cand.path = canonical.to_string_lossy().into_owned();
        match index.get(&canonical) {
            Some(&i) => {
                let kept = &mut out[i];
                kept.session |= cand.session;
                kept.is_edit |= cand.is_edit;
                kept.newly_created |= cand.newly_created;
                if kept.line.is_none() {
                    kept.line = cand.line;
                }
                if kept.diff_stat.is_none() {
                    kept.diff_stat = cand.diff_stat;
                }
                kept.touched_unix = kept.touched_unix.max(cand.touched_unix);
            }
            None => {
                index.insert(canonical, out.len());
                out.push(cand);
            }
        }
    }
    // Re-establish newest-first after merging (stable: ties keep source order).
    out.sort_by(|a, b| {
        b.touched_unix
            .cmp(&a.touched_unix)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

fn write_handoff(handoff: &Value) -> Result<PathBuf> {
    let dir = state_dir().join("handoff");
    fs::create_dir_all(&dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("{}-{nanos}.json", std::process::id()));
    fs::write(&path, serde_json::to_vec(handoff)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

fn wait_for_ui(cfg: &Config, socket: &Path) -> bool {
    // Each probe is a socket round trip (~0.1 ms), so poll tightly: this
    // interval is pure added latency before the picker appears. It used to
    // spawn a whole Neovim per iteration, which is why it was 100 ms.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(n) = daemon::remote_expr(cfg, socket, "len(nvim_list_uis())") {
            if n.trim().parse::<u32>().unwrap_or(0) > 0 {
                return true;
            }
        }
        sleep(Duration::from_millis(5));
    }
    false
}

pub fn pick_file(args: &[String]) -> Result<i32> {
    let opts = parse_args(args)?;
    match pick_inner(&opts) {
        Ok(value) => {
            if opts.json {
                println!("{value}");
            } else if let Some(msg) = value.get("message").and_then(Value::as_str) {
                println!("{msg}");
            }
            Ok(0)
        }
        Err(err) => {
            if opts.json {
                println!("{}", json!({ "ok": false, "error": format!("{err:#}") }));
                Ok(1)
            } else {
                Err(err)
            }
        }
    }
}

fn pick_inner(opts: &PickArgs) -> Result<Value> {
    let host = Host::new()?;
    if !opts.json {
        host.prepare()?;
    }
    let agents = host.herdr.agents().context("cannot list agents")?;
    let target = match choose_target(&host, &agents, opts.target.as_deref(), opts.json) {
        Target::Pane(c) => *c,
        Target::NeedsPick(reason, list) => {
            if opts.json {
                return Ok(
                    json!({ "ok": false, "needs_pick": true, "reason": reason, "candidates": list }),
                );
            }
            bail!("no agent is running in this workspace");
        }
        Target::None => bail!("no agent is running in this workspace"),
    };

    let info = host
        .herdr
        .pane_get(&target.pane_id)?
        .with_context(|| format!("agent pane {} no longer exists", target.pane_id))?;
    let cwd = info
        .foreground_cwd
        .clone()
        .or_else(|| info.cwd.clone())
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| host.ctx.resolve_cwd(&host.herdr));
    let lines = effective_scan_lines(host.cfg.picker_scan_lines, info.scroll);
    let scrape = host
        .herdr
        .pane_read(&target.pane_id, "recent_unwrapped", lines)
        .unwrap_or_default();
    let (agent_kind, log) = session_text(&info, &cwd);
    let list = gather(&agent_kind, &log, &scrape, &cwd);
    if list.is_empty() {
        if !opts.json {
            host.herdr.notify(
                "herdr-nvim",
                "no files found in the agent's session or output",
            );
        }
        bail!("no files found in the agent's session or output");
    }
    let handoff = json!({
        "candidates": list,
        "cwd": cwd.to_string_lossy(),
        "max_files": host.cfg.picker_max_files,
        "title": format!("open file · {} {}", target.agent, target.pane_id),
    });
    if opts.json {
        return Ok(json!({
            "ok": true,
            "target": { "pane_id": target.pane_id, "agent": target.agent },
            "handoff": handoff,
        }));
    }

    let path = write_handoff(&handoff)?;
    let (sidebar, _) = host.ensure_open(true)?;
    let record = host
        .record()?
        .context("no daemon record after opening the sidebar")?;
    if !wait_for_ui(&host.cfg, &record.socket) {
        eprintln!(
            "warning: no UI attached to the daemon yet; the picker will show when it attaches"
        );
    }
    let expr = format!(
        "luaeval(\"require('herdr-nvim.picker').open_file(_A)\", {})",
        daemon::viml_string(&path.to_string_lossy())
    );
    if daemon::remote_expr(&host.cfg, &record.socket, &expr).is_none() {
        let _ = fs::remove_file(&path);
        bail!("the Neovim daemon did not open the picker");
    }
    Ok(
        json!({ "message": format!("picker with {} file(s) opened in sidebar {sidebar}", list.len()) }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// A small repo with a commit, one dirty file and one untracked file.
    fn fixture_repo(name: &str) -> Option<PathBuf> {
        let dir =
            std::env::temp_dir().join(format!("herdr-nvim-pick-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).ok()?;
        if !git(&dir, &["init", "-q"]) {
            return None; // no git available: the caller skips
        }
        for i in 1..=4 {
            fs::write(dir.join("src").join(format!("f{i}.rs")), "fn main() {}\n").ok()?;
        }
        fs::write(dir.join("README.md"), "hi\n").ok()?;
        git(&dir, &["add", "-A"]);
        git(
            &dir,
            &[
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "init",
            ],
        );
        fs::write(dir.join("src/f1.rs"), "fn main() { dirty() }\n").ok()?;
        fs::write(dir.join("src/untracked.rs"), "new\n").ok()?;
        Some(dir)
    }

    #[test]
    fn gather_lists_repo_files_and_marks_dirty_ones() {
        let Some(dir) = fixture_repo("gather") else {
            return;
        };
        let out = gather("claude", "", "", &dir);
        let paths: Vec<&str> = out.iter().map(|c| c.path.as_str()).collect();
        let has = |suffix: &str| paths.iter().any(|p| p.ends_with(suffix));

        assert!(has("src/f1.rs"), "dirty tracked file missing: {paths:?}");
        assert!(has("src/f4.rs"), "clean tracked file missing: {paths:?}");
        assert!(has("README.md"), "root file missing: {paths:?}");
        assert!(has("src/untracked.rs"), "untracked file missing: {paths:?}");
        assert!(
            !paths.iter().any(|p| p.contains("/.git/")),
            "walked into .git: {paths:?}"
        );

        // Every candidate must exist on disk and be inside the repo.
        for c in &out {
            assert!(Path::new(&c.path).is_file(), "phantom entry {}", c.path);
        }

        // The four scans now run on threads; the result must not depend on
        // which finishes first.
        let again = gather("claude", "", "", &dir);
        assert_eq!(
            out.iter().map(|c| &c.path).collect::<Vec<_>>(),
            again.iter().map(|c| &c.path).collect::<Vec<_>>(),
            "gather is not deterministic across runs"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gather_outside_a_repo_still_returns_scraped_paths() {
        let dir = std::env::temp_dir().join(format!("herdr-nvim-nogit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("noted.txt"), "x\n").unwrap();
        // No `git init`: every git scan returns empty and must not panic.
        let out = gather("claude", "", "see ./noted.txt for details", &dir);
        assert!(
            out.iter().any(|c| c.path.ends_with("noted.txt")),
            "scrape layer lost outside a repo: {:?}",
            out.iter().map(|c| &c.path).collect::<Vec<_>>()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_lines_clamp_to_cheap_reads() {
        let alt = PaneScroll {
            viewport_rows: 73,
            max_offset_from_bottom: 0,
        };
        assert_eq!(effective_scan_lines(300, Some(alt)), 73);
        let history = PaneScroll {
            viewport_rows: 34,
            max_offset_from_bottom: 3459,
        };
        assert_eq!(effective_scan_lines(300, Some(history)), 300);
        assert_eq!(effective_scan_lines(300, None), 300);
        assert_eq!(effective_scan_lines(50, Some(history)), 50);
    }

    #[test]
    fn duplicate_spellings_merge_into_one_entry() {
        let dir = std::env::temp_dir().join(format!("hn-pick-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("real")).unwrap();
        fs::write(dir.join("real/a.txt"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        let mk = |path: PathBuf, session: bool, line: Option<u32>| Candidate {
            path: path.to_string_lossy().into_owned(),
            line,
            is_edit: false,
            newly_created: false,
            session,
            touched_unix: None,
            diff_stat: None,
        };
        let merged = merge_duplicates(vec![
            mk(dir.join("real/a.txt"), false, None),
            mk(dir.join("link/a.txt"), true, Some(2)),
        ]);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert!(merged[0].session);
        assert_eq!(merged[0].line, Some(2));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pick_args_parse() {
        let args: Vec<String> = ["--json", "--target", "w1:p2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = parse_args(&args).unwrap();
        assert!(parsed.json);
        assert_eq!(parsed.target.as_deref(), Some("w1:p2"));
        assert!(parse_args(&["--nope".to_string()]).is_err());
    }
}
