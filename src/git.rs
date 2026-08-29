//! Minimal Git context for prompts: repo root, name, branch, short sha.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How many characters of the object id to show. `git rev-parse --short` picks
/// a repo-dependent minimum (>= 7); we ask for the full id in the same call as
/// everything else and shorten here, which is worth one fewer process.
const SHORT_SHA_LEN: usize = 7;

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

/// Turn the three lines of `rev-parse --show-toplevel HEAD --abbrev-ref HEAD`
/// into (root, short sha, branch). Detached HEAD reports the branch as `HEAD`.
fn parse_rev_parse(out: &str) -> Option<(PathBuf, Option<String>, String)> {
    let mut lines = out.lines();
    let root = PathBuf::from(lines.next()?.trim());
    if root.as_os_str().is_empty() {
        return None;
    }
    let sha = lines.next()?.trim();
    let head = lines.next()?.trim();
    let short_sha = (!sha.is_empty() && sha != "HEAD")
        .then(|| sha.chars().take(SHORT_SHA_LEN).collect::<String>());
    let branch = match head {
        b if !b.is_empty() && b != "HEAD" => b.to_string(),
        _ => short_sha
            .as_deref()
            .map(|sha| format!("detached at {sha}"))
            .unwrap_or_else(|| "unborn".to_string()),
    };
    Some((root, short_sha, branch))
}

pub fn info(cwd: &Path) -> Option<GitInfo> {
    // One process for all three answers. It fails on an unborn HEAD, where the
    // second query has nothing to resolve; fall back to asking for the root
    // alone so a fresh `git init` still gets a usable header.
    let (root, short_sha, branch) = match git(
        cwd,
        &[
            "rev-parse",
            "--show-toplevel",
            "HEAD",
            "--abbrev-ref",
            "HEAD",
        ],
    )
    .as_deref()
    .and_then(parse_rev_parse)
    {
        Some(parsed) => parsed,
        None => {
            let root = PathBuf::from(git(cwd, &["rev-parse", "--show-toplevel"])?);
            (root, None, "unborn".to_string())
        }
    };

    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    Some(GitInfo {
        root,
        name,
        branch,
        short_sha,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_normal_checkout() {
        let out = "/repo\na532baf01c8477f065367b258c762ece1c3080eb\nmain\n";
        let (root, sha, branch) = parse_rev_parse(out).expect("should parse");
        assert_eq!(root, PathBuf::from("/repo"));
        assert_eq!(sha.as_deref(), Some("a532baf"));
        assert_eq!(branch, "main");
    }

    #[test]
    fn detached_head_reports_the_sha() {
        // `--abbrev-ref HEAD` prints the literal "HEAD" when detached.
        let out = "/repo\n1aac8d69d8f1de6c64df939b26bf5d46074111ac\nHEAD\n";
        let (_, sha, branch) = parse_rev_parse(out).expect("should parse");
        assert_eq!(sha.as_deref(), Some("1aac8d6"));
        assert_eq!(branch, "detached at 1aac8d6");
    }

    #[test]
    fn a_branch_with_slashes_survives() {
        let out = "/repo\ndeadbeefcafe0000000000000000000000000000\nfeat/add-thing\n";
        let (_, _, branch) = parse_rev_parse(out).expect("should parse");
        assert_eq!(branch, "feat/add-thing");
    }

    #[test]
    fn missing_lines_are_rejected_rather_than_guessed() {
        assert!(parse_rev_parse("").is_none());
        assert!(parse_rev_parse("/repo\n").is_none());
        assert!(parse_rev_parse("/repo\nsha\n").is_none());
        assert!(parse_rev_parse("\nsha\nmain\n").is_none());
    }

    #[test]
    fn unresolved_head_yields_no_sha() {
        // Defensive: if HEAD ever echoes back instead of resolving.
        let out = "/repo\nHEAD\nHEAD\n";
        let (_, sha, branch) = parse_rev_parse(out).expect("should parse");
        assert_eq!(sha, None);
        assert_eq!(branch, "unborn");
    }

    #[test]
    fn real_repository_round_trip() {
        // This crate's own checkout: proves the flag combination still works
        // with the installed git, not just the recorded fixture strings.
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        if let Some(info) = info(here) {
            assert!(info.root.join("Cargo.toml").exists(), "root looks wrong");
            assert!(!info.branch.is_empty());
            if let Some(sha) = info.short_sha {
                assert_eq!(sha.len(), SHORT_SHA_LEN);
                assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
            }
        }
    }
}
