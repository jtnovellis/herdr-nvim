//! Daemon registry: one JSON file under the plugin state directory, guarded
//! by an advisory lock so concurrent plugin commands do not clobber each other.

use crate::config::Config;
use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const PLUGIN_ID: &str = "herdr-nvim";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonRecord {
    pub pid: u32,
    pub socket: PathBuf,
    pub cwd: PathBuf,
    #[serde(default)]
    pub sidebar_pane_id: Option<String>,
    /// `PaneInfo.terminal_id` of the sidebar pane. Herdr never persists it
    /// across restarts, so a restored pane with a reused id fails this check.
    #[serde(default)]
    pub sidebar_terminal_id: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// `ps -o lstart=` of the daemon captured right after spawn; guards
    /// against pid reuse before we ever signal a pid.
    #[serde(default)]
    pub ps_lstart: Option<String>,
    /// Reservation placeholder while a daemon is being started.
    #[serde(default)]
    pub starting: bool,
    #[serde(default)]
    pub started_unix: u64,
    /// Set while a full-height sidebar is open (or mid-open, for recovery).
    #[serde(default)]
    pub layout: Option<LayoutState>,
}

impl DaemonRecord {
    pub fn age_secs(&self) -> u64 {
        now_unix().saturating_sub(self.started_unix)
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where a full-height sidebar open is: `Evacuating` while panes are parked
/// in a temporary tab, `Open` once the layout is rebuilt beside the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutPhase {
    Evacuating,
    Open,
}

/// Crash-recovery record for the layout maneuver behind a sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutState {
    pub phase: LayoutPhase,
    /// The pane that stayed in the tab; the rebuilt half hangs off it.
    pub anchor: String,
    #[serde(default)]
    pub parking_tab: Option<String>,
    #[serde(default)]
    pub parking_placeholder: Option<String>,
    /// Panes still sitting in the parking tab.
    #[serde(default)]
    pub parked: Vec<String>,
    #[serde(default)]
    pub steps: Vec<crate::layout::MoveStep>,
}

/// `sessions[session_key][tab_id] -> daemon`
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub sessions: BTreeMap<String, BTreeMap<String, DaemonRecord>>,
}

impl State {
    pub fn get(&self, session: &str, tab_id: &str) -> Option<&DaemonRecord> {
        self.sessions.get(session).and_then(|tabs| tabs.get(tab_id))
    }

    pub fn get_mut(&mut self, session: &str, tab_id: &str) -> Option<&mut DaemonRecord> {
        self.sessions
            .get_mut(session)
            .and_then(|tabs| tabs.get_mut(tab_id))
    }

    pub fn insert(&mut self, session: &str, tab_id: &str, record: DaemonRecord) {
        self.sessions
            .entry(session.to_string())
            .or_default()
            .insert(tab_id.to_string(), record);
    }

    pub fn remove(&mut self, session: &str, tab_id: &str) -> Option<DaemonRecord> {
        let removed = self
            .sessions
            .get_mut(session)
            .and_then(|tabs| tabs.remove(tab_id));
        self.prune();
        removed
    }

    pub fn tabs_of(&self, session: &str) -> Vec<String> {
        self.sessions
            .get(session)
            .map(|tabs| tabs.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn prune(&mut self) {
        self.sessions.retain(|_, tabs| !tabs.is_empty());
    }
}

/// Holds the state lock for as long as it lives. Never keep one across a
/// daemon spawn, stop, or socket wait.
pub struct StateFile {
    path: PathBuf,
    _lock: File,
}

fn lock_timeout() -> Duration {
    // The environment is re-read on every call so a one-off run (or a test)
    // can override it mid-process. Otherwise fall back to config.env, which is
    // the only place the knob is documented -- and which used to be ignored
    // here, because Config::load() parses the file into a map without
    // exporting it, and Herdr does not forward the user's environment through
    // `plugin action invoke`. Resolved once: this is on the state-lock path.
    if let Some(ms) = env::var("HERDR_NVIM_LOCK_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return Duration::from_millis(ms.clamp(100, 60_000));
    }
    static FROM_FILE: OnceLock<u64> = OnceLock::new();
    Duration::from_millis(*FROM_FILE.get_or_init(|| Config::load().lock_timeout_ms))
}

impl StateFile {
    pub fn open() -> Result<StateFile> {
        let dir = state_dir();
        fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create state dir {}", dir.display()))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("daemons.lock"))
            .context("cannot open state lock")?;
        let deadline = Instant::now() + lock_timeout();
        loop {
            // SAFETY: flock on a valid, owned file descriptor.
            let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                break;
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
                bail!("cannot lock state file: {err}");
            }
            if Instant::now() >= deadline {
                bail!("state is locked by another herdr-nvim command; try again");
            }
            sleep(Duration::from_millis(25));
        }
        Ok(StateFile {
            path: dir.join("daemons.json"),
            _lock: lock,
        })
    }

    pub fn load(&self) -> Result<State> {
        match fs::read(&self.path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(state) => Ok(state),
                Err(err) => {
                    let quarantine = self
                        .path
                        .with_extension(format!("json.corrupt-{}", now_unix()));
                    eprintln!(
                        "warning: {} is not valid JSON ({err}); moving it to {}",
                        self.path.display(),
                        quarantine.display()
                    );
                    let _ = fs::rename(&self.path, &quarantine);
                    Ok(State::default())
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(State::default()),
            Err(err) => Err(err).with_context(|| format!("cannot read {}", self.path.display())),
        }
    }

    pub fn save(&self, state: &State) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(state)?;
        {
            let mut file =
                File::create(&tmp).with_context(|| format!("cannot write {}", tmp.display()))?;
            io::Write::write_all(&mut file, &bytes)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("cannot replace {}", self.path.display()))?;
        Ok(())
    }
}

/// Serialises tests that touch process-global environment variables.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn plugin_id() -> String {
    env::var("HERDR_PLUGIN_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| PLUGIN_ID.to_string())
}

fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Herdr's own layout for plugin state, so the registry is shared between
/// plugin invocations (which get `HERDR_PLUGIN_STATE_DIR`) and calls from
/// ordinary panes (which do not).
pub fn state_dir() -> PathBuf {
    if let Some(dir) = env_path("HERDR_PLUGIN_STATE_DIR") {
        return dir;
    }
    if let Some(dir) = env_path("HERDR_NVIM_STATE_DIR") {
        return dir;
    }
    let base = env_path("XDG_STATE_HOME")
        .or_else(|| env_path("HOME").map(|h| h.join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("herdr").join("plugins").join(plugin_id())
}

pub fn print_status() -> Result<i32> {
    let file = StateFile::open()?;
    let state = file.load()?;
    let mut out = serde_json::Map::new();
    out.insert(
        "state_file".into(),
        serde_json::Value::String(state_dir().join("daemons.json").to_string_lossy().into()),
    );
    let mut sessions = serde_json::Map::new();
    // One `ps` covering every record, rather than one fork per daemon.
    let ps = crate::daemon::ps_snapshot(state.sessions.values().flat_map(|tabs| tabs.values()));
    for (session, tabs) in &state.sessions {
        let mut entries = serde_json::Map::new();
        for (tab, record) in tabs {
            let mut value = serde_json::to_value(record)?;
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "running".into(),
                    serde_json::Value::Bool(crate::daemon::is_running_with(
                        record,
                        ps.get(&record.pid).map(String::as_str),
                    )),
                );
            }
            entries.insert(tab.clone(), value);
        }
        sessions.insert(session.clone(), serde_json::Value::Object(entries));
    }
    out.insert("sessions".into(), serde_json::Value::Object(sessions));
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(pid: u32) -> DaemonRecord {
        DaemonRecord {
            pid,
            socket: PathBuf::from("/tmp/x.sock"),
            cwd: PathBuf::from("/tmp"),
            sidebar_pane_id: None,
            sidebar_terminal_id: None,
            workspace_id: None,
            ps_lstart: None,
            starting: false,
            started_unix: 0,
            layout: None,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("herdr-nvim-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn insert_get_remove_prunes_empty_sessions() {
        let mut state = State::default();
        state.insert("s1", "w1:t1", record(1));
        state.insert("s1", "w1:t2", record(2));
        assert_eq!(state.get("s1", "w1:t1").map(|r| r.pid), Some(1));
        assert_eq!(state.tabs_of("s1"), vec!["w1:t1", "w1:t2"]);
        assert!(state.remove("s1", "w1:t1").is_some());
        assert!(state.remove("s1", "w1:t2").is_some());
        assert!(state.sessions.is_empty());
        assert!(state.remove("s1", "w1:t9").is_none());
    }

    #[test]
    fn old_records_without_new_fields_still_parse() {
        let json = r#"{"sessions":{"s":{"w1:t1":{"pid":42,"socket":"/tmp/a.sock","cwd":"/tmp","sidebar_pane_id":"w1:p2","started_unix":5}}}}"#;
        let back: State = serde_json::from_str(json).unwrap();
        let rec = back.get("s", "w1:t1").unwrap();
        assert_eq!(rec.pid, 42);
        assert_eq!(rec.sidebar_terminal_id, None);
        assert!(!rec.starting);
    }

    #[test]
    fn corrupt_file_is_quarantined_and_lock_times_out() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("state");
        env::set_var("HERDR_NVIM_STATE_DIR", &dir);
        env::remove_var("HERDR_PLUGIN_STATE_DIR");
        fs::write(dir.join("daemons.json"), b"{not json").unwrap();

        let file = StateFile::open().unwrap();
        let state = file.load().unwrap();
        assert!(state.sessions.is_empty());
        assert!(!dir.join("daemons.json").exists());
        assert!(fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("corrupt")));

        // A second opener must time out while the first lock is held.
        env::set_var("HERDR_NVIM_LOCK_TIMEOUT_MS", "150");
        let started = Instant::now();
        let err = StateFile::open().err().expect("lock should be busy");
        assert!(err.to_string().contains("locked"));
        assert!(started.elapsed() >= Duration::from_millis(100));
        drop(file);
        assert!(StateFile::open().is_ok());
        env::remove_var("HERDR_NVIM_LOCK_TIMEOUT_MS");
        env::remove_var("HERDR_NVIM_STATE_DIR");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_dir_fallback_order() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved: Vec<(String, Option<std::ffi::OsString>)> = [
            "HERDR_PLUGIN_STATE_DIR",
            "HERDR_NVIM_STATE_DIR",
            "XDG_STATE_HOME",
            "HERDR_PLUGIN_ID",
        ]
        .iter()
        .map(|k| (k.to_string(), env::var_os(k)))
        .collect();
        env::remove_var("HERDR_PLUGIN_ID");
        env::set_var("XDG_STATE_HOME", "/xdg");
        env::remove_var("HERDR_PLUGIN_STATE_DIR");
        env::remove_var("HERDR_NVIM_STATE_DIR");
        assert_eq!(state_dir(), PathBuf::from("/xdg/herdr/plugins/herdr-nvim"));
        env::set_var("HERDR_NVIM_STATE_DIR", "/override");
        assert_eq!(state_dir(), PathBuf::from("/override"));
        env::set_var("HERDR_PLUGIN_STATE_DIR", "/plugin");
        assert_eq!(state_dir(), PathBuf::from("/plugin"));
        for (key, value) in saved {
            match value {
                Some(v) => env::set_var(&key, v),
                None => env::remove_var(&key),
            }
        }
    }
}
