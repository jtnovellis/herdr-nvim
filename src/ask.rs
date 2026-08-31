//! Ask an agent about a piece of code, straight from Neovim.
//!
//! `send` flushes a queue of annotations as one numbered review request. This
//! delivers a single message instead: the code first, the question last, the
//! way you would paste it into the agent's terminal yourself. A follow-up turn
//! carries no selection at all — the agent already has the earlier one.

use crate::config::Config;
use crate::context::Context;
use crate::git::{self, GitInfo};
use crate::herdr::{self, Herdr};
use crate::send::{
    candidates, coded, deliver, describe, error_json, paths_should_be_absolute, precheck,
    read_stdin_or_file, relative_path, resolve_target, truncate_code, Resolution,
};
use crate::sessions;
use anyhow::{bail, Context as _, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
pub struct AskPayload {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub message: String,
    /// Absent on a follow-up turn.
    #[serde(default)]
    pub selection: Option<Selection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Selection {
    pub file: String,
    pub line: u32,
    #[serde(default)]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub filetype: Option<String>,
    #[serde(default)]
    pub modified: Option<bool>,
}

#[derive(Debug, Default)]
struct AskArgs {
    target: Option<String>,
    focus: bool,
    force: bool,
    paste: bool,
    file: Option<PathBuf>,
    dry_run: bool,
}

fn parse_ask_args(args: &[String]) -> Result<AskArgs> {
    let mut out = AskArgs::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--paste" => out.paste = true,
            "--submit" => out.paste = false,
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
            other => bail!("unknown ask option `{other}`"),
        }
    }
    Ok(out)
}

fn read_payload(opts: &AskArgs) -> Result<AskPayload> {
    let raw = read_stdin_or_file(opts.file.as_deref(), "the message")?;
    if raw.trim().is_empty() {
        return Ok(AskPayload::default());
    }
    serde_json::from_str(&raw).context("ask payload is not valid JSON")
}

/// `herdr-nvim ask`: prints one JSON object describing the outcome.
pub fn ask(args: &[String]) -> Result<i32> {
    let opts = parse_ask_args(args)?;
    match ask_inner(&opts) {
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

/// The message, refused when it is blank: an empty prompt would just make the
/// agent start working on whatever it last saw.
fn checked_message(payload: &AskPayload) -> Result<&str> {
    let message = payload.message.trim();
    if message.is_empty() {
        return Err(coded("no_message", "nothing to ask: the message is empty"));
    }
    Ok(message)
}

fn ask_inner(opts: &AskArgs) -> Result<Value> {
    let payload = read_payload(opts)?;
    let message = checked_message(&payload)?;
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
    let selection = payload.selection.as_ref();

    if opts.dry_run {
        let prompt = build_ask_prompt(
            message,
            selection,
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
                "ok": false, "needs_pick": true, "reason": "ambiguous", "candidates": list,
            }));
        }
        Resolution::OtherWorkspaces(list) => {
            return Ok(json!({
                "ok": false, "needs_pick": true, "reason": "no_agent_in_workspace",
                "candidates": list,
            }));
        }
        Resolution::Unknown(name) => {
            return Err(coded(
                "unknown_target",
                format!("no agent matches `{name}`"),
            ))
        }
    };

    // Re-check right before delivery: state can change after `agent list`. A
    // remembered target that has since gone reports `agent_not_found`, which is
    // the Lua side's cue to forget it and resolve again.
    let session = match herdr.agent_get(&target.pane_id) {
        Ok(Some(fresh)) => {
            if let Some(status) = fresh.agent_status.clone() {
                target.status = status;
            }
            // Where the reply will land. Read *before* delivering: the offset
            // has to be the length as it was when we sent, or the tail would
            // skip the first lines of the answer.
            let marker = session_marker(&fresh, &cwd);
            precheck(&target, fresh.launch_pending.unwrap_or(false), opts.force)?;
            marker
        }
        Ok(None) => {
            return Err(coded(
                "agent_not_found",
                format!("agent pane {} no longer exists", target.pane_id),
            ))
        }
        Err(err) => return Err(err.context("cannot check the agent's state")),
    };

    let absolute = paths_should_be_absolute(repo_root, &cwd, target.cwd.as_deref());
    let prompt = build_ask_prompt(
        message,
        selection,
        git.as_ref(),
        &cwd,
        cfg.max_snippet_lines,
        absolute.then_some(target.cwd.as_deref().unwrap_or("?")),
    );
    let via = deliver(&herdr, &target, &prompt, !opts.paste).map_err(|e| describe(e, &target))?;
    if opts.focus {
        let _ = herdr.agent_focus(&target.pane_id);
    }

    Ok(json!({
        "ok": true,
        "mode": if opts.paste { "paste" } else { "submit" },
        "via": via,
        "target": target,
        "session": session,
    }))
}

/// `{path, offset, agent}` for the agent's transcript, or `null` when Herdr
/// tracks no session we can resolve (an agent kind with no parser, or one
/// reporting an id we cannot turn into a file). A null marker is not an
/// error: the caller simply gets no reply view, exactly as before.
fn session_marker(fresh: &herdr::AgentInfo, cwd: &Path) -> Value {
    let Some(session) = &fresh.agent_session else {
        return Value::Null;
    };
    let agent = session.agent.clone().unwrap_or_default();
    let agent_cwd = fresh.cwd.as_deref().map(Path::new);
    let mut cwds: Vec<&Path> = vec![cwd];
    if let Some(agent_cwd) = agent_cwd {
        if agent_cwd != cwd {
            cwds.push(agent_cwd);
        }
    }
    let existing = sessions::session_path_for(
        session.kind.as_deref(),
        session.value.as_deref(),
        &agent,
        &cwds,
    );
    // A brand-new agent writes its transcript when it receives its first
    // message -- this one. Naming the file before it exists is what lets the
    // very first ask still get a reply; the reader waits for it to appear.
    let path = match existing {
        Some(path) => path,
        None => match (session.kind.as_deref(), session.value.as_deref()) {
            (Some("id"), Some(id)) if agent == "claude" => {
                match sessions::claude_session_path_expected(id, cwd) {
                    Some(path) => path,
                    None => return Value::Null,
                }
            }
            _ => return Value::Null,
        },
    };
    json!({
        "path": path.to_string_lossy(),
        "offset": crate::tail::offset_of(&path),
        "agent": agent,
    })
}

/// One message for the agent. `absolute_for` names the agent cwd when the path
/// must be absolute because the agent works outside our repo.
pub fn build_ask_prompt(
    message: &str,
    selection: Option<&Selection>,
    git: Option<&GitInfo>,
    cwd: &Path,
    max_snippet_lines: usize,
    absolute_for: Option<&str>,
) -> String {
    let message = message.trim();
    // A follow-up turn: the agent already has the code from the previous one,
    // and a repeated header would only muddy it.
    let Some(selection) = selection else {
        return format!("{message}\n");
    };

    let shown = if absolute_for.is_some() {
        selection.file.clone()
    } else {
        relative_path(&selection.file, git, cwd)
    };
    let mut location = format!("{shown}:{}", selection.line);
    if let Some(end) = selection.end_line.filter(|e| *e > selection.line) {
        location.push_str(&format!("-{end}"));
    }

    let mut notes: Vec<String> = Vec::new();
    if let Some(git) = git {
        let mut note = format!("{}, {}", git.name, git.branch);
        if let Some(sha) = &git.short_sha {
            note.push_str(&format!(" @ {sha}"));
        }
        notes.push(note);
    }
    if selection.modified == Some(true) {
        notes.push("unsaved changes".to_string());
    }

    // "From Neovim" so the agent knows this is a person pointing at code, not
    // a tool result it should go re-read.
    let mut out = format!("From Neovim — {location}");
    if !notes.is_empty() {
        out.push_str(&format!(" ({})", notes.join(", ")));
    }
    if let Some(agent_cwd) = absolute_for {
        out.push_str(&format!(
            "\nThe path above is absolute; your working directory is {agent_cwd}."
        ));
    }
    out.push_str("\n\n");

    if let Some(code) = selection.code.as_deref().filter(|c| !c.trim().is_empty()) {
        let lang = selection.filetype.as_deref().unwrap_or("");
        out.push_str(&format!(
            "```{lang}\n{}\n```\n\n",
            truncate_code(code, max_snippet_lines)
        ));
    }
    out.push_str(message);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr;

    fn git() -> GitInfo {
        GitInfo {
            root: PathBuf::from("/repo"),
            name: "repo".into(),
            branch: "main".into(),
            short_sha: Some("abc123".into()),
        }
    }

    fn selection() -> Selection {
        Selection {
            file: "/repo/src/main.rs".into(),
            line: 4,
            end_line: Some(6),
            code: Some("a\nb\nc".into()),
            filetype: Some("rust".into()),
            modified: Some(false),
        }
    }

    #[test]
    fn prompt_leads_with_the_code_and_ends_with_the_question() {
        let prompt = build_ask_prompt(
            "  why does this swallow the error?  ",
            Some(&selection()),
            Some(&git()),
            Path::new("/repo"),
            80,
            None,
        );
        assert_eq!(
            prompt,
            "From Neovim — src/main.rs:4-6 (repo, main @ abc123)\n\n```rust\na\nb\nc\n```\n\nwhy does this swallow the error?\n"
        );
        // The batch framing must not leak into a single message.
        assert!(!prompt.contains("refer to them by number"));
        assert!(!prompt.contains("Code annotations"));
    }

    #[test]
    fn single_line_selection_has_no_range() {
        let mut sel = selection();
        sel.end_line = Some(4);
        sel.code = Some("a".into());
        let prompt = build_ask_prompt(
            "why?",
            Some(&sel),
            Some(&git()),
            Path::new("/repo"),
            80,
            None,
        );
        assert!(
            prompt.starts_with("From Neovim — src/main.rs:4 (repo, main @ abc123)\n"),
            "{prompt}"
        );
        assert!(!prompt.contains("4-4"));
        // `end_line` may also simply be absent.
        sel.end_line = None;
        let prompt = build_ask_prompt(
            "why?",
            Some(&sel),
            Some(&git()),
            Path::new("/repo"),
            80,
            None,
        );
        assert!(
            prompt.starts_with("From Neovim — src/main.rs:4 ("),
            "{prompt}"
        );
    }

    #[test]
    fn unsaved_buffer_is_flagged_beside_the_repo() {
        let mut sel = selection();
        sel.modified = Some(true);
        let prompt = build_ask_prompt("hm", Some(&sel), Some(&git()), Path::new("/repo"), 80, None);
        assert!(
            prompt.starts_with(
                "From Neovim — src/main.rs:4-6 (repo, main @ abc123, unsaved changes)\n"
            ),
            "{prompt}"
        );
        // Without git the note still shows, in its own parenthesis.
        let prompt = build_ask_prompt("hm", Some(&sel), None, Path::new("/repo"), 80, None);
        assert!(
            prompt.starts_with("From Neovim — src/main.rs:4-6 (unsaved changes)\n"),
            "{prompt}"
        );
    }

    #[test]
    fn a_follow_up_turn_is_just_the_message() {
        let prompt = build_ask_prompt(
            "  what about the fallback path?\n",
            None,
            Some(&git()),
            Path::new("/repo"),
            80,
            None,
        );
        assert_eq!(prompt, "what about the fallback path?\n");
    }

    #[test]
    fn absolute_path_when_the_agent_works_outside_the_repo() {
        let prompt = build_ask_prompt(
            "why?",
            Some(&selection()),
            Some(&git()),
            Path::new("/repo"),
            80,
            Some("/other"),
        );
        assert!(
            prompt.starts_with(
                "From Neovim — /repo/src/main.rs:4-6 (repo, main @ abc123)\nThe path above is absolute; your working directory is /other.\n"
            ),
            "{prompt}"
        );
        assert!(paths_should_be_absolute(
            Some(Path::new("/repo")),
            Path::new("/repo"),
            Some("/other")
        ));
    }

    #[test]
    fn long_snippets_are_truncated() {
        let mut sel = selection();
        sel.code = Some("1\n2\n3\n4\n5".into());
        let prompt = build_ask_prompt("why?", Some(&sel), None, Path::new("/repo"), 2, None);
        assert!(
            prompt.contains("```rust\n1\n2\n… (3 more lines)\n```"),
            "{prompt}"
        );
    }

    #[test]
    fn a_selection_without_code_still_names_the_location() {
        let mut sel = selection();
        sel.code = None;
        let prompt = build_ask_prompt("why?", Some(&sel), None, Path::new("/repo"), 80, None);
        assert_eq!(prompt, "From Neovim — src/main.rs:4-6\n\nwhy?\n");
    }

    #[test]
    fn a_blank_message_is_refused() {
        let payload = AskPayload {
            message: "   \n ".into(),
            ..Default::default()
        };
        let err = checked_message(&payload).unwrap_err();
        assert_eq!(herdr::error_code(&err), Some("no_message"));
        let payload = AskPayload {
            message: " hi ".into(),
            ..Default::default()
        };
        assert_eq!(checked_message(&payload).unwrap(), "hi");
    }

    #[test]
    fn payload_parses_with_and_without_a_selection() {
        let full: AskPayload = serde_json::from_str(
            r#"{"cwd":"/repo","message":"why?","selection":{"file":"/repo/a.rs","line":1,"end_line":2,"code":"x","filetype":"rust","modified":true}}"#,
        )
        .unwrap();
        let sel = full.selection.expect("selection");
        assert_eq!((sel.line, sel.end_line), (1, Some(2)));
        assert_eq!(sel.modified, Some(true));

        let follow_up: AskPayload = serde_json::from_str(r#"{"message":"and now?"}"#).unwrap();
        assert!(follow_up.selection.is_none());
        assert!(follow_up.cwd.is_none());
    }

    #[test]
    fn ask_args_parse() {
        let args: Vec<String> = ["--target", "w1:p2", "--focus", "--force", "--dry-run"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = parse_ask_args(&args).unwrap();
        assert_eq!(parsed.target.as_deref(), Some("w1:p2"));
        assert!(parsed.focus && parsed.force && parsed.dry_run);
        assert!(!parsed.paste, "ask submits by default");
        assert!(parse_ask_args(&["--paste".to_string()]).unwrap().paste);
        assert!(parse_ask_args(&["--bogus".to_string()]).is_err());
        assert!(parse_ask_args(&["--target".to_string()]).is_err());
    }
}
