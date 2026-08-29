//! Deliver Neovim annotations to an agent running in the current workspace.

use crate::config::Config;
use crate::context::Context;
use crate::git::{self, GitInfo};
use crate::herdr::{self, AgentInfo, Herdr, HerdrError};
use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
pub struct Payload {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Comment {
    pub file: String,
    pub line: u32,
    #[serde(default)]
    pub end_line: Option<u32>,
    pub text: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub filetype: Option<String>,
    #[serde(default)]
    pub modified: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub pane_id: String,
    pub agent: String,
    pub name: Option<String>,
    pub label: String,
    pub tab_id: String,
    pub workspace_id: String,
    pub workspace_label: Option<String>,
    pub status: String,
    pub same_tab: bool,
    pub same_workspace: bool,
    pub in_repo: bool,
    pub cwd: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug)]
pub enum Resolution {
    Target(Box<Candidate>),
    NoAgents,
    Ambiguous(Vec<Candidate>),
    OtherWorkspaces(Vec<Candidate>),
    Unknown(String),
}

#[derive(Debug, Default)]
struct SendArgs {
    submit: bool,
    target: Option<String>,
    focus: bool,
    force: bool,
    file: Option<PathBuf>,
    dry_run: bool,
}

pub fn coded(code: &str, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(HerdrError {
        code: code.to_string(),
        message: message.into(),
    })
}

fn parse_send_args(args: &[String]) -> Result<SendArgs> {
    let mut out = SendArgs::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--submit" => out.submit = true,
            "--paste" => out.submit = false,
            "--focus" => out.focus = true,
            "--force" => out.force = true,
            "--dry-run" => out.dry_run = true,
            "--target" => {
                out.target = Some(
                    iter.next()
                        .cloned()
                        .context("--target needs a pane id or agent name")?,
                )
            }
            "--file" => out.file = Some(PathBuf::from(iter.next().context("--file needs a path")?)),
            other => bail!("unknown send option `{other}`"),
        }
    }
    Ok(out)
}

/// The JSON payload the Lua side hands us: `--file PATH`, or stdin when the
/// path is absent or `-`. Shared with `ask`.
pub fn read_stdin_or_file(file: Option<&Path>, what: &str) -> Result<String> {
    match file {
        Some(path) if path.as_os_str() != "-" => {
            fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))
        }
        _ => {
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .with_context(|| format!("cannot read {what} from stdin"))?;
            Ok(String::from_utf8_lossy(&buf).into_owned())
        }
    }
}

fn read_payload(opts: &SendArgs) -> Result<Payload> {
    let raw = read_stdin_or_file(opts.file.as_deref(), "annotations")?;
    if raw.trim().is_empty() {
        return Ok(Payload::default());
    }
    serde_json::from_str(&raw).context("annotations payload is not valid JSON")
}

pub fn error_json(err: &anyhow::Error) -> Value {
    match err.downcast_ref::<HerdrError>() {
        Some(e) => json!({ "ok": false, "code": e.code, "error": e.message }),
        None => json!({ "ok": false, "error": format!("{err:#}") }),
    }
}

/// `herdr-nvim send`: prints one JSON object describing the outcome.
pub fn send(args: &[String]) -> Result<i32> {
    let opts = parse_send_args(args)?;
    match send_inner(&opts) {
        Ok(value) => {
            println!("{value}");
            Ok(0)
        }
        Err(err) => {
            println!("{}", error_json(&err));
            Ok(1)
        }
    }
}

fn send_inner(opts: &SendArgs) -> Result<Value> {
    let payload = read_payload(opts)?;
    if payload.comments.is_empty() {
        return Err(coded("no_annotations", "no annotations to send"));
    }
    let cfg = Config::load();
    let herdr = Herdr::from_env();
    let ctx = Context::from_env();
    let cwd = payload
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| ctx.cwd.clone())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let git = git::info(&cwd);
    let repo_root = git.as_ref().map(|g| g.root.as_path());

    if opts.dry_run {
        let prompt = build_prompt(
            &payload.comments,
            git.as_ref(),
            &cwd,
            cfg.max_snippet_lines,
            None,
        );
        return Ok(json!({ "ok": true, "dry_run": true, "prompt": prompt }));
    }

    let agents = herdr.agents().map_err(|e| {
        coded(
            "herdr_unreachable",
            format!("cannot list agents (is Herdr running?): {e:#}"),
        )
    })?;
    let needs_labels = agents.iter().any(|a| {
        ctx.workspace_id
            .as_deref()
            .is_some_and(|ws| a.workspace_id != ws)
    });
    let labels = if needs_labels {
        herdr.workspace_labels().unwrap_or_default()
    } else {
        Vec::new()
    };
    let list = candidates(&agents, &ctx, repo_root, &labels);
    let mut target = match resolve_target(&list, opts.target.as_deref()) {
        Resolution::Target(candidate) => *candidate,
        Resolution::NoAgents => return Err(coded("no_agents", "no agent is running in Herdr")),
        Resolution::Ambiguous(list) => {
            return Ok(json!({
                "ok": false, "needs_pick": true, "reason": "ambiguous",
                "candidates": list, "count": payload.comments.len(),
            }));
        }
        Resolution::OtherWorkspaces(list) => {
            return Ok(json!({
                "ok": false, "needs_pick": true, "reason": "no_agent_in_workspace",
                "candidates": list, "count": payload.comments.len(),
            }));
        }
        Resolution::Unknown(name) => {
            return Err(coded(
                "unknown_target",
                format!("no agent matches `{name}`"),
            ))
        }
    };

    // Re-check right before delivery: state can change after `agent list`.
    match herdr.agent_get(&target.pane_id) {
        Ok(Some(fresh)) => {
            if let Some(status) = fresh.agent_status.clone() {
                target.status = status;
            }
            precheck(&target, fresh.launch_pending.unwrap_or(false), opts.force)?;
        }
        Ok(None) => {
            return Err(coded(
                "agent_not_found",
                format!("agent pane {} no longer exists", target.pane_id),
            ))
        }
        Err(err) => return Err(err.context("cannot check the agent's state")),
    }

    let absolute = paths_should_be_absolute(repo_root, &cwd, target.cwd.as_deref());
    let prompt = build_prompt(
        &payload.comments,
        git.as_ref(),
        &cwd,
        cfg.max_snippet_lines,
        absolute.then_some(target.cwd.as_deref().unwrap_or("?")),
    );
    let via = deliver(&herdr, &target, &prompt, opts.submit).map_err(|e| describe(e, &target))?;
    if opts.focus {
        let _ = herdr.agent_focus(&target.pane_id);
    }

    Ok(json!({
        "ok": true,
        "mode": if opts.submit { "submit" } else { "paste" },
        "via": via,
        "target": target,
        "count": payload.comments.len(),
    }))
}

/// Refuse to type into an agent that is waiting at a prompt or still starting.
pub fn precheck(target: &Candidate, launch_pending: bool, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    if target.status == "blocked" {
        return Err(coded(
            "agent_blocked",
            format!(
                "{} ({}) is blocked waiting for your input; answer it first, or force with the ! form of the command",
                target.agent, target.pane_id
            ),
        ));
    }
    if launch_pending {
        return Err(coded(
            "agent_not_ready",
            format!(
                "{} ({}) is still starting; try again in a moment",
                target.agent, target.pane_id
            ),
        ));
    }
    Ok(())
}

/// `herdr-nvim agents`: agents visible from here as JSON.
pub fn list_agents(_args: &[String]) -> Result<i32> {
    let herdr = Herdr::from_env();
    let ctx = Context::from_env();
    let cwd = ctx.cwd.clone().or_else(|| std::env::current_dir().ok());
    let git = cwd.as_deref().and_then(git::info);
    match herdr.agents() {
        Ok(agents) => {
            let labels = herdr.workspace_labels().unwrap_or_default();
            let list = candidates(
                &agents,
                &ctx,
                git.as_ref().map(|g| g.root.as_path()),
                &labels,
            );
            println!(
                "{}",
                json!({
                    "ok": true,
                    "workspace_id": ctx.workspace_id,
                    "tab_id": ctx.tab_id,
                    "candidates": list,
                })
            );
            Ok(0)
        }
        Err(err) => {
            println!("{}", error_json(&err));
            Ok(1)
        }
    }
}

pub fn candidates(
    agents: &[AgentInfo],
    ctx: &Context,
    repo_root: Option<&Path>,
    labels: &[(String, String)],
) -> Vec<Candidate> {
    let mut list: Vec<Candidate> = agents
        .iter()
        .filter(|a| ctx.pane_id.as_deref() != Some(a.pane_id.as_str()))
        .map(|a| to_candidate(a, ctx, repo_root, labels))
        .collect();
    list.sort_by(|a, b| {
        b.same_workspace
            .cmp(&a.same_workspace)
            .then_with(|| b.same_tab.cmp(&a.same_tab))
            .then_with(|| b.in_repo.cmp(&a.in_repo))
            .then_with(|| a.pane_id.cmp(&b.pane_id))
    });
    list
}

fn to_candidate(
    agent: &AgentInfo,
    ctx: &Context,
    repo_root: Option<&Path>,
    labels: &[(String, String)],
) -> Candidate {
    let kind = agent
        .display_agent
        .clone()
        .or_else(|| agent.agent.clone())
        .unwrap_or_else(|| "agent".to_string());
    let status = agent
        .agent_status
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let same_tab = ctx.tab_id.as_deref() == Some(agent.tab_id.as_str());
    let same_workspace = ctx
        .workspace_id
        .as_deref()
        .is_none_or(|ws| ws == agent.workspace_id);
    let cwd = agent.foreground_cwd.clone().or_else(|| agent.cwd.clone());
    let in_repo = match (repo_root, cwd.as_deref()) {
        (Some(root), Some(dir)) => Path::new(dir).starts_with(root),
        _ => false,
    };
    let workspace_label = labels
        .iter()
        .find(|(id, _)| *id == agent.workspace_id)
        .map(|(_, label)| label.clone())
        .filter(|l| !l.is_empty());
    let title = agent
        .title
        .clone()
        .or_else(|| agent.terminal_title_stripped.clone());

    let mut label = kind.clone();
    if let Some(name) = &agent.name {
        label.push_str(&format!(" \"{name}\""));
    }
    label.push_str(&format!(" · {status} · {}", agent.pane_id));
    if same_tab {
        label.push_str(" · this tab");
    } else if !same_workspace {
        match &workspace_label {
            Some(ws) => label.push_str(&format!(" · workspace {ws}")),
            None => label.push_str(&format!(" · workspace {}", agent.workspace_id)),
        }
    }
    if let Some(dir) = cwd.as_deref().and_then(|d| Path::new(d).file_name()) {
        label.push_str(&format!(" · {}", dir.to_string_lossy()));
    }
    if let Some(title) = &title {
        label.push_str(&format!(" · {title}"));
    }
    Candidate {
        pane_id: agent.pane_id.clone(),
        agent: kind,
        name: agent.name.clone(),
        label,
        tab_id: agent.tab_id.clone(),
        workspace_id: agent.workspace_id.clone(),
        workspace_label,
        status,
        same_tab,
        same_workspace,
        in_repo,
        cwd,
        title,
    }
}

/// Skip the picker when the target is obvious: the lone agent in the
/// workspace, or the single agent sharing this tab. Agents in other
/// workspaces are only offered through the picker.
pub fn resolve_target(candidates: &[Candidate], explicit: Option<&str>) -> Resolution {
    if let Some(wanted) = explicit {
        if let Some(found) = candidates
            .iter()
            .find(|c| c.pane_id == wanted || c.name.as_deref() == Some(wanted))
        {
            return Resolution::Target(Box::new(found.clone()));
        }
        let by_kind: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| c.agent.eq_ignore_ascii_case(wanted))
            .collect();
        if by_kind.len() == 1 {
            return Resolution::Target(Box::new(by_kind[0].clone()));
        }
        return Resolution::Unknown(wanted.to_string());
    }
    if candidates.is_empty() {
        return Resolution::NoAgents;
    }
    let in_workspace: Vec<&Candidate> = candidates.iter().filter(|c| c.same_workspace).collect();
    match in_workspace.len() {
        0 => Resolution::OtherWorkspaces(candidates.to_vec()),
        1 => Resolution::Target(Box::new(in_workspace[0].clone())),
        _ => {
            let same_tab: Vec<&Candidate> = in_workspace
                .iter()
                .copied()
                .filter(|c| c.same_tab)
                .collect();
            if same_tab.len() == 1 {
                Resolution::Target(Box::new(same_tab[0].clone()))
            } else {
                Resolution::Ambiguous(in_workspace.into_iter().cloned().collect())
            }
        }
    }
}

/// Use absolute paths when the agent does not work inside our repo (or,
/// without git, inside our directory), so `file:line` still resolves.
pub fn paths_should_be_absolute(
    repo_root: Option<&Path>,
    cwd: &Path,
    agent_cwd: Option<&str>,
) -> bool {
    let Some(agent_cwd) = agent_cwd else {
        return false;
    };
    let agent_cwd = Path::new(agent_cwd);
    match repo_root {
        Some(root) => !(agent_cwd.starts_with(root) || root.starts_with(agent_cwd)),
        None => !(agent_cwd.starts_with(cwd) || cwd.starts_with(agent_cwd)),
    }
}

pub fn deliver(
    herdr: &Herdr,
    target: &Candidate,
    prompt: &str,
    submit: bool,
) -> Result<&'static str> {
    let pane = target.pane_id.as_str();
    if !submit {
        herdr.pane_send_input(pane, Some(prompt), &[])?;
        return Ok("pane.send_input");
    }
    match herdr.agent_prompt(pane, prompt) {
        Ok(_) => Ok("agent.prompt"),
        Err(err) => match herdr::error_code(&err) {
            Some(
                "agent_not_ready"
                | "agent_not_found"
                | "invalid_target"
                | "agent_not_running"
                | "agent_pane_not_found",
            ) => {
                herdr.pane_send_input(pane, Some(prompt), &["enter"])?;
                Ok("pane.send_input+enter")
            }
            _ => Err(err),
        },
    }
}

pub fn describe(err: anyhow::Error, target: &Candidate) -> anyhow::Error {
    let who = format!("{} ({})", target.agent, target.pane_id);
    let mapped = match herdr::error_code(&err) {
        Some("agent_blocked") => Some(("agent_blocked", format!("{who} is blocked waiting for your input; answer it first, or force with the ! form of the command"))),
        Some("not_found" | "pane_not_found" | "agent_pane_not_found" | "agent_not_found") => {
            Some(("agent_not_found", format!("agent pane {} no longer exists", target.pane_id)))
        }
        Some("agent_not_running") => Some(("agent_not_running", format!("{who} is no longer running an agent"))),
        Some("agent_target_ambiguous") => Some(("agent_target_ambiguous", "several agents match that target; pick one explicitly".to_string())),
        Some("agent_prompt_stalled") => Some(("agent_prompt_stalled", format!("{who} accepted the prompt but showed no activity"))),
        Some("timeout") => Some(("timeout", format!("Herdr timed out delivering to {who}"))),
        Some("ui_busy") => Some(("ui_busy", "Herdr is busy with a modal; close it and retry".to_string())),
        _ => None,
    };
    match mapped {
        Some((code, message)) => coded(code, message),
        None => err.context(format!("delivery to {who} failed")),
    }
}

pub fn relative_path(file: &str, git: Option<&GitInfo>, cwd: &Path) -> String {
    let path = Path::new(file);
    if let Some(git) = git {
        if let Ok(rel) = path.strip_prefix(&git.root) {
            return rel.to_string_lossy().into_owned();
        }
    }
    if let Ok(rel) = path.strip_prefix(cwd) {
        return rel.to_string_lossy().into_owned();
    }
    file.to_string()
}

pub fn truncate_code(code: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = code.lines().collect();
    if lines.len() <= max_lines {
        return code.trim_end_matches('\n').to_string();
    }
    let omitted = lines.len() - max_lines;
    let mut out = lines[..max_lines].join("\n");
    out.push_str(&format!(
        "\n… ({omitted} more line{})",
        if omitted == 1 { "" } else { "s" }
    ));
    out
}

/// Build the prompt. `absolute_for` names the agent cwd when paths must be
/// absolute because the agent works outside our repo.
pub fn build_prompt(
    comments: &[Comment],
    git: Option<&GitInfo>,
    cwd: &Path,
    max_snippet_lines: usize,
    absolute_for: Option<&str>,
) -> String {
    let mut out = String::new();
    let n = comments.len();
    out.push_str(&format!(
        "Code annotations from Neovim ({n} comment{})",
        if n == 1 { "" } else { "s" }
    ));
    match git {
        Some(git) => {
            out.push_str(&format!(" — repo {}, branch {}", git.name, git.branch));
            if let Some(sha) = &git.short_sha {
                out.push_str(&format!(" @ {sha}"));
            }
            out.push_str(&format!("\nRoot: {}", git.root.display()));
        }
        None => out.push_str(&format!("\nDirectory: {}", cwd.display())),
    }
    match absolute_for {
        Some(agent_cwd) => out.push_str(&format!(
            " (paths below are absolute; your working directory is {agent_cwd})\n"
        )),
        None => out.push('\n'),
    }

    for (index, comment) in comments.iter().enumerate() {
        out.push('\n');
        let shown = if absolute_for.is_some() {
            comment.file.clone()
        } else {
            relative_path(&comment.file, git, cwd)
        };
        let mut location = format!("{shown}:{}", comment.line);
        if let Some(end) = comment.end_line.filter(|e| *e > comment.line) {
            location.push_str(&format!("-{end}"));
        }
        out.push_str(&format!("## {}. {location}", index + 1));
        if comment.modified == Some(true) {
            out.push_str(" (buffer has unsaved changes)");
        }
        out.push('\n');
        out.push_str(&format!("Comment: {}\n", comment.text.trim()));
        if let Some(code) = comment.code.as_deref().filter(|c| !c.trim().is_empty()) {
            let lang = comment.filetype.as_deref().unwrap_or("");
            out.push_str(&format!(
                "```{lang}\n{}\n```\n",
                truncate_code(code, max_snippet_lines)
            ));
        }
    }
    out.push_str("\nPlease address each annotation above; refer to them by number.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(pane: &str, tab: &str, ws: &str, kind: &str, name: Option<&str>) -> AgentInfo {
        AgentInfo {
            pane_id: pane.into(),
            workspace_id: ws.into(),
            tab_id: tab.into(),
            agent: Some(kind.into()),
            name: name.map(str::to_string),
            display_agent: None,
            agent_status: Some("idle".into()),
            title: None,
            terminal_title_stripped: None,
            cwd: Some("/repo".into()),
            foreground_cwd: None,
            launch_pending: None,
        }
    }

    fn ctx(ws: &str, tab: &str) -> Context {
        Context {
            workspace_id: Some(ws.into()),
            tab_id: Some(tab.into()),
            pane_id: None,
            cwd: None,
            selected_text: None,
            clicked_url: None,
            invocation_source: None,
        }
    }

    fn cands(agents: &[AgentInfo], c: &Context) -> Vec<Candidate> {
        candidates(
            agents,
            c,
            Some(Path::new("/repo")),
            &[("w2".into(), "other".into())],
        )
    }

    #[test]
    fn lone_agent_in_workspace_is_picked() {
        let agents = vec![
            agent("w1:p2", "w1:t1", "w1", "claude", None),
            agent("w2:p2", "w2:t1", "w2", "codex", None),
        ];
        let list = cands(&agents, &ctx("w1", "w1:t9"));
        assert_eq!(list.len(), 2, "other workspaces stay listed");
        match resolve_target(&list, None) {
            Resolution::Target(c) => assert_eq!(c.pane_id, "w1:p2"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn single_sibling_wins_over_other_tabs() {
        let agents = vec![
            agent("w1:p2", "w1:t1", "w1", "claude", None),
            agent("w1:p5", "w1:t2", "w1", "codex", None),
        ];
        let list = cands(&agents, &ctx("w1", "w1:t2"));
        assert!(list[0].same_tab, "same-tab agents sort first");
        match resolve_target(&list, None) {
            Resolution::Target(c) => assert_eq!(c.pane_id, "w1:p5"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn two_plausible_agents_need_a_pick() {
        let agents = vec![
            agent("w1:p2", "w1:t1", "w1", "claude", None),
            agent("w1:p3", "w1:t1", "w1", "codex", None),
            agent("w2:p3", "w2:t1", "w2", "codex", None),
        ];
        let list = cands(&agents, &ctx("w1", "w1:t1"));
        match resolve_target(&list, None) {
            Resolution::Ambiguous(l) => assert_eq!(l.len(), 2, "only this workspace is offered"),
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(resolve_target(&[], None), Resolution::NoAgents));
    }

    #[test]
    fn other_workspace_agents_are_offered_not_auto_picked() {
        let agents = vec![agent("w2:p2", "w2:t1", "w2", "claude", None)];
        let list = cands(&agents, &ctx("w1", "w1:t1"));
        assert!(!list[0].same_workspace);
        assert!(
            list[0].label.contains("workspace other"),
            "{}",
            list[0].label
        );
        match resolve_target(&list, None) {
            Resolution::OtherWorkspaces(l) => assert_eq!(l.len(), 1),
            other => panic!("unexpected {other:?}"),
        }
        // Explicit targets may cross workspaces.
        assert!(matches!(
            resolve_target(&list, Some("w2:p2")),
            Resolution::Target(_)
        ));
    }

    #[test]
    fn outside_herdr_every_agent_is_in_scope() {
        let agents = vec![agent("w2:p2", "w2:t1", "w2", "claude", None)];
        let list = cands(&agents, &Context::default());
        assert!(list[0].same_workspace);
        assert!(matches!(resolve_target(&list, None), Resolution::Target(_)));
    }

    #[test]
    fn explicit_target_by_pane_name_or_kind() {
        let agents = vec![
            agent("w1:p2", "w1:t1", "w1", "claude", Some("reviewer")),
            agent("w1:p3", "w1:t1", "w1", "codex", None),
        ];
        let list = cands(&agents, &ctx("w1", "w1:t1"));
        assert!(
            matches!(resolve_target(&list, Some("w1:p3")), Resolution::Target(c) if c.pane_id == "w1:p3")
        );
        assert!(
            matches!(resolve_target(&list, Some("reviewer")), Resolution::Target(c) if c.pane_id == "w1:p2")
        );
        assert!(
            matches!(resolve_target(&list, Some("Codex")), Resolution::Target(c) if c.pane_id == "w1:p3")
        );
        assert!(matches!(
            resolve_target(&list, Some("gemini")),
            Resolution::Unknown(_)
        ));
    }

    #[test]
    fn own_pane_is_never_a_candidate() {
        let agents = vec![agent("w1:p2", "w1:t1", "w1", "claude", None)];
        let mut context = ctx("w1", "w1:t1");
        context.pane_id = Some("w1:p2".into());
        assert!(cands(&agents, &context).is_empty());
    }

    #[test]
    fn blocked_is_refused_without_force() {
        let agents = vec![agent("w1:p2", "w1:t1", "w1", "claude", None)];
        let mut c = cands(&agents, &ctx("w1", "w1:t1")).remove(0);
        assert!(precheck(&c, false, false).is_ok());
        c.status = "blocked".into();
        let err = precheck(&c, false, false).unwrap_err();
        assert_eq!(herdr::error_code(&err), Some("agent_blocked"));
        assert!(precheck(&c, false, true).is_ok(), "--force overrides");
        c.status = "idle".into();
        let err = precheck(&c, true, false).unwrap_err();
        assert_eq!(herdr::error_code(&err), Some("agent_not_ready"));
        let json = error_json(&err);
        assert_eq!(json["code"], "agent_not_ready");
    }

    #[test]
    fn absolute_paths_when_agent_is_outside_the_repo() {
        let root = Path::new("/repo");
        assert!(!paths_should_be_absolute(
            Some(root),
            root,
            Some("/repo/sub")
        ));
        assert!(
            !paths_should_be_absolute(Some(root), root, Some("/")),
            "agent above the repo can still resolve"
        );
        assert!(paths_should_be_absolute(
            Some(root),
            root,
            Some("/other/worktree")
        ));
        assert!(!paths_should_be_absolute(
            None,
            Path::new("/proj"),
            Some("/proj")
        ));
        assert!(paths_should_be_absolute(
            None,
            Path::new("/proj"),
            Some("/elsewhere")
        ));
        assert!(!paths_should_be_absolute(Some(root), root, None));
    }

    #[test]
    fn prompt_includes_location_git_and_code() {
        let git = GitInfo {
            root: PathBuf::from("/repo"),
            name: "repo".into(),
            branch: "main".into(),
            short_sha: Some("abc123".into()),
        };
        let comments = vec![
            Comment {
                file: "/repo/src/main.rs".into(),
                line: 4,
                end_line: Some(6),
                text: "handle the error".into(),
                code: Some("a\nb\nc".into()),
                filetype: Some("rust".into()),
                modified: Some(true),
            },
            Comment {
                file: "/elsewhere/x.lua".into(),
                line: 1,
                end_line: Some(1),
                text: "rename".into(),
                code: None,
                filetype: None,
                modified: None,
            },
        ];
        let prompt = build_prompt(&comments, Some(&git), Path::new("/repo"), 2, None);
        assert!(prompt.starts_with("Code annotations from Neovim (2 comments) — repo repo, branch main @ abc123\nRoot: /repo\n"));
        assert!(prompt.contains("## 1. src/main.rs:4-6 (buffer has unsaved changes)\nComment: handle the error\n```rust\na\nb\n… (1 more line)\n```"));
        assert!(prompt.contains("## 2. /elsewhere/x.lua:1\nComment: rename\n"));
        assert!(!prompt.contains("x.lua:1-1"));

        let absolute = build_prompt(
            &comments,
            Some(&git),
            Path::new("/repo"),
            80,
            Some("/other"),
        );
        assert!(absolute.contains(
            "Root: /repo (paths below are absolute; your working directory is /other)\n"
        ));
        assert!(absolute.contains("## 1. /repo/src/main.rs:4-6"));
    }

    #[test]
    fn prompt_without_git_uses_directory() {
        let comments = vec![Comment {
            file: "/tmp/proj/a.py".into(),
            line: 10,
            end_line: None,
            text: "why?".into(),
            code: Some("x = 1\n".into()),
            filetype: Some("python".into()),
            modified: Some(false),
        }];
        let prompt = build_prompt(&comments, None, Path::new("/tmp/proj"), 80, None);
        assert!(prompt.contains("(1 comment)\nDirectory: /tmp/proj\n"));
        assert!(prompt.contains("## 1. a.py:10\nComment: why?\n```python\nx = 1\n```"));
    }

    #[test]
    fn send_args_parse() {
        let args: Vec<String> = ["--submit", "--target", "w1:p2", "--focus", "--force"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = parse_send_args(&args).unwrap();
        assert!(parsed.submit && parsed.focus && parsed.force);
        assert_eq!(parsed.target.as_deref(), Some("w1:p2"));
        assert!(parse_send_args(&["--bogus".to_string()]).is_err());
    }
}
