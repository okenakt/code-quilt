//! `cq`: CodeQuilt command line. A single binary provides the user commands, the per-session
//! host, and the hook/status line endpoints invoked by agents.

mod agent;
mod attach;
mod cli;
mod config;
mod daemon;
mod hook;
mod host;
mod ipc;
mod launch;
mod machine;
mod mcp;
mod paths;
mod plugin;
mod remote;
mod session;
mod term;
mod usage;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use cq_proto::{LOCAL_MACHINE, SessionStatus, Size};
use std::ffi::OsString;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "cq", version, about = "CodeQuilt: orchestrate parallel coding agent sessions")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run claude inside a detachable CodeQuilt session. `--cq-name NAME` names the session
    /// (unique among running ones), `--cq-settings FILE` adds hooks; other arguments go to claude.
    #[command(disable_help_flag = true)]
    Claude {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run codex inside a detachable CodeQuilt session (same `--cq-*` options as `claude`).
    #[command(disable_help_flag = true)]
    Codex {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Attach this terminal to a running session (name, id, or unique id prefix). Ctrl+\ detaches.
    #[command(visible_alias = "a")]
    Attach { id: String },
    /// List sessions.
    Ls,
    /// Show rate limits per account and tokens per running session (all machines).
    Usage {
        /// Only this machine (no ssh to remote machines).
        #[arg(long)]
        local: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Name a running session (`[machine:]name-or-id`); `--clear` removes the name.
    Rename {
        target: String,
        name: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// Type text into a session once it is idle and nobody is typing.
    Inject {
        target: String,
        text: String,
        /// Press Enter after pasting.
        #[arg(long)]
        submit: bool,
        /// Required quiet time of terminal input, in milliseconds.
        #[arg(long, default_value_t = 3000)]
        quiet_ms: u64,
    },
    /// End a running session (`[machine:]name-or-id`): ask the agent to exit, kill it if it does
    /// not exit within the grace period.
    Kill {
        target: String,
        /// Kill right away without asking the agent to exit.
        #[arg(long)]
        force: bool,
        /// Grace period in seconds before the agent is killed.
        #[arg(long, default_value_t = 10)]
        grace: u64,
    },
    /// Run a command declared by a plugin (as the command palette does).
    Run {
        /// Command name, e.g. `queen.start`.
        name: String,
        /// Command argument as NAME=VALUE (repeatable).
        #[arg(long = "arg", value_name = "NAME=VALUE")]
        args: Vec<String>,
    },
    /// Manage plugins and the commands they provide.
    Plugin {
        #[command(subcommand)]
        action: PluginCmd,
    },
    /// Manage remote machines shown in the dashboard.
    Machine {
        #[command(subcommand)]
        action: MachineCmd,
    },
    /// MCP server (stdio) exposing the sessions to an agent, e.g. an orchestrator.
    Mcp {
        /// Limit to the sessions of a machine: `machine:ID` (repeatable).
        #[arg(long)]
        scope: Vec<String>,
        /// Also act as a Claude Code channel that orchestrators can push messages into.
        #[arg(long)]
        channel: bool,
    },
    /// Run the machine daemon that relays sessions to dashboards (started automatically), or
    /// manage it. Sessions keep running when it stops.
    Daemon {
        #[command(subcommand)]
        action: Option<DaemonCmd>,
    },
    /// Relay stdin/stdout to the local daemon (for `ssh <host> cq bridge`).
    Bridge,
    /// Session host process (internal).
    #[command(hide = true)]
    Host {
        #[arg(long)]
        id: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        cwd: String,
        #[arg(long)]
        cols: u16,
        #[arg(long)]
        rows: u16,
        #[arg(long)]
        name: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Agent hook endpoint (internal).
    #[command(hide = true)]
    Hook,
    /// Developer aid: read raw terminal bytes on stdin and print what the screen adapters see.
    #[command(hide = true)]
    DebugScreen {
        #[arg(long, default_value_t = 120)]
        cols: u16,
        #[arg(long, default_value_t = 36)]
        rows: u16,
    },
    /// Agent status line endpoint (internal).
    #[command(hide = true)]
    Statusline,
    /// Any other name runs the plugin `cq-NAME` (e.g. `cq queen` runs cq-queen).
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Show whether the local daemon and the remote machines are running.
    Status,
    /// Stop a daemon.
    Stop {
        #[arg(long, default_value = LOCAL_MACHINE)]
        machine: String,
    },
    /// Stop a daemon and start it again (e.g. after updating cq).
    Restart {
        #[arg(long, default_value = LOCAL_MACHINE)]
        machine: String,
    },
}

#[derive(Subcommand)]
enum PluginCmd {
    /// List installed plugins and their commands (those next to cq are installed with it).
    List,
    /// Install `cq-NAME` from PATH and record the commands it declares.
    Add { name: String },
    /// Uninstall a plugin.
    Remove { name: String },
    /// Re-read the manifests of all installed plugins.
    Refresh,
}

#[derive(Subcommand)]
enum MachineCmd {
    /// List registered machines.
    List,
    /// Register a machine; without HOST, choose from ~/.ssh/config.
    Add {
        host: Option<String>,
        #[arg(long)]
        label: Option<String>,
        /// Path of `cq` on the remote machine (default: `cq` on PATH).
        #[arg(long)]
        cq_path: Option<String>,
        /// Run `cq bridge` through a login shell (Linux/macOS remotes whose PATH is set in ~/.profile).
        #[arg(long)]
        login_shell: bool,
    },
    /// Unregister a machine.
    Remove { id: String },
}

/// Takes `--cq-name NAME` and `--cq-settings FILE` out of the agent arguments.
fn split_labels(args: Vec<String>) -> (launch::Labels, Vec<String>) {
    let mut labels = launch::Labels::default();
    let mut rest = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cq-name" => labels.name = it.next(),
            "--cq-settings" => labels.settings_extra = it.next(),
            _ => rest.push(a),
        }
    }
    (labels, rest)
}

fn main() {
    let builtin = Cli::command();
    let args = plugin::expand_alias(std::env::args_os().collect(), |name| {
        name == "help" || builtin.find_subcommand(name).is_some()
    });
    let cli = Cli::parse_from(args);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
    let code = rt.block_on(async {
        match run(cli.command).await {
            Ok(code) => code,
            Err(e) => {
                eprintln!("cq: {e:#}");
                1
            }
        }
    });
    // Blocking console reader threads may still be parked in ReadConsoleW; exit explicitly.
    std::process::exit(code);
}

async fn run(cmd: Cmd) -> Result<i32> {
    match cmd {
        Cmd::Claude { args } => {
            let (labels, args) = split_labels(args);
            launch::run_agent("claude", labels, args).await
        }
        Cmd::Codex { args } => {
            let (labels, args) = split_labels(args);
            launch::run_agent("codex", labels, args).await
        }
        Cmd::Rename { target, name, clear } => {
            if name.is_none() && !clear {
                anyhow::bail!("give a name or --clear");
            }
            let r = cq_proto::Rename { name: if clear { None } else { name } };
            let id = cli::session_op(&target, cq_proto::Message::Rename(r), "meta").await?;
            println!("renamed {id}");
            Ok(0)
        }
        Cmd::Inject { target, text, submit, quiet_ms } => {
            let inj = cq_proto::Inject { id: session::now_ms(), text, submit, quiet_ms };
            let id = cli::session_op(&target, cq_proto::Message::Inject(inj), "inject_queued").await?;
            println!("queued for {id}; it is typed once the session is idle");
            Ok(0)
        }
        Cmd::Kill { target, force, grace } => {
            let t = cq_proto::Terminate { grace_ms: grace * 1000, force };
            let id = cli::session_op(&target, cq_proto::Message::Terminate(t), cli::EXITED).await?;
            println!("session {id} ended");
            Ok(0)
        }
        Cmd::Attach { id } => {
            let meta = session::resolve(&id)?;
            let outcome = attach::attach(&meta.pipe, Duration::from_secs(2)).await?;
            Ok(attach::report(&meta.id, outcome))
        }
        Cmd::Ls => {
            println!("{:<12}  {:<16}  {:<7}  {:<8}  CWD", "ID", "NAME", "AGENT", "STATUS");
            for m in session::list()? {
                let status = match m.status {
                    SessionStatus::Running => "running",
                    SessionStatus::Exited => "exited",
                };
                println!(
                    "{:<12}  {:<16}  {:<7}  {:<8}  {}",
                    m.id,
                    m.name.as_deref().unwrap_or("-"),
                    m.agent,
                    status,
                    m.cwd
                );
            }
            Ok(0)
        }
        Cmd::Run { name, args } => cli::run_command(&name, &args).await,
        Cmd::Plugin { action } => {
            match action {
                PluginCmd::List => plugin::list()?,
                PluginCmd::Add { name } => plugin::add(&name)?,
                PluginCmd::Remove { name } => plugin::remove(&name)?,
                PluginCmd::Refresh => plugin::refresh()?,
            }
            Ok(0)
        }
        Cmd::Machine { action } => {
            match action {
                MachineCmd::List => machine::list()?,
                MachineCmd::Add { host, label, cq_path, login_shell } => machine::add(host, label, cq_path, login_shell)?,
                MachineCmd::Remove { id } => machine::remove(&id)?,
            }
            Ok(0)
        }
        Cmd::Usage { local, json } => {
            cli::usage(local, json).await?;
            Ok(0)
        }
        Cmd::Mcp { scope, channel } => {
            mcp::run(cq_client::Scope::parse(&scope)?, channel).await?;
            Ok(0)
        }
        Cmd::Daemon { action } => {
            match action {
                None => daemon::run().await?,
                Some(DaemonCmd::Status) => cli::daemon_status().await?,
                Some(DaemonCmd::Stop { machine }) => cli::daemon_stop(&machine, false).await?,
                Some(DaemonCmd::Restart { machine }) => cli::daemon_stop(&machine, true).await?,
            }
            Ok(0)
        }
        Cmd::Bridge => {
            daemon::bridge().await?;
            Ok(0)
        }
        Cmd::Host { id, agent, cwd, cols, rows, name, command } => {
            host::run(host::HostArgs { id, agent, cwd, size: Size { cols, rows }, name, command })
                .await?;
            Ok(0)
        }
        Cmd::DebugScreen { cols, rows } => {
            let mut raw = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut raw)?;
            let mut p = vt100::Parser::new(rows, cols, 0);
            p.process(&raw);
            println!("input_empty: {:?}", agent::claude_input_empty(p.screen()));
            println!("prompt: {:?}", agent::find_choice_prompt(&host::screen_lines(p.screen())));
            agent::debug_input_row(p.screen());
            Ok(0)
        }
        Cmd::External(mut args) => {
            let name = args.remove(0);
            plugin::run_external(&name.to_string_lossy(), &args)
        }
        Cmd::Hook => {
            hook::hook().await;
            Ok(0)
        }
        Cmd::Statusline => {
            hook::statusline().await;
            Ok(0)
        }
    }
}
