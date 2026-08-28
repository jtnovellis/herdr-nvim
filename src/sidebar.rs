//! The per-tab Neovim sidebar: a full-height Herdr split pane running
//! `nvim --remote-ui` against the tab's headless daemon. Opening it parks the
//! tab's other panes in a temporary tab, splits the lone anchor, and rebuilds
//! the original arrangement inside the other half (layout planner adapted
//! from ChmaraX/herdr-nvim, MIT; see THIRD_PARTY.md).

use crate::config::{Config, Side};
use crate::context::{workspace_of_tab, Context};
use crate::daemon;
use crate::herdr::{Herdr, PaneInfo};
use crate::layout::{self, Dir};
use crate::state::{plugin_id, DaemonRecord, LayoutPhase, LayoutState, StateFile};
use anyhow::{anyhow, bail, Context as _, Result};
use serde_json::Value;
use std::env;
use std::io::{self, BufRead};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};

const ENTRYPOINT: &str = "sidebar";
pub const METADATA_SOURCE: &str = "plugin:herdr-nvim";
pub const PARKING_LABEL: &str = "nvim: restoring layout";

/// Pid of the `--remote-ui` client, for signal forwarding.
static CLIENT_PID: AtomicI32 = AtomicI32::new(0);

/// Herdr signals only the pane's own process when it closes the pane; pass
/// the signal on to the client so it exits, then finish our cleanup.
extern "C" fn forward_signal(signal: libc::c_int) {
    let pid = CLIENT_PID.load(Ordering::SeqCst);
    if pid > 0 {
        // SAFETY: kill is async-signal-safe; pid is our own child.
        unsafe {
            libc::kill(pid, signal);
        }
    }
}

/// What a recorded sidebar pane id turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ours,
    Gone,
    Foreign,
    Adopt,
}

pub fn decide(
    record: &DaemonRecord,
    pane: Option<&PaneInfo>,
    owned: impl FnOnce() -> bool,
) -> Verdict {
    let Some(pane) = pane else {
        return Verdict::Gone;
    };
    if pane.terminal_id.is_some() && pane.terminal_id == record.sidebar_terminal_id {
        return Verdict::Ours;
    }
    if owned() {
        Verdict::Adopt
    } else {
        Verdict::Foreign
    }
}

/// Re-validate a record's sidebar pane. Returns true when the record changed.
pub fn reconcile_sidebar(herdr: &Herdr, record: &mut DaemonRecord) -> bool {
    let Some(pane) = record.sidebar_pane_id.clone() else {
        return false;
    };
    let Ok(info) = herdr.pane_get(&pane) else {
        return false;
    };
    let socket = record.socket.to_string_lossy().into_owned();
    match decide(record, info.as_ref(), || {
        herdr.sidebar_owned(&pane, &socket)
    }) {
        Verdict::Ours => false,
        Verdict::Adopt => {
            record.sidebar_terminal_id = info.and_then(|i| i.terminal_id);
            true
        }
        Verdict::Gone | Verdict::Foreign => {
            clear_sidebar_fields(record);
            true
        }
    }
}

/// Forget the sidebar pane; a finished layout record goes with it, a
/// mid-open one is kept for recovery.
fn clear_sidebar_fields(record: &mut DaemonRecord) {
    record.sidebar_pane_id = None;
    record.sidebar_terminal_id = None;
    if record
        .layout
        .as_ref()
        .is_some_and(|l| l.phase == LayoutPhase::Open)
    {
        record.layout = None;
    }
}

fn set_layout(session: &str, tab: &str, layout: Option<LayoutState>) -> Result<()> {
    let file = StateFile::open()?;
    let mut state = file.load()?;
    if let Some(record) = state.get_mut(session, tab) {
        record.layout = layout;
        file.save(&state)?;
    }
    Ok(())
}

/// Outcome of `recover_all`.
#[derive(Debug, Default)]
pub struct Recovered {
    /// Tabs whose layout was rebuilt.
    pub tabs: Vec<String>,
    /// Parking tabs that were closed (or emptied) in the process.
    pub parking_tabs: Vec<String>,
}

/// Finish any interrupted layout maneuver of this session.
pub fn recover_all(herdr: &Herdr, session: &str) -> Result<Recovered> {
    let pending: Vec<(String, LayoutState)> = {
        let file = StateFile::open()?;
        let state = file.load()?;
        state
            .sessions
            .get(session)
            .map(|tabs| {
                tabs.iter()
                    .filter_map(|(tab, rec)| {
                        rec.layout
                            .clone()
                            .filter(|l| l.phase == LayoutPhase::Evacuating)
                            .map(|l| (tab.clone(), l))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = Recovered::default();
    for (tab, layout) in pending {
        let _guard = daemon::tab_lock(session, &tab)?;
        if let Some(parking) = &layout.parking_tab {
            out.parking_tabs.push(parking.clone());
        }
        match recover_tab(herdr, session, &tab, layout) {
            Ok(true) => out.tabs.push(tab),
            Ok(false) => {}
            Err(err) => eprintln!("warning: could not recover the layout of {tab}: {err:#}"),
        }
    }
    Ok(out)
}

fn pane_in_tab(herdr: &Herdr, pane: &str, tab: &str) -> Result<bool> {
    Ok(herdr
        .pane_get(pane)?
        .is_some_and(|info| info.tab_id.as_deref() == Some(tab)))
}

/// Replay the remaining moves of an interrupted open. Returns true when the
/// layout was rebuilt (false when the tab itself is gone).
fn recover_tab(herdr: &Herdr, session: &str, tab: &str, mut ls: LayoutState) -> Result<bool> {
    let Some(any_pane) = herdr.any_pane_in_tab(tab)? else {
        // Never lose panes: leave them where they are, forget the maneuver.
        if !ls.parked.is_empty() {
            herdr.notify(
                "herdr-nvim: layout not restored",
                &format!(
                    "tab {tab} is gone; {} pane(s) left in tab {}",
                    ls.parked.len(),
                    ls.parking_tab.as_deref().unwrap_or("?")
                ),
            );
        }
        set_layout(session, tab, None)?;
        return Ok(false);
    };
    if herdr.tab_layout(&any_pane)?.zoomed {
        herdr.pane_zoom_off(&any_pane)?;
    }

    for step in ls.steps.clone() {
        if !ls.parked.iter().any(|p| p == &step.pane) {
            continue;
        }
        if herdr.pane_get(&step.pane)?.is_none() {
            ls.parked.retain(|p| p != &step.pane);
            set_layout(session, tab, Some(ls.clone()))?;
            continue;
        }
        let target = if pane_in_tab(herdr, &step.target, tab)? {
            Some(step.target.as_str())
        } else if pane_in_tab(herdr, &ls.anchor, tab)? {
            Some(ls.anchor.as_str())
        } else {
            None
        };
        herdr.pane_move(&step.pane, tab, step.dir, target, Some(step.ratio))?;
        ls.parked.retain(|p| p != &step.pane);
        set_layout(session, tab, Some(ls.clone()))?;
    }
    // Anything parked that no step covers (should not happen): bring it back.
    for pane in ls.parked.clone() {
        if herdr.pane_get(&pane)?.is_some() {
            herdr.pane_move(&pane, tab, Dir::Right, None, None)?;
        }
        ls.parked.retain(|p| p != &pane);
        set_layout(session, tab, Some(ls.clone()))?;
    }
    if let Some(placeholder) = ls.parking_placeholder.take() {
        if herdr.pane_get(&placeholder)?.is_some() {
            let _ = herdr.pane_close(&placeholder);
        }
    }
    ls.parking_tab = None;
    ls.phase = LayoutPhase::Open;

    // Keep the record only if the sidebar pane is still around.
    let sidebar_alive = {
        let file = StateFile::open()?;
        let state = file.load()?;
        match state
            .get(session, tab)
            .and_then(|r| r.sidebar_pane_id.clone())
        {
            Some(pane) => herdr.pane_get(&pane)?.is_some(),
            None => false,
        }
    };
    set_layout(session, tab, sidebar_alive.then_some(ls))?;
    Ok(true)
}

pub struct Host {
    pub cfg: Config,
    pub herdr: Herdr,
    pub ctx: Context,
    pub tab_id: String,
    pub session: String,
}

impl Host {
    pub fn new() -> Result<Host> {
        let cfg = Config::load();
        let herdr = Herdr::from_env();
        let ctx = Context::from_env();
        let tab_id = ctx.tab_id.clone().ok_or_else(|| {
            anyhow!("no tab context: run this from a Herdr pane or plugin action (HERDR_TAB_ID is not set)")
        })?;
        let session = herdr.session_key();
        Ok(Host {
            cfg,
            herdr,
            ctx,
            tab_id,
            session,
        })
    }

    pub fn workspace_id(&self) -> String {
        self.ctx
            .workspace_id
            .clone()
            .or_else(|| workspace_of_tab(&self.tab_id))
            .unwrap_or_default()
    }

    pub fn record(&self) -> Result<Option<DaemonRecord>> {
        let file = StateFile::open()?;
        let state = file.load()?;
        Ok(state.get(&self.session, &self.tab_id).cloned())
    }

    /// Finish interrupted maneuvers first. Errors when this tab was a parking
    /// tab (it no longer exists after recovery).
    pub fn prepare(&self) -> Result<Recovered> {
        let recovered = recover_all(&self.herdr, &self.session)?;
        if recovered.parking_tabs.contains(&self.tab_id) {
            bail!(
                "tab {} was a temporary parking tab and has been restored into its original tab",
                self.tab_id
            );
        }
        Ok(recovered)
    }

    /// The sidebar pane recorded for this tab, if it still exists and is ours.
    pub fn existing_sidebar(&self) -> Result<Option<String>> {
        let Some(record) = self.record()? else {
            return Ok(None);
        };
        let Some(pane) = record.sidebar_pane_id.clone() else {
            return Ok(None);
        };
        let info = self.herdr.pane_get(&pane)?;
        let socket = record.socket.to_string_lossy().into_owned();
        match decide(&record, info.as_ref(), || {
            self.herdr.sidebar_owned(&pane, &socket)
        }) {
            Verdict::Ours => Ok(Some(pane)),
            Verdict::Adopt => {
                let terminal = info.and_then(|i| i.terminal_id);
                self.set_sidebar_record(Some(&pane), terminal.as_deref(), Some(&pane))?;
                Ok(Some(pane))
            }
            Verdict::Gone | Verdict::Foreign => {
                self.set_sidebar_record(None, None, Some(&pane))?;
                Ok(None)
            }
        }
    }

    pub fn set_sidebar_record(
        &self,
        pane: Option<&str>,
        terminal: Option<&str>,
        only_if: Option<&str>,
    ) -> Result<()> {
        let file = StateFile::open()?;
        let mut state = file.load()?;
        if let Some(record) = state.get_mut(&self.session, &self.tab_id) {
            if only_if.is_none() || record.sidebar_pane_id.as_deref() == only_if {
                match pane {
                    Some(pane) => {
                        record.sidebar_pane_id = Some(pane.to_string());
                        record.sidebar_terminal_id = terminal.map(str::to_string);
                    }
                    None => clear_sidebar_fields(record),
                }
                file.save(&state)?;
            }
        }
        Ok(())
    }

    pub fn ensure_daemon(&self) -> Result<DaemonRecord> {
        let cwd = self.ctx.resolve_cwd(&self.herdr);
        let (record, spawned) = daemon::ensure(
            &self.cfg,
            &self.herdr,
            &self.session,
            &self.tab_id,
            &cwd,
            self.ctx.workspace_id.as_deref(),
        )?;
        if spawned {
            eprintln!(
                "started nvim daemon for {} (pid {}, {})",
                self.tab_id,
                record.pid,
                record.socket.display()
            );
        }
        Ok(record)
    }

    /// Open the sidebar, or return the existing one (focusing it when asked).
    pub fn ensure_open(&self, focus: bool) -> Result<(String, bool)> {
        self.prepare()?;
        if let Some(pane) = self.existing_sidebar()? {
            if focus {
                self.herdr.plugin_pane_focus(&pane)?;
            }
            return Ok((pane, false));
        }
        Ok((self.open_sidebar()?, true))
    }

    fn launch_sidebar_pane(&self, record: &DaemonRecord, target: Option<&str>) -> Result<String> {
        let envs = vec![
            (
                "HERDR_NVIM_SOCKET".to_string(),
                record.socket.to_string_lossy().into_owned(),
            ),
            ("HERDR_NVIM_TAB_ID".to_string(), self.tab_id.clone()),
        ];
        let info = self
            .herdr
            .plugin_pane_open(&plugin_id(), ENTRYPOINT, target, None, &envs, true)
            .context("cannot open sidebar pane")?;
        let pane = info.pane_id.clone();
        self.set_sidebar_record(Some(&pane), info.terminal_id.as_deref(), None)?;
        Ok(pane)
    }

    /// Full-height open: park, split the anchor, rebuild beside the sidebar.
    pub fn open_sidebar(&self) -> Result<String> {
        let record = self.ensure_daemon()?;
        let _guard = daemon::tab_lock(&self.session, &self.tab_id)?;

        let probe = match &self.ctx.pane_id {
            Some(p) if pane_in_tab(&self.herdr, p, &self.tab_id)? => p.clone(),
            _ => self
                .herdr
                .any_pane_in_tab(&self.tab_id)?
                .ok_or_else(|| anyhow!("tab {} has no panes", self.tab_id))?,
        };
        let mut tab_layout = self.herdr.tab_layout(&probe)?;
        if tab_layout.zoomed {
            self.herdr.pane_zoom_off(&probe)?;
            tab_layout = self.herdr.tab_layout(&probe)?;
            if tab_layout.zoomed {
                bail!("the tab is zoomed; unzoom it first");
            }
        }
        let plan = match layout::plan_rebuild(&tab_layout.rects) {
            Ok(plan) => plan,
            Err(err) => {
                eprintln!("warning: {err:#}; opening beside the focused pane instead");
                self.herdr.notify(
                    "herdr-nvim",
                    "layout could not be planned; opening the sidebar beside the focused pane",
                );
                return self.open_simple(&record);
            }
        };

        let anchor = plan.anchor.clone();
        let parked: Vec<String> = tab_layout
            .rects
            .iter()
            .filter(|r| r.pane_id != anchor)
            .map(|r| r.pane_id.clone())
            .collect();
        let mut ls = LayoutState {
            phase: LayoutPhase::Evacuating,
            anchor: anchor.clone(),
            parking_tab: None,
            parking_placeholder: None,
            parked: parked.clone(),
            steps: plan.steps,
        };
        if !parked.is_empty() {
            let (parking_tab, placeholder) =
                self.herdr.tab_create(&self.workspace_id(), PARKING_LABEL)?;
            ls.parking_tab = Some(parking_tab.clone());
            ls.parking_placeholder = Some(placeholder);
            set_layout(&self.session, &self.tab_id, Some(ls.clone()))?;
            for pane in &parked {
                self.herdr
                    .pane_move(pane, &parking_tab, Dir::Right, None, None)?;
            }
        } else {
            set_layout(&self.session, &self.tab_id, Some(ls.clone()))?;
        }

        let sidebar = self.launch_sidebar_pane(&record, Some(&anchor))?;
        if self.cfg.side == Side::Left {
            if let Err(err) = self.herdr.pane_swap(&anchor, &sidebar) {
                eprintln!("warning: could not move the sidebar to the left: {err:#}");
            }
        }
        if let Err(err) = self.set_width(&sidebar) {
            eprintln!("warning: could not set the sidebar width: {err:#}");
        }

        for step in ls.steps.clone() {
            if !ls.parked.iter().any(|p| p == &step.pane) {
                continue;
            }
            self.herdr.pane_move(
                &step.pane,
                &self.tab_id,
                step.dir,
                Some(&step.target),
                Some(step.ratio),
            )?;
            ls.parked.retain(|p| p != &step.pane);
            set_layout(&self.session, &self.tab_id, Some(ls.clone()))?;
        }
        if let Some(placeholder) = ls.parking_placeholder.take() {
            if self.herdr.pane_close(&placeholder).is_err() {
                if let Some(tab) = &ls.parking_tab {
                    let _ = self.herdr.tab_close(tab);
                }
            }
        }
        ls.parking_tab = None;
        ls.parked.clear();
        ls.phase = LayoutPhase::Open;
        set_layout(&self.session, &self.tab_id, Some(ls))?;

        if let Err(err) = self.herdr.plugin_pane_focus(&sidebar) {
            eprintln!("warning: could not focus the sidebar: {err:#}");
        }
        Ok(sidebar)
    }

    /// Fallback: a plain split beside the focused pane (no layout record).
    fn open_simple(&self, record: &DaemonRecord) -> Result<String> {
        let target = self.ctx.pane_id.as_deref();
        let pane = self.launch_sidebar_pane(record, target)?;
        if self.cfg.side == Side::Left {
            if let Some(target) = target {
                if let Err(err) = self.herdr.pane_swap(target, &pane) {
                    eprintln!("warning: could not move the sidebar to the left: {err:#}");
                }
            }
        }
        if let Err(err) = self.set_width(&pane) {
            eprintln!("warning: could not set the sidebar width: {err:#}");
        }
        let _ = self.herdr.plugin_pane_focus(&pane);
        Ok(pane)
    }

    fn set_width(&self, pane: &str) -> Result<()> {
        let layout = self.herdr.layout_export(pane)?;
        let root = layout
            .get("root")
            .ok_or_else(|| anyhow!("layout.export returned no root"))?;
        let tab_id = layout
            .get("tab_id")
            .and_then(Value::as_str)
            .unwrap_or(&self.tab_id);
        let Some(parent) = find_parent_split(root, pane, Vec::new()) else {
            return Ok(());
        };
        if parent.direction != "right" {
            return Ok(());
        }
        let ratio = if parent.pane_is_first {
            self.cfg.width
        } else {
            1.0 - self.cfg.width
        };
        self.herdr
            .layout_set_split_ratio(tab_id, &parent.path, ratio)
    }

    pub fn close_sidebar(&self, pane: &str) -> Result<()> {
        self.herdr.pane_close(pane)?;
        self.set_sidebar_record(None, None, Some(pane))
    }
}

struct ParentSplit {
    path: Vec<bool>,
    direction: String,
    pane_is_first: bool,
}

fn is_pane(node: &Value, pane_id: &str) -> bool {
    node.get("type").and_then(Value::as_str) == Some("pane")
        && node.get("pane_id").and_then(Value::as_str) == Some(pane_id)
}

fn find_parent_split(node: &Value, pane_id: &str, path: Vec<bool>) -> Option<ParentSplit> {
    if node.get("type").and_then(Value::as_str) != Some("split") {
        return None;
    }
    let first = node.get("first")?;
    let second = node.get("second")?;
    let direction = node
        .get("direction")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if is_pane(first, pane_id) {
        return Some(ParentSplit {
            path,
            direction,
            pane_is_first: true,
        });
    }
    if is_pane(second, pane_id) {
        return Some(ParentSplit {
            path,
            direction,
            pane_is_first: false,
        });
    }
    let mut first_path = path.clone();
    first_path.push(false);
    if let Some(found) = find_parent_split(first, pane_id, first_path) {
        return Some(found);
    }
    let mut second_path = path;
    second_path.push(true);
    find_parent_split(second, pane_id, second_path)
}

pub fn toggle() -> Result<i32> {
    let host = Host::new()?;
    let recovered = recover_all(&host.herdr, &host.session)?;
    if recovered.tabs.contains(&host.tab_id) {
        println!("restored the sidebar layout of {}", host.tab_id);
        return Ok(0);
    }
    if recovered.parking_tabs.contains(&host.tab_id) {
        println!(
            "tab {} was a temporary parking tab; its panes are back in their tab",
            host.tab_id
        );
        return Ok(0);
    }
    if let Some(pane) = host.existing_sidebar()? {
        host.close_sidebar(&pane)?;
        println!("hid Neovim sidebar {pane}; its daemon keeps running");
    } else {
        let pane = host.open_sidebar()?;
        println!("opened Neovim sidebar {pane}");
    }
    Ok(0)
}

pub fn open() -> Result<i32> {
    let host = Host::new()?;
    let (pane, opened) = host.ensure_open(true)?;
    if opened {
        println!("opened Neovim sidebar {pane}");
    } else {
        println!("focused Neovim sidebar {pane}");
    }
    Ok(0)
}

pub fn close() -> Result<i32> {
    let host = Host::new()?;
    host.prepare()?;
    if let Some(pane) = host.existing_sidebar()? {
        host.close_sidebar(&pane)?;
        println!("hid Neovim sidebar {pane}; its daemon keeps running");
    } else {
        println!("no Neovim sidebar is open in {}", host.tab_id);
    }
    Ok(0)
}

/// `herdr-nvim title <text>`: display the current file on the sidebar pane.
pub fn title(args: &[String]) -> Result<i32> {
    let text = args.join(" ");
    let host = Host::new()?;
    let Some(pane) = host.record()?.and_then(|r| r.sidebar_pane_id) else {
        return Ok(0);
    };
    let title = (!text.trim().is_empty()).then_some(text.as_str());
    match host.herdr.pane_set_title(&pane, METADATA_SOURCE, title) {
        Ok(()) => Ok(0),
        Err(err) => {
            eprintln!("herdr-nvim: could not set the pane title: {err:#}");
            Ok(0)
        }
    }
}

fn pause(message: &str) {
    eprintln!("herdr-nvim: {message}");
    eprintln!("press Enter to close this pane");
    let _ = io::stdin().lock().read_line(&mut String::new());
}

/// `[[panes]]` entrypoint: attach a UI client to this tab's daemon.
pub fn run_sidebar() -> Result<i32> {
    // SAFETY: the handler only calls async-signal-safe `kill`.
    unsafe {
        libc::signal(
            libc::SIGHUP,
            forward_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            forward_signal as *const () as libc::sighandler_t,
        );
    }
    let host = match Host::new() {
        Ok(host) => host,
        Err(err) => {
            pause(&format!("{err:#}"));
            return Ok(1);
        }
    };
    let my_pane = env::var("HERDR_PANE_ID").ok().filter(|p| !p.is_empty());

    let record = match host.ensure_daemon() {
        Ok(record) => record,
        Err(err) => {
            pause(&format!("cannot start nvim daemon: {err:#}"));
            return Ok(1);
        }
    };

    if let Some(pane) = &my_pane {
        let terminal = host
            .herdr
            .pane_get(pane)
            .ok()
            .flatten()
            .and_then(|info| info.terminal_id);
        if let Err(err) = host.set_sidebar_record(Some(pane), terminal.as_deref(), None) {
            pause(&format!("cannot record the sidebar pane: {err:#}"));
            return Ok(1);
        }
    }

    let status = Command::new(&host.cfg.nvim)
        .arg("--server")
        .arg(&record.socket)
        .arg("--remote-ui")
        .spawn()
        .and_then(|mut child| {
            CLIENT_PID.store(child.id() as i32, Ordering::SeqCst);
            let status = child.wait();
            CLIENT_PID.store(0, Ordering::SeqCst);
            status
        });

    let code = match status {
        Ok(status) if status.success() => 0,
        Ok(status) => {
            if status.code().is_some() {
                pause(&format!("nvim client exited with {status}"));
            }
            status.code().unwrap_or(1)
        }
        Err(err) => {
            pause(&format!(
                "cannot run `{} --remote-ui`: {err}",
                host.cfg.nvim
            ));
            1
        }
    };

    if let Some(pane) = &my_pane {
        let _ = host.set_sidebar_record(None, None, Some(pane));
        let _ = host
            .herdr
            .rpc("pane.close", serde_json::json!({ "pane_id": pane }));
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn record(terminal: Option<&str>) -> DaemonRecord {
        DaemonRecord {
            pid: 1,
            socket: PathBuf::from("/tmp/a.sock"),
            cwd: PathBuf::from("/tmp"),
            sidebar_pane_id: Some("w1:p2".into()),
            sidebar_terminal_id: terminal.map(str::to_string),
            workspace_id: None,
            ps_lstart: None,
            starting: false,
            started_unix: 0,
            layout: None,
        }
    }

    fn pane(terminal: &str) -> PaneInfo {
        PaneInfo {
            pane_id: "w1:p2".into(),
            terminal_id: Some(terminal.into()),
            tab_id: None,
            workspace_id: None,
            cwd: None,
            foreground_cwd: None,
            label: None,
            title: None,
            focused: false,
            agent_session: None,
            scroll: None,
        }
    }

    #[test]
    fn identity_decision() {
        assert_eq!(decide(&record(Some("t1")), None, || true), Verdict::Gone);
        assert_eq!(
            decide(&record(Some("t1")), Some(&pane("t1")), || false),
            Verdict::Ours
        );
        assert_eq!(
            decide(&record(Some("t1")), Some(&pane("t2")), || false),
            Verdict::Foreign
        );
        assert_eq!(
            decide(&record(None), Some(&pane("t9")), || true),
            Verdict::Adopt
        );
        assert_eq!(
            decide(&record(None), Some(&pane("t9")), || false),
            Verdict::Foreign
        );
    }

    #[test]
    fn clearing_the_sidebar_drops_only_finished_layouts() {
        let layout = |phase| LayoutState {
            phase,
            anchor: "w1:p1".into(),
            parking_tab: None,
            parking_placeholder: None,
            parked: vec![],
            steps: vec![],
        };
        let mut open = record(Some("t1"));
        open.layout = Some(layout(LayoutPhase::Open));
        clear_sidebar_fields(&mut open);
        assert!(open.layout.is_none() && open.sidebar_pane_id.is_none());

        let mut evacuating = record(Some("t1"));
        evacuating.layout = Some(layout(LayoutPhase::Evacuating));
        clear_sidebar_fields(&mut evacuating);
        assert!(evacuating.layout.is_some(), "recovery info must survive");
    }

    #[test]
    fn finds_path_to_parent_split() {
        let root = json!({
            "type": "split", "direction": "right", "ratio": 0.5,
            "first": {"type": "pane", "pane_id": "w1:p1"},
            "second": {
                "type": "split", "direction": "down", "ratio": 0.5,
                "first": {"type": "pane", "pane_id": "w1:p2"},
                "second": {
                    "type": "split", "direction": "right", "ratio": 0.5,
                    "first": {"type": "pane", "pane_id": "w1:p3"},
                    "second": {"type": "pane", "pane_id": "w1:p4"}
                }
            }
        });
        let found = find_parent_split(&root, "w1:p4", Vec::new()).unwrap();
        assert_eq!(found.path, vec![true, true]);
        assert_eq!(found.direction, "right");
        assert!(!found.pane_is_first);
        let found = find_parent_split(&root, "w1:p1", Vec::new()).unwrap();
        assert!(found.path.is_empty());
        assert!(found.pane_is_first);
        assert!(find_parent_split(&root, "w1:p9", Vec::new()).is_none());
    }
}
