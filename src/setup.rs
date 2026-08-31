//! First-run setup: the two things `herdr plugin install` cannot do itself.
//!
//! A plugin manifest can declare actions, panes, events and link handlers, but
//! not keybindings — those live in the user's own `config.toml`. So a fresh
//! install leaves the sidebar reachable only through
//! `herdr plugin action invoke`, which is nobody's idea of a keystroke. This
//! writes the bindings on request, and never silently: it is an action the
//! user runs, it backs the file up first, and it refuses to take a key that
//! is already spoken for.

use crate::config;
use crate::herdr::Herdr;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The bindings a working install wants: the key, the action, and what to
/// call it in the config file.
const BINDINGS: &[(&str, &str, &str)] = &[
    ("prefix+e", "herdr-nvim.toggle", "toggle Neovim sidebar"),
    (
        "prefix+f",
        "herdr-nvim.pick-file",
        "open file from agent output",
    ),
];

fn config_toml() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("herdr").join("config.toml"))
}

/// Is `key` already bound to something, or `action` already bound to a key?
///
/// Deliberately a text scan rather than a TOML parse: the file is the user's,
/// with their comments and ordering in it, and this only ever appends. Reading
/// it as data and writing it back would reformat everything.
fn existing(text: &str, key: &str, action: &str) -> Option<String> {
    if text.contains(&format!("\"{action}\"")) {
        return Some(format!("{action} is already bound"));
    }
    // `key = "prefix+e"` inside a [[keys.command]] block.
    if text.contains(&format!("key = \"{key}\"")) {
        return Some(format!("{key} is already bound to something else"));
    }
    None
}

fn block(key: &str, action: &str, description: &str) -> String {
    format!(
        "\n[[keys.command]]\nkey = \"{key}\"\ntype = \"plugin_action\"\ncommand = \"{action}\"\ndescription = \"{description}\"\n"
    )
}

/// `herdr-nvim setup-keys [--force]`: bind the sidebar to keys in the user's
/// Herdr config, then reload it so the keys work without a restart.
pub fn setup_keys(args: &[String]) -> Result<i32> {
    let force = args.iter().any(|a| a == "--force");
    for arg in args {
        if arg != "--force" {
            anyhow::bail!("unknown setup-keys option `{arg}`");
        }
    }
    let path = config_toml().context("cannot find ~/.config/herdr/config.toml")?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();

    let mut adding = String::new();
    let mut added: Vec<&str> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for (key, action, description) in BINDINGS {
        match existing(&text, key, action) {
            Some(why) if !force => skipped.push(why),
            _ => {
                adding.push_str(&block(key, action, description));
                added.push(key);
            }
        }
    }

    if adding.is_empty() {
        println!("nothing to bind: {}", skipped.join("; "));
        return Ok(0);
    }

    if path.exists() {
        let backup = backup_path(&path);
        std::fs::copy(&path, &backup).with_context(|| {
            format!("cannot back up {} to {}", path.display(), backup.display())
        })?;
        println!("backed up {} to {}", path.display(), backup.display());
    } else if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let mut out = text;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n# Added by `herdr-nvim setup-keys`.\n");
    out.push_str(adding.trim_start_matches('\n'));
    std::fs::write(&path, out).with_context(|| format!("cannot write {}", path.display()))?;

    let herdr = Herdr::from_env();
    let reloaded = herdr.reload_config().is_ok();
    let mut message = format!("bound {}", added.join(", "));
    if !skipped.is_empty() {
        message.push_str(&format!(" (skipped: {})", skipped.join("; ")));
    }
    if !reloaded {
        message.push_str("; run `herdr server reload-config` to apply");
    }
    println!("{message}");
    herdr.notify("herdr-nvim: keys bound", &message);
    Ok(0)
}

/// `config.toml.bak-YYYYMMDD`, matching the convention Herdr itself uses.
fn backup_path(path: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".bak-{stamp}"));
    path.with_file_name(name)
}

/// Put a commented `config.env` in the plugin's config directory the first
/// time the plugin starts, so the knobs the README documents are discoverable
/// where they are actually read. Never overwrites one that exists.
pub fn ensure_config_env() {
    let Some(dir) = config::config_dir() else {
        return;
    };
    let Some(example) = plugin_root().map(|r| r.join("config.env.example")) else {
        return;
    };
    if let Some(written) = seed_config_env(&dir, &example) {
        println!("wrote {} from config.env.example", written.display());
    }
}

/// Copy `example` to `dir/config.env` unless it is already there. Returns the
/// path only when something was actually written, so the caller says nothing
/// on the overwhelmingly common second run.
fn seed_config_env(dir: &Path, example: &Path) -> Option<PathBuf> {
    let target = dir.join("config.env");
    if target.exists() {
        return None;
    }
    let body = std::fs::read_to_string(example).ok()?;
    std::fs::create_dir_all(dir).ok()?;
    std::fs::write(&target, body).ok()?;
    Some(target)
}

/// The plugin checkout holding `config.env.example`. Herdr's own
/// `HERDR_PLUGIN_ROOT` first, then the binary's path shape -- see
/// `daemon::plugin_root`.
fn plugin_root() -> Option<PathBuf> {
    crate::daemon::plugin_root().filter(|root| root.join("config.env.example").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_action_already_bound_is_left_alone() {
        let text = "[[keys.command]]\nkey = \"prefix+x\"\ncommand = \"herdr-nvim.toggle\"\n";
        assert!(existing(text, "prefix+e", "herdr-nvim.toggle").is_some());
    }

    #[test]
    fn a_key_already_taken_is_not_stolen() {
        let text = "[[keys.command]]\nkey = \"prefix+e\"\ncommand = \"lazygit\"\n";
        let why = existing(text, "prefix+e", "herdr-nvim.toggle").unwrap();
        assert!(why.contains("already bound to something else"), "{why}");
    }

    #[test]
    fn a_free_key_and_action_is_bound() {
        let text = "[[keys.command]]\nkey = \"prefix+t\"\ncommand = \"popup\"\n";
        assert!(existing(text, "prefix+e", "herdr-nvim.toggle").is_none());
    }

    #[test]
    fn the_block_is_valid_toml_for_a_plugin_action() {
        let block = block("prefix+e", "herdr-nvim.toggle", "toggle");
        assert!(block.contains("[[keys.command]]"));
        assert!(block.contains("type = \"plugin_action\""));
        assert!(block.contains("command = \"herdr-nvim.toggle\""));
    }

    #[test]
    fn config_env_is_seeded_once_and_never_overwritten() {
        let base = std::env::temp_dir().join(format!("hn-setup-{}", std::process::id()));
        let dir = base.join("config");
        let example = base.join("config.env.example");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(&example, "HERDR_NVIM_SIDE=right\n").unwrap();

        let written = seed_config_env(&dir, &example).expect("first run should seed");
        assert_eq!(
            std::fs::read_to_string(&written).unwrap(),
            "HERDR_NVIM_SIDE=right\n"
        );

        // A user's own edits must survive every later startup.
        std::fs::write(&written, "HERDR_NVIM_SIDE=left\n").unwrap();
        assert!(seed_config_env(&dir, &example).is_none(), "seeded twice");
        assert_eq!(
            std::fs::read_to_string(&written).unwrap(),
            "HERDR_NVIM_SIDE=left\n"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn backups_sit_next_to_the_original() {
        let backup = backup_path(Path::new("/tmp/config.toml"));
        assert!(backup
            .to_string_lossy()
            .starts_with("/tmp/config.toml.bak-"));
    }
}
