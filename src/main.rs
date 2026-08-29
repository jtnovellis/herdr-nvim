//! herdr-nvim: Neovim integration for Herdr.
//!
//! One binary serves every manifest entrypoint of the plugin:
//!
//! * `toggle` / `open` / `close` / `edit` — actions that manage the per-tab sidebar
//! * `sidebar`                            — the pane command that attaches a UI client
//! * `event` / `startup` / `gc`           — daemon lifecycle (tabs closing, restarts)
//! * `ask` / `send` / `agents` / `title`    — called by the Lua plugin

mod ask;
mod candidates;
mod config;
mod context;
mod daemon;
mod edit;
mod extract;
mod git;
mod gitscan;
mod herdr;
mod layout;
mod msgpack;
mod pick;
mod send;
mod sessions;
mod setup;
mod sidebar;
mod state;
mod tail;

use anyhow::{bail, Result};

/// Commands whose failures should surface as a Herdr toast: they are
/// triggered from Herdr itself, where nobody reads stderr.
const UI_COMMANDS: &[&str] = &[
    "toggle",
    "open",
    "close",
    "edit",
    "pick-file",
    "sidebar",
    "event",
    "startup",
    "gc",
    "setup-keys",
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("herdr-nvim: {err:#}");
            let cmd = args.first().map(String::as_str).unwrap_or("");
            if UI_COMMANDS.contains(&cmd) && std::env::var_os("HERDR_ENV").is_some() {
                let first_line = format!("{err:#}");
                let first_line = first_line.lines().next().unwrap_or("error");
                herdr::Herdr::from_env().notify(&format!("herdr-nvim: {cmd} failed"), first_line);
            }
            1
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<i32> {
    let Some(cmd) = args.first() else {
        print_usage();
        return Ok(2);
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "toggle" => sidebar::toggle(),
        "open" => sidebar::open(),
        "close" => sidebar::close(),
        "edit" => edit::edit(rest),
        "pick-file" => pick::pick_file(rest),
        "title" => sidebar::title(rest),
        "sidebar" => sidebar::run_sidebar(),
        "event" => daemon::handle_event(),
        "startup" => {
            setup::ensure_config_env();
            daemon::gc()
        }
        "gc" => daemon::gc(),
        "send" => send::send(rest),
        "ask" => ask::ask(rest),
        "agents" => send::list_agents(rest),
        "tail" => tail::tail(rest),
        "setup-keys" => setup::setup_keys(rest),
        "status" => state::print_status(),
        "version" | "--version" | "-V" => {
            println!("herdr-nvim {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(0)
        }
        other => bail!("unknown command `{other}` (try `herdr-nvim help`)"),
    }
}

fn print_usage() {
    println!(
        "\
herdr-nvim {version}

Usage: herdr-nvim <command> [options]

Sidebar (herdr actions):
  toggle              Show or hide this tab's Neovim sidebar
  open                Open the sidebar, or focus it when already open
  close               Hide the sidebar; the daemon keeps running
  edit <file[:line[:col]]> [--no-focus]
                      Open a file in this tab's sidebar (from any pane)
  pick-file [--json] [--target T]
                      Fuzzy-pick a file the agent touched (or any repo file)
                      and open it in the sidebar; --json prints the list
  sidebar             Pane entrypoint: attach a UI client to the tab's daemon

Daemons:
  event               Event hook: stop daemons of closed tabs/workspaces
  startup | gc        Stop orphaned daemons, forget dead ones, re-check sidebars
  setup-keys [--force]
                      Bind prefix+e/prefix+f to the sidebar in your Herdr
                      config; backs it up first and never takes a bound key
  status              Print daemon state as JSON

Agents (used by the Neovim plugin):
  agents              List agents visible from here as JSON
  ask [--target T] [--force] [--focus] [--file PATH] [--dry-run] [--paste]
                      Read one message (plus an optional code selection) as
                      JSON from stdin and send it to an agent
  send [--submit|--paste] [--target T] [--force] [--focus] [--file PATH] [--dry-run]
                      Read annotations JSON from stdin (or --file) and paste
                      them into an agent's input; --submit presses Enter
  title [TEXT]        Show TEXT as the sidebar pane title (empty clears it)
  tail --path P [--agent KIND] [--from BYTES]
                      Print what the agent said and edited past BYTES in its
                      transcript, as JSON",
        version = env!("CARGO_PKG_VERSION")
    );
}
