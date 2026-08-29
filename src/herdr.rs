//! Talking to Herdr: the CLI (via `HERDR_BIN_PATH`) for everything the CLI
//! exposes, and the raw NDJSON socket for the few methods it does not.

use anyhow::{anyhow, bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// A structured error returned by the Herdr server (`{"error":{"code","message"}}`).
#[derive(Debug, Clone)]
pub struct HerdrError {
    pub code: String,
    pub message: String,
}

impl fmt::Display for HerdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            write!(f, "{}", self.code)
        } else {
            write!(f, "{} ({})", self.message, self.code)
        }
    }
}

impl std::error::Error for HerdrError {}

fn herdr_error(value: &Value) -> anyhow::Error {
    anyhow::Error::new(HerdrError {
        code: value
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_string(),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

pub fn error_code(err: &anyhow::Error) -> Option<&str> {
    err.downcast_ref::<HerdrError>().map(|e| e.code.as_str())
}

/// Error codes meaning "that pane/agent does not exist (any more)".
pub const GONE_CODES: &[&str] = &[
    "not_found",
    "pane_not_found",
    "plugin_pane_not_found",
    "agent_pane_not_found",
    "agent_not_found",
];

pub fn is_gone(err: &anyhow::Error) -> bool {
    error_code(err).is_some_and(|code| GONE_CODES.contains(&code))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub launch_pending: Option<bool>,
    /// The transcript Herdr tracks for this agent, when it tracks one. This is
    /// the only route to the agent's actual words: `agent.prompt` answers with
    /// lifecycle state, never text.
    #[serde(default)]
    pub agent_session: Option<AgentSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PaneScroll {
    #[serde(default)]
    pub viewport_rows: u32,
    #[serde(default)]
    pub max_offset_from_bottom: u32,
}

impl PaneScroll {
    /// Lines Herdr serves without driving the application's own scroll.
    pub fn cheap_read_limit(self) -> u32 {
        self.viewport_rows
            .saturating_add(self.max_offset_from_bottom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    #[serde(default)]
    pub terminal_id: Option<String>,
    #[serde(default)]
    pub tab_id: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub agent_session: Option<AgentSession>,
    #[serde(default)]
    pub scroll: Option<PaneScroll>,
}

/// Rects of one tab plus whether it is zoomed.
pub struct TabLayout {
    pub zoomed: bool,
    pub rects: Vec<crate::layout::PaneRect>,
}

pub struct Herdr {
    pub bin: PathBuf,
    pub socket: PathBuf,
}

impl Herdr {
    pub fn from_env() -> Herdr {
        let bin = env::var_os("HERDR_BIN_PATH")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("herdr"));
        let socket = env::var_os("HERDR_SOCKET_PATH")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(default_socket);
        Herdr { bin, socket }
    }

    /// Identifies the Herdr session; daemon state is scoped by it because
    /// tab ids are only unique inside one session.
    pub fn session_key(&self) -> String {
        self.socket.to_string_lossy().into_owned()
    }

    /// Run a CLI command and return its `result` (or the parsed output).
    /// Scrollback text from a pane. `source` uses the socket spelling
    /// (`recent_unwrapped`), not the CLI's hyphenated form.
    pub fn pane_read(&self, pane_id: &str, source: &str, lines: u32) -> Result<String> {
        let result = self.rpc(
            "pane.read",
            json!({
                "pane_id": pane_id,
                "source": source,
                "lines": lines,
                "format": "text",
            }),
        )?;
        Ok(result
            .pointer("/read/text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Run a CLI command whose stdout is plain text (e.g. `pane read`).
    /// Send one raw request over the session socket and return its `result`.
    pub fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let mut stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("cannot connect to Herdr socket {}", self.socket.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let id = format!("herdr-nvim:{method}");
        let request = json!({
            "id": &id,
            "method": method,
            "params": params,
        });
        stream.write_all(request.to_string().as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim().is_empty() {
            bail!("empty response from Herdr for {method}");
        }
        let value: Value = serde_json::from_str(line.trim())
            .with_context(|| format!("invalid JSON response for {method}"))?;
        // Reject a frame that is not the reply to this request rather than
        // mistaking an unsolicited event for our result.
        if let Some(got) = value.get("id").and_then(Value::as_str) {
            if got != id {
                bail!("Herdr replied to `{got}` while waiting for `{id}`");
            }
        }
        if let Some(err) = value.get("error") {
            return Err(herdr_error(err));
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }

    // ----- panes ----------------------------------------------------------

    /// `Ok(None)` only when Herdr says the pane does not exist; transport or
    /// server failures propagate so callers never mistake them for "gone".
    pub fn pane_get(&self, pane_id: &str) -> Result<Option<PaneInfo>> {
        match self.rpc("pane.get", json!({ "pane_id": pane_id })) {
            Ok(result) => {
                let pane = result
                    .get("pane")
                    .cloned()
                    .ok_or_else(|| anyhow!("unexpected `pane get` response"))?;
                Ok(Some(
                    serde_json::from_value(pane).context("cannot parse pane info")?,
                ))
            }
            Err(err) if is_gone(&err) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// argv of every foreground process in the pane.
    pub fn pane_process_argv(&self, pane_id: &str) -> Result<Vec<Vec<String>>> {
        let result = self.rpc("pane.process_info", json!({ "pane_id": pane_id }))?;
        let info = result.get("process_info").unwrap_or(&result);
        Ok(info
            .get("foreground_processes")
            .and_then(Value::as_array)
            .map(|procs| {
                procs
                    .iter()
                    .filter_map(|p| p.get("argv").and_then(Value::as_array))
                    .map(|argv| {
                        argv.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Does the pane run a `--remote-ui` client attached to `socket`?
    pub fn sidebar_owned(&self, pane_id: &str, socket: &str) -> bool {
        self.pane_process_argv(pane_id)
            .map(|procs| {
                procs.iter().any(|argv| {
                    argv.iter().any(|a| a == "--remote-ui") && argv.iter().any(|a| a == socket)
                })
            })
            .unwrap_or(false)
    }

    pub fn pane_close(&self, pane_id: &str) -> Result<()> {
        self.rpc("pane.close", json!({ "pane_id": pane_id }))
            .map(|_| ())
    }

    pub fn pane_swap(&self, source: &str, target: &str) -> Result<()> {
        let result = self.rpc(
            "pane.swap",
            json!({ "source_pane_id": source, "target_pane_id": target }),
        )?;
        let swap = result.get("swap").unwrap_or(&result);
        if swap.get("changed").and_then(Value::as_bool) == Some(false) {
            bail!(
                "pane swap {source} <-> {target} did nothing ({})",
                swap.get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            );
        }
        Ok(())
    }

    pub fn pane_zoom_off(&self, pane_id: &str) -> Result<()> {
        self.rpc("pane.zoom", json!({ "pane_id": pane_id, "mode": "off" }))
            .map(|_| ())
    }

    /// First pane id of a tab (from `pane list`).
    pub fn any_pane_in_tab(&self, tab_id: &str) -> Result<Option<String>> {
        let result = self.rpc("pane.list", json!({}))?;
        Ok(result
            .get("panes")
            .and_then(Value::as_array)
            .and_then(|panes| {
                panes.iter().find_map(|p| {
                    (p.get("tab_id").and_then(Value::as_str) == Some(tab_id))
                        .then(|| p.get("pane_id").and_then(Value::as_str).map(str::to_string))
                        .flatten()
                })
            }))
    }

    /// Layout of the tab containing `pane_id`.
    pub fn tab_layout(&self, pane_id: &str) -> Result<TabLayout> {
        let result = self.rpc("pane.layout", json!({ "pane_id": pane_id }))?;
        let layout = result
            .get("layout")
            .ok_or_else(|| anyhow!("unexpected `pane layout` response"))?;
        Ok(TabLayout {
            zoomed: layout
                .get("zoomed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            rects: crate::layout::parse_pane_rects(layout)?,
        })
    }

    pub fn pane_set_title(&self, pane_id: &str, source: &str, title: Option<&str>) -> Result<()> {
        match title.map(str::trim).filter(|t| !t.is_empty()) {
            Some(title) => self.rpc(
                "pane.report_metadata",
                json!({ "pane_id": pane_id, "source": source, "title": title }),
            ),
            None => self.rpc(
                "pane.report_metadata",
                json!({ "pane_id": pane_id, "source": source, "clear_title": true }),
            ),
        }
        .map(|_| ())
    }

    /// Insert text and/or keys into a pane. `pane.send_input` honors
    /// bracketed paste, so multi-line text lands as one block.
    pub fn pane_send_input(
        &self,
        pane_id: &str,
        text: Option<&str>,
        keys: &[&str],
    ) -> Result<Value> {
        let mut params = json!({ "pane_id": pane_id });
        if let Some(text) = text {
            params["text"] = Value::String(text.to_string());
        }
        if !keys.is_empty() {
            params["keys"] = json!(keys);
        }
        self.rpc("pane.send_input", params)
    }

    // ----- plugin panes ---------------------------------------------------

    pub fn plugin_pane_open(
        &self,
        plugin_id: &str,
        entrypoint: &str,
        target_pane: Option<&str>,
        cwd: Option<&str>,
        env: &[(String, String)],
        focus: bool,
    ) -> Result<PaneInfo> {
        let env_map: serde_json::Map<String, Value> = env
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        let mut params = json!({
            "plugin_id": plugin_id,
            "entrypoint": entrypoint,
            "placement": "split",
            "direction": "right",
            "env": env_map,
            "focus": focus,
        });
        if let Some(target) = target_pane {
            params["target_pane_id"] = json!(target);
        }
        if let Some(cwd) = cwd {
            params["cwd"] = json!(cwd);
        }
        let result = self.rpc("plugin.pane.open", params)?;
        let pane = result
            .pointer("/plugin_pane/pane")
            .cloned()
            .ok_or_else(|| anyhow!("`plugin pane open` returned no pane: {result}"))?;
        serde_json::from_value(pane).context("cannot parse opened pane")
    }

    pub fn plugin_pane_focus(&self, pane_id: &str) -> Result<()> {
        self.rpc("plugin.pane.focus", json!({ "pane_id": pane_id }))
            .map(|_| ())
    }

    // ----- layout ---------------------------------------------------------

    /// Create an unfocused tab; returns `(tab_id, root_pane_id)`.
    pub fn tab_create(&self, workspace_id: &str, label: &str) -> Result<(String, String)> {
        let result = self.rpc(
            "tab.create",
            json!({ "workspace_id": workspace_id, "label": label, "focus": false }),
        )?;
        let tab = result
            .pointer("/tab/tab_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("`tab create` returned no tab id"))?;
        let pane = result
            .pointer("/root_pane/pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("`tab create` returned no root pane"))?;
        Ok((tab.to_string(), pane.to_string()))
    }

    /// Move a running pane into `tab`, splitting `target` (or the tab's
    /// focused pane) in `dir` with the given ratio.
    pub fn pane_move(
        &self,
        pane_id: &str,
        tab_id: &str,
        dir: crate::layout::Dir,
        target: Option<&str>,
        ratio: Option<f64>,
    ) -> Result<()> {
        let mut destination = json!({
            "type": "tab",
            "tab_id": tab_id,
            "split": dir.as_cli_arg(),
        });
        if let Some(target) = target {
            destination["target_pane_id"] = json!(target);
        }
        if let Some(ratio) = ratio {
            destination["ratio"] = json!(ratio);
        }
        let result = self.rpc(
            "pane.move",
            json!({ "pane_id": pane_id, "destination": destination, "focus": false }),
        )?;
        let mv = result.get("move_result").unwrap_or(&result);
        if mv.get("changed").and_then(Value::as_bool) == Some(false) {
            bail!(
                "pane move {pane_id} -> {tab_id} did nothing ({})",
                mv.get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            );
        }
        if let Some(new_id) = mv.pointer("/pane/pane_id").and_then(Value::as_str) {
            if new_id != pane_id {
                bail!("pane move changed the pane id ({pane_id} -> {new_id}); aborting");
            }
        }
        Ok(())
    }

    pub fn tab_close(&self, tab_id: &str) -> Result<()> {
        self.rpc("tab.close", json!({ "tab_id": tab_id }))
            .map(|_| ())
    }

    pub fn layout_export(&self, pane_id: &str) -> Result<Value> {
        let result = self.rpc("layout.export", json!({ "pane_id": pane_id }))?;
        result
            .get("layout")
            .cloned()
            .ok_or_else(|| anyhow!("unexpected layout.export response"))
    }

    pub fn layout_set_split_ratio(&self, tab_id: &str, path: &[bool], ratio: f64) -> Result<()> {
        self.rpc(
            "layout.set_split_ratio",
            json!({ "tab_id": tab_id, "path": path, "ratio": ratio }),
        )
        .map(|_| ())
    }

    // ----- tabs / workspaces / sessions ------------------------------------

    pub fn tab_ids(&self) -> Result<Vec<String>> {
        let result = self.rpc("tab.list", json!({}))?;
        Ok(result
            .get("tabs")
            .and_then(Value::as_array)
            .map(|tabs| {
                tabs.iter()
                    .filter_map(|t| t.get("tab_id").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// `workspace_id -> label` for every workspace in the session.
    pub fn workspace_labels(&self) -> Result<Vec<(String, String)>> {
        let result = self.rpc("workspace.list", json!({}))?;
        Ok(result
            .get("workspaces")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|w| {
                        Some((
                            w.get("workspace_id")?.as_str()?.to_string(),
                            w.get("label")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn workspace_cwd(&self, workspace_id: &str) -> Option<String> {
        let result = self
            .rpc("workspace.get", json!({ "workspace_id": workspace_id }))
            .ok()?;
        result
            .pointer("/workspace/cwd")
            .or_else(|| result.pointer("/workspace/worktree/checkout_path"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// Socket paths of sessions Herdr currently reports as running. Works
    /// without a server. `None` when the CLI itself failed.
    pub fn running_session_sockets(&self) -> Option<Vec<String>> {
        let output = Command::new(&self.bin)
            .args(["session", "list", "--json"])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let value: Value = serde_json::from_slice(&output.stdout).ok()?;
        Some(parse_running_sessions(&value))
    }

    // ----- agents ---------------------------------------------------------

    pub fn agents(&self) -> Result<Vec<AgentInfo>> {
        let result = self.rpc("agent.list", json!({}))?;
        let agents = result
            .get("agents")
            .cloned()
            .ok_or_else(|| anyhow!("unexpected `agent list` response"))?;
        serde_json::from_value(agents).context("cannot parse agent list")
    }

    pub fn agent_get(&self, target: &str) -> Result<Option<AgentInfo>> {
        match self.rpc("agent.get", json!({ "target": target })) {
            Ok(result) => {
                let agent = result
                    .get("agent")
                    .cloned()
                    .ok_or_else(|| anyhow!("unexpected `agent get` response"))?;
                Ok(Some(
                    serde_json::from_value(agent).context("cannot parse agent info")?,
                ))
            }
            Err(err) if is_gone(&err) => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub fn agent_prompt(&self, target: &str, text: &str) -> Result<Value> {
        self.rpc("agent.prompt", json!({ "target": target, "text": text }))
    }

    /// Re-read `config.toml` in the running server, so a key written by
    /// `setup-keys` works without restarting Herdr.
    pub fn reload_config(&self) -> Result<()> {
        self.rpc("server.reload_config", json!({})).map(|_| ())
    }

    pub fn agent_focus(&self, target: &str) -> Result<()> {
        self.rpc("agent.focus", json!({ "target": target }))
            .map(|_| ())
    }

    // ----- notifications --------------------------------------------------

    /// Best-effort toast in the Herdr UI; failures are ignored.
    pub fn notify(&self, title: &str, body: &str) {
        let title: String = title.chars().take(80).collect();
        let body: String = body.chars().take(240).collect();
        let _ = self.rpc("notification.show", json!({ "title": title, "body": body }));
    }
}

/// Session sockets flagged as running in a `session list --json` response.
pub fn parse_running_sessions(value: &Value) -> Vec<String> {
    let result = value.get("result").unwrap_or(value);
    result
        .get("sessions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|s| {
                    s.get("running").and_then(Value::as_bool).unwrap_or(false)
                        || s.get("status").and_then(Value::as_str) == Some("running")
                })
                .filter_map(|s| {
                    s.get("socket_path")
                        .or_else(|| s.get("socket"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn default_socket() -> PathBuf {
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    let base = config_home.join("herdr");
    match env::var("HERDR_SESSION") {
        Ok(name) if !name.is_empty() && name != "default" => {
            base.join("sessions").join(name).join("herdr.sock")
        }
        _ => base.join("herdr.sock"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gone_codes_are_recognised() {
        for code in ["not_found", "pane_not_found", "plugin_pane_not_found"] {
            let err = anyhow::Error::new(HerdrError {
                code: code.into(),
                message: String::new(),
            });
            assert!(is_gone(&err), "{code}");
        }
        let busy = anyhow::Error::new(HerdrError {
            code: "ui_busy".into(),
            message: "x".into(),
        });
        assert!(!is_gone(&busy));
        assert!(!is_gone(&anyhow!("transport failure")));
    }

    #[test]
    fn parses_running_sessions_from_both_shapes() {
        let value = json!({"result": {"sessions": [
            {"name": "default", "running": true, "socket_path": "/a/herdr.sock"},
            {"name": "old", "running": false, "socket_path": "/b/herdr.sock"},
            {"name": "alt", "status": "running", "socket": "/c/herdr.sock"}
        ]}});
        assert_eq!(
            parse_running_sessions(&value),
            vec!["/a/herdr.sock", "/c/herdr.sock"]
        );
        assert!(parse_running_sessions(&json!({})).is_empty());
    }
}
