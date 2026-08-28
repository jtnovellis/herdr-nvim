//! Invocation context injected by Herdr into plugin commands.

use crate::herdr::Herdr;
use serde_json::Value;
use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct Context {
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    /// The focused pane for actions, or the pane hosting a pane command.
    pub pane_id: Option<String>,
    /// Best-known working directory from the context JSON (may be None).
    pub cwd: Option<PathBuf>,
    pub selected_text: Option<String>,
    pub clicked_url: Option<String>,
    pub invocation_source: Option<String>,
}

fn env_str(key: &str) -> Option<String> {
    env::var(key).ok().filter(|s| !s.trim().is_empty())
}

impl Context {
    pub fn from_env() -> Context {
        let json: Value = env::var("HERDR_PLUGIN_CONTEXT_JSON")
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or(Value::Null);

        let json_str = |key: &str| {
            json.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };

        let tab_id = env_str("HERDR_NVIM_TAB_ID")
            .or_else(|| env_str("HERDR_TAB_ID"))
            .or_else(|| json_str("tab_id"));
        let workspace_id = env_str("HERDR_WORKSPACE_ID")
            .or_else(|| json_str("workspace_id"))
            .or_else(|| tab_id.as_deref().and_then(workspace_of_tab));
        let pane_id = env_str("HERDR_PANE_ID").or_else(|| json_str("focused_pane_id"));
        let cwd = json_str("focused_pane_cwd")
            .or_else(|| json_str("workspace_cwd"))
            .map(PathBuf::from)
            .filter(|p| p.is_dir());

        Context {
            workspace_id,
            tab_id,
            pane_id,
            cwd,
            selected_text: json_str("selected_text"),
            clicked_url: json_str("clicked_url").or_else(|| env_str("HERDR_PLUGIN_CLICKED_URL")),
            invocation_source: json_str("invocation_source"),
        }
    }

    /// Working directory for a new daemon: context JSON, then the focused
    /// pane's directory, then the workspace, then the process cwd unless that
    /// is the plugin checkout (runtime commands start there), then `$HOME`.
    pub fn resolve_cwd(&self, herdr: &Herdr) -> PathBuf {
        if let Some(cwd) = &self.cwd {
            return cwd.clone();
        }
        if let Some(pane) = &self.pane_id {
            if let Ok(Some(info)) = herdr.pane_get(pane) {
                if let Some(dir) = info.foreground_cwd.or(info.cwd).map(PathBuf::from) {
                    if dir.is_dir() {
                        return dir;
                    }
                }
            }
        }
        if let Some(ws) = &self.workspace_id {
            if let Some(dir) = herdr.workspace_cwd(ws).map(PathBuf::from) {
                if dir.is_dir() {
                    return dir;
                }
            }
        }
        let plugin_root = env::var_os("HERDR_PLUGIN_ROOT").map(PathBuf::from);
        if let Ok(current) = env::current_dir() {
            if plugin_root.as_deref() != Some(current.as_path()) {
                return current;
            }
        }
        env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))
    }
}

/// Public tab ids look like `w1:t1`; the workspace is the part before the colon.
pub fn workspace_of_tab(tab_id: &str) -> Option<String> {
    tab_id
        .split_once(':')
        .map(|(ws, _)| ws.to_string())
        .filter(|ws| !ws.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_workspace_from_tab_id() {
        assert_eq!(workspace_of_tab("wF:t2").as_deref(), Some("wF"));
        assert_eq!(workspace_of_tab("nocolon"), None);
        assert_eq!(workspace_of_tab(":t1"), None);
    }
}
