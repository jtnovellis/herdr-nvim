//! Plugin configuration: `config.env` in the plugin config directory,
//! overridden by environment variables of the same name.

use crate::state::plugin_id;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub nvim: String,
    pub side: Side,
    pub width: f64,
    pub daemon_args: Vec<String>,
    pub grace_ms: u64,
    pub max_snippet_lines: usize,
    pub save_on_close: bool,
    pub picker_scan_lines: u32,
    pub picker_max_files: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            nvim: "nvim".to_string(),
            side: Side::Right,
            width: 0.45,
            daemon_args: Vec::new(),
            grace_ms: 1500,
            max_snippet_lines: 80,
            save_on_close: true,
            picker_scan_lines: 300,
            picker_max_files: 20,
        }
    }
}

impl Config {
    pub fn load() -> Config {
        let mut values = HashMap::new();
        if let Some(dir) = config_dir() {
            if let Ok(content) = fs::read_to_string(dir.join("config.env")) {
                values.extend(parse_env_file(&content));
            }
        }
        // Real environment variables win over the file.
        for (key, value) in env::vars() {
            if key.starts_with("HERDR_NVIM_") {
                values.insert(key, value);
            }
        }
        Config::from_values(&values)
    }

    fn from_values(values: &HashMap<String, String>) -> Config {
        let mut cfg = Config::default();
        let get = |key: &str| values.get(key).map(|v| v.trim()).filter(|v| !v.is_empty());

        if let Some(nvim) = get("HERDR_NVIM_NVIM") {
            cfg.nvim = expand_home(nvim);
        }
        if let Some(side) = get("HERDR_NVIM_SIDE") {
            cfg.side = match side.to_ascii_lowercase().as_str() {
                "left" => Side::Left,
                _ => Side::Right,
            };
        }
        if let Some(width) = get("HERDR_NVIM_WIDTH").and_then(|w| w.parse::<f64>().ok()) {
            if width.is_finite() {
                cfg.width = width.clamp(0.1, 0.9);
            }
        }
        if let Some(args) = get("HERDR_NVIM_ARGS") {
            cfg.daemon_args = args.split_whitespace().map(expand_home).collect();
        }
        if let Some(ms) = get("HERDR_NVIM_GRACE_MS").and_then(|v| v.parse::<u64>().ok()) {
            cfg.grace_ms = ms.clamp(100, 60_000);
        }
        if let Some(n) = get("HERDR_NVIM_MAX_SNIPPET_LINES").and_then(|v| v.parse::<usize>().ok()) {
            cfg.max_snippet_lines = n.max(1);
        }
        if let Some(n) = get("HERDR_NVIM_PICKER_SCAN_LINES").and_then(|v| v.parse::<u32>().ok()) {
            cfg.picker_scan_lines = n.clamp(20, 2000);
        }
        if let Some(n) = get("HERDR_NVIM_PICKER_MAX_FILES").and_then(|v| v.parse::<u32>().ok()) {
            cfg.picker_max_files = n.clamp(1, 200);
        }
        if let Some(flag) = get("HERDR_NVIM_SAVE_ON_CLOSE") {
            cfg.save_on_close = !matches!(
                flag.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            );
        }
        cfg
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The plugin config directory. Plugin invocations get it from Herdr;
/// ordinary panes derive Herdr's layout or ask the CLI.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = env_path("HERDR_PLUGIN_CONFIG_DIR") {
        return Some(dir);
    }
    if let Some(dir) = env_path("HERDR_NVIM_CONFIG_DIR") {
        return Some(dir);
    }
    let id = plugin_id();
    let base =
        env_path("XDG_CONFIG_HOME").or_else(|| env_path("HOME").map(|h| h.join(".config")))?;
    let derived = base.join("herdr").join("plugins").join("config").join(&id);
    if derived.is_dir() {
        return Some(derived);
    }
    let bin = env_path("HERDR_BIN_PATH").unwrap_or_else(|| PathBuf::from("herdr"));
    let output = Command::new(bin)
        .args(["plugin", "config-dir", &id])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

pub fn expand_home(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home)
                .join(rest)
                .to_string_lossy()
                .into_owned();
        }
    }
    value.to_string()
}

pub fn parse_env_file(content: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let mut value = value.trim();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = &value[1..value.len() - 1];
        }
        out.insert(key.to_string(), value.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_file_with_comments_and_quotes() {
        let parsed = parse_env_file(
            "# comment\nHERDR_NVIM_SIDE=right\nexport HERDR_NVIM_WIDTH=\"0.3\"\n\nBAD LINE\nEMPTY=\n",
        );
        assert_eq!(
            parsed.get("HERDR_NVIM_SIDE").map(String::as_str),
            Some("right")
        );
        assert_eq!(
            parsed.get("HERDR_NVIM_WIDTH").map(String::as_str),
            Some("0.3")
        );
        assert_eq!(parsed.get("EMPTY").map(String::as_str), Some(""));
        assert!(!parsed.contains_key("BAD LINE"));
    }

    #[test]
    fn config_values_are_validated() {
        let mut values = HashMap::new();
        values.insert("HERDR_NVIM_SIDE".to_string(), "LEFT".to_string());
        values.insert("HERDR_NVIM_PICKER_MAX_FILES".to_string(), "999".to_string());
        values.insert("HERDR_NVIM_WIDTH".to_string(), "5".to_string());
        values.insert("HERDR_NVIM_GRACE_MS".to_string(), "1".to_string());
        values.insert("HERDR_NVIM_ARGS".to_string(), "-u  none".to_string());
        values.insert("HERDR_NVIM_SAVE_ON_CLOSE".to_string(), "off".to_string());
        let cfg = Config::from_values(&values);
        assert_eq!(cfg.side, Side::Left);
        assert_eq!(cfg.picker_max_files, 200);
        assert_eq!(cfg.width, 0.9);
        assert_eq!(cfg.grace_ms, 100);
        assert_eq!(cfg.daemon_args, vec!["-u", "none"]);
        assert!(!cfg.save_on_close);
    }

    #[test]
    fn defaults_when_unset() {
        let cfg = Config::from_values(&HashMap::new());
        assert_eq!(cfg.side, Side::Right);
        assert_eq!(cfg.picker_scan_lines, 300);
        assert_eq!(cfg.nvim, "nvim");
        assert!(cfg.save_on_close);
        assert!((cfg.width - 0.45).abs() < f64::EPSILON);
    }

    #[test]
    fn config_dir_prefers_explicit_env() {
        let _guard = crate::state::ENV_LOCK.lock().unwrap();
        let saved: Vec<(&str, Option<std::ffi::OsString>)> =
            ["HERDR_PLUGIN_CONFIG_DIR", "HERDR_NVIM_CONFIG_DIR"]
                .iter()
                .map(|k| (*k, env::var_os(k)))
                .collect();
        env::set_var("HERDR_NVIM_CONFIG_DIR", "/nvim-cfg");
        env::remove_var("HERDR_PLUGIN_CONFIG_DIR");
        assert_eq!(config_dir(), Some(PathBuf::from("/nvim-cfg")));
        env::set_var("HERDR_PLUGIN_CONFIG_DIR", "/plugin-cfg");
        assert_eq!(config_dir(), Some(PathBuf::from("/plugin-cfg")));
        for (key, value) in saved {
            match value {
                Some(v) => env::set_var(key, v),
                None => env::remove_var(key),
            }
        }
    }
}
