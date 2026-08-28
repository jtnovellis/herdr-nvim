//! `herdr-nvim edit <file[:line[:col]]>`: open a file in this tab's sidebar
//! from any pane (or from a Herdr action via the invocation context).

use crate::context::Context;
use crate::daemon;
use crate::sidebar::Host;
use anyhow::{bail, Result};
use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: Option<u32>,
    pub col: Option<u32>,
}

fn number(s: &str) -> Option<u32> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Parse `path`, `path:12`, `path:12:3`, tolerating trailing colons.
pub fn parse_location(input: &str) -> Location {
    let text = input.trim().trim_end_matches(':');
    let Some((rest, last)) = text.rsplit_once(':') else {
        return Location {
            path: PathBuf::from(text),
            line: None,
            col: None,
        };
    };
    let Some(last_num) = number(last).filter(|_| !rest.is_empty()) else {
        return Location {
            path: PathBuf::from(text),
            line: None,
            col: None,
        };
    };
    if let Some((path, mid)) = rest.rsplit_once(':') {
        if let Some(line) = number(mid).filter(|_| !path.is_empty()) {
            return Location {
                path: PathBuf::from(path),
                line: Some(line),
                col: Some(last_num),
            };
        }
    }
    Location {
        path: PathBuf::from(rest),
        line: Some(last_num),
        col: None,
    }
}

/// The first thing that looks like a file location in free text.
pub fn location_from_text(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|token| token.trim_matches(|c: char| "()[]{}<>\"'`,;".contains(c)))
        .find(|token| !token.is_empty())
        .map(str::to_string)
}

pub fn location_from_context(ctx: &Context) -> Option<String> {
    if let Some(url) = &ctx.clicked_url {
        return parse_clicked(url).map(|(path, line)| match line {
            Some(line) => format!("{path}:{line}"),
            None => path,
        });
    }
    ctx.selected_text.as_deref().and_then(location_from_text)
}

/// Decode `%XX` escapes (as used by `file://` OSC 8 links).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn strip_file_url(clicked: &str) -> Option<String> {
    let rest = clicked.strip_prefix("file://")?;
    let path_part = if rest.starts_with('/') {
        rest
    } else {
        let slash = rest.find('/')?;
        &rest[slash..]
    };
    Some(percent_decode(path_part))
}

/// Parse Herdr's clicked link text into `(path, line)`: a `file://` URL or
/// a bare `path[:line[:col]]` token, minus trailing sentence punctuation.
/// Adapted from ChmaraX/herdr-nvim (MIT).
pub fn parse_clicked(text: &str) -> Option<(String, Option<u32>)> {
    let decoded = strip_file_url(text).unwrap_or_else(|| text.to_owned());
    let trimmed = decoded.trim_end_matches(['.', ',']);
    let (path, line) = crate::extract::parse_token(trimmed)?;
    Some((path.to_owned(), line))
}

/// Resolve a clicked path against `cwd`, then (relative paths only) against
/// the git toplevel of `cwd`. `None` when nothing exists.
pub fn resolve_click(
    path: &str,
    cwd: &Path,
    exists: &dyn Fn(&Path) -> bool,
    git_toplevel: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Option<PathBuf> {
    let direct = crate::extract::resolve(path, cwd);
    if exists(&direct) {
        return Some(direct);
    }
    if path.starts_with('/') || path.starts_with('~') {
        return None;
    }
    let toplevel = git_toplevel(cwd)?;
    let via_toplevel = crate::extract::resolve(path, &toplevel);
    exists(&via_toplevel).then_some(via_toplevel)
}

struct EditArgs {
    location: Option<String>,
    focus: bool,
    from_context: bool,
}

fn parse_args(args: &[String]) -> Result<EditArgs> {
    let mut out = EditArgs {
        location: None,
        focus: true,
        from_context: false,
    };
    for arg in args {
        match arg.as_str() {
            "--no-focus" => out.focus = false,
            "--focus" => out.focus = true,
            "--from-context" => out.from_context = true,
            other if other.starts_with("--") => bail!("unknown edit option `{other}`"),
            other => out.location = Some(other.to_string()),
        }
    }
    Ok(out)
}

/// Where relative paths come from: the caller's shell when invoked from a
/// pane, otherwise the invocation context.
fn base_dir(host: &Host) -> PathBuf {
    if env::var_os("HERDR_PLUGIN_CONTEXT_JSON").is_none() {
        if let Ok(dir) = env::current_dir() {
            return dir;
        }
    }
    host.ctx.resolve_cwd(&host.herdr)
}

pub fn edit(args: &[String]) -> Result<i32> {
    let opts = parse_args(args)?;
    let host = Host::new()?;
    let from_click = opts.from_context
        && (host.ctx.clicked_url.is_some()
            || host.ctx.invocation_source.as_deref() == Some("link_click"));
    let raw = match opts.location {
        Some(loc) => loc,
        None if opts.from_context => match location_from_context(&host.ctx) {
            Some(loc) => loc,
            None if from_click => return Ok(0), // a misclick is never an error
            None => bail!("no file location in the selection or clicked link"),
        },
        None => bail!("usage: herdr-nvim edit <file[:line[:col]]> [--no-focus]"),
    };
    let location = parse_location(&raw);
    let path = if location.path.is_absolute() {
        location.path.clone()
    } else if from_click {
        let base = base_dir(&host);
        match resolve_click(
            &location.path.to_string_lossy(),
            &base,
            &|p| p.is_file(),
            &crate::gitscan::toplevel,
        ) {
            Some(resolved) => resolved,
            None => {
                eprintln!(
                    "herdr-nvim: clicked path {} not found",
                    location.path.display()
                );
                return Ok(0);
            }
        }
    } else {
        base_dir(&host).join(&location.path)
    };
    let parent = path.parent().unwrap_or(Path::new("/"));
    if !parent.is_dir() {
        bail!("directory {} does not exist", parent.display());
    }
    let path = match parent.canonicalize() {
        Ok(dir) => dir.join(path.file_name().unwrap_or_default()),
        Err(_) => path,
    };

    let record = host.ensure_daemon()?;
    let (pane, opened) = host.ensure_open(opts.focus)?;

    let mut commands = vec![format!("drop {}", viml_fnameescape(&path))];
    if let Some(line) = location.line {
        commands.push(format!(
            "call cursor({}, {})",
            line.max(1),
            location.col.unwrap_or(1).max(1)
        ));
        commands.push("normal! zz".to_string());
    }
    if daemon::remote_execute(&host.cfg, &record.socket, &commands).is_none() {
        bail!("the Neovim daemon did not accept the edit command");
    }
    if opts.focus && !opened {
        let _ = host.herdr.plugin_pane_focus(&pane);
    }
    println!(
        "opened {}{} in Neovim sidebar {pane}",
        path.display(),
        location.line.map(|l| format!(":{l}")).unwrap_or_default()
    );
    Ok(0)
}

/// `fnameescape()` evaluated inside Neovim, over a VimL string literal.
fn viml_fnameescape(path: &Path) -> String {
    // The command list is built with viml_string(), so this expression is
    // itself embedded as a literal; keep it plain and let :drop take it.
    let mut escaped = String::new();
    for ch in path.to_string_lossy().chars() {
        if " \t\n*?[{`$\\%#'\"|!<".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(path: &str, line: Option<u32>, col: Option<u32>) -> Location {
        Location {
            path: PathBuf::from(path),
            line,
            col,
        }
    }

    #[test]
    fn parses_locations() {
        assert_eq!(parse_location("a.rs"), loc("a.rs", None, None));
        assert_eq!(parse_location("a.rs:3"), loc("a.rs", Some(3), None));
        assert_eq!(parse_location("a.rs:3:7"), loc("a.rs", Some(3), Some(7)));
        assert_eq!(parse_location("a.rs:"), loc("a.rs", None, None));
        assert_eq!(parse_location("a.rs:3:"), loc("a.rs", Some(3), None));
        assert_eq!(
            parse_location("/abs/p.rs:1"),
            loc("/abs/p.rs", Some(1), None)
        );
        assert_eq!(
            parse_location("weird:name.txt"),
            loc("weird:name.txt", None, None)
        );
        assert_eq!(parse_location(":12"), loc(":12", None, None));
    }

    #[test]
    fn extracts_locations_from_text() {
        assert_eq!(
            location_from_text("  see (src/main.rs:42) here").as_deref(),
            Some("see")
        );
        assert_eq!(
            location_from_text("`src/main.rs:42`,").as_deref(),
            Some("src/main.rs:42")
        );
        assert_eq!(location_from_text("   "), None);
    }

    #[test]
    fn parses_clicked_links() {
        assert_eq!(
            parse_clicked("src/main.rs:42"),
            Some(("src/main.rs".into(), Some(42)))
        );
        assert_eq!(
            parse_clicked("src/main.rs:42:7"),
            Some(("src/main.rs".into(), Some(42)))
        );
        assert_eq!(
            parse_clicked("~/sub/notes.md"),
            Some(("~/sub/notes.md".into(), None))
        );
        assert_eq!(
            parse_clicked("src/main.rs."),
            Some(("src/main.rs".into(), None))
        );
        assert_eq!(
            parse_clicked("src/main.rs,"),
            Some(("src/main.rs".into(), None))
        );
        assert_eq!(
            parse_clicked("README.md"),
            None,
            "bare filename is not path-shaped"
        );
        assert_eq!(
            parse_clicked("file:///Users/a/src/main.rs:10"),
            Some(("/Users/a/src/main.rs".into(), Some(10)))
        );
        assert_eq!(
            parse_clicked("file://localhost/Users/a/src/main.rs"),
            Some(("/Users/a/src/main.rs".into(), None))
        );
        assert_eq!(
            parse_clicked("file:///Users/a/my%20project/main.rs"),
            Some(("/Users/a/my project/main.rs".into(), None))
        );
    }

    #[test]
    fn resolves_clicks_against_cwd_then_toplevel() {
        let exists = |p: &Path| p == Path::new("/repo/src/main.rs");
        let no_toplevel = |_: &Path| -> Option<PathBuf> { panic!("not needed") };
        assert_eq!(
            resolve_click("src/main.rs", Path::new("/repo"), &exists, &no_toplevel),
            Some(PathBuf::from("/repo/src/main.rs"))
        );
        let toplevel = |_: &Path| Some(PathBuf::from("/repo"));
        assert_eq!(
            resolve_click(
                "src/main.rs",
                Path::new("/repo/sub/dir"),
                &exists,
                &toplevel
            ),
            Some(PathBuf::from("/repo/src/main.rs"))
        );
        assert_eq!(
            resolve_click(
                "src/ghost.rs",
                Path::new("/repo/sub"),
                &|_| false,
                &toplevel
            ),
            None
        );
        assert_eq!(
            resolve_click(
                "/tmp/ghost.rs",
                Path::new("/repo"),
                &|_| false,
                &no_toplevel
            ),
            None
        );
    }

    #[test]
    fn escapes_paths_for_drop() {
        assert_eq!(
            viml_fnameescape(Path::new("/a b/c#d.rs")),
            "/a\\ b/c\\#d.rs"
        );
        assert_eq!(
            viml_fnameescape(Path::new("/plain/file.rs")),
            "/plain/file.rs"
        );
    }
}
