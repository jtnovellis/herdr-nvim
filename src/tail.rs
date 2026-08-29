//! Read an agent's transcript forward from a byte offset.
//!
//! `agent.prompt` answers with lifecycle state and never text, so the reply
//! to a message cannot come back through the call that sent it. It can be
//! read, though: Herdr reports the transcript behind every agent pane, and
//! that file is append-only JSONL. `ask` records the file's length at the
//! moment it sends; everything appended past that offset is the answer to
//! that message, so tailing from it needs no diffing and no guesswork about
//! where one turn ends and the next begins.

use crate::sessions;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

#[derive(Debug, Default)]
struct TailArgs {
    path: Option<PathBuf>,
    agent: String,
    from: u64,
}

fn parse_tail_args(args: &[String]) -> Result<TailArgs> {
    let mut out = TailArgs::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--path" => {
                out.path = Some(PathBuf::from(
                    iter.next().context("--path needs a transcript path")?,
                ))
            }
            "--agent" => out.agent = iter.next().cloned().context("--agent needs a kind")?,
            "--from" => {
                out.from = iter
                    .next()
                    .context("--from needs a byte offset")?
                    .parse()
                    .context("--from must be a byte offset")?
            }
            other => bail!("unknown tail option `{other}`"),
        }
    }
    Ok(out)
}

/// `herdr-nvim tail`: prints one JSON object describing what the agent has
/// said and done since `--from`.
pub fn tail(args: &[String]) -> Result<i32> {
    match tail_inner(args) {
        Ok(value) => {
            println!("{value}");
            Ok(0)
        }
        Err(err) => {
            println!("{}", crate::send::error_json(&err));
            Ok(1)
        }
    }
}

fn tail_inner(args: &[String]) -> Result<Value> {
    let opts = parse_tail_args(args)?;
    let path = opts.path.context("tail needs --path")?;
    let (text, offset) = read_from(&path, opts.from)?;
    let reply = sessions::assistant_text(&opts.agent, &text);
    let edits = sessions::mine_edits(&opts.agent, &text);
    Ok(json!({
        "ok": true,
        "offset": offset,
        "reply": reply,
        "edits": edits,
    }))
}

/// The transcript text after `from`, plus the offset to resume at.
///
/// Two things can go wrong with a remembered offset, and both mean "read the
/// whole file": the session was replaced by a shorter one (offset past the
/// end), or the offset landed mid-line. A JSONL line is only meaningful whole,
/// so a partial leading line is dropped rather than handed to the parser.
pub(crate) fn read_from(path: &std::path::Path, from: u64) -> Result<(String, u64)> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let len = file.metadata()?.len();
    if from >= len {
        return Ok((String::new(), len));
    }
    let mut partial = false;
    if from > 0 {
        file.seek(SeekFrom::Start(from - 1))?;
        let mut prev = [0u8; 1];
        if file.read_exact(&mut prev).is_ok() && prev[0] != b'\n' {
            partial = true;
        }
    }
    file.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if partial {
        text = match text.find('\n') {
            Some(nl) => text[nl + 1..].to_owned(),
            None => String::new(),
        };
    }
    Ok((text, len))
}

/// The transcript's current length, used by `ask` to mark where its own
/// message ends and the reply begins. Zero when the file cannot be read --
/// the tail then simply starts from the beginning.
pub(crate) fn offset_of(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("hn-tail-{name}-{}", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn reads_only_what_was_appended() {
        let path = tmp("append", "one\ntwo\n");
        let (text, offset) = read_from(&path, 4).unwrap();
        assert_eq!(text, "two\n");
        assert_eq!(offset, 8);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn offset_past_the_end_yields_nothing_not_an_error() {
        let path = tmp("past", "one\n");
        let (text, offset) = read_from(&path, 999).unwrap();
        assert!(text.is_empty());
        assert_eq!(offset, 4);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_mid_line_offset_drops_the_partial_line() {
        // Offset 2 lands inside "one\n"; that half-line is not parseable JSON.
        let path = tmp("partial", "one\ntwo\n");
        let (text, _) = read_from(&path, 2).unwrap();
        assert_eq!(text, "two\n");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_unknown_options() {
        assert!(parse_tail_args(&["--nope".to_string()]).is_err());
    }
}
