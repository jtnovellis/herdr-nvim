//! Minimal Git context for prompts: repo root, name, branch, short sha.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct GitInfo {
    pub root: PathBuf,
    pub name: String,
    pub branch: String,
    pub short_sha: Option<String>,
}

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

pub fn info(cwd: &Path) -> Option<GitInfo> {
    let root = PathBuf::from(git(cwd, &["rev-parse", "--show-toplevel"])?);
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    let short_sha = git(cwd, &["rev-parse", "--short", "HEAD"]);
    let branch = match git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Some(b) if b != "HEAD" => b,
        _ => short_sha
            .as_deref()
            .map(|sha| format!("detached at {sha}"))
            .unwrap_or_else(|| "unborn".to_string()),
    };
    Some(GitInfo {
        root,
        name,
        branch,
        short_sha,
    })
}
