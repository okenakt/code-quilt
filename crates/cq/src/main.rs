//! `cq`: CodeQuilt command line. A single binary provides the user commands, the per-session
//! host, and the hook/status line endpoints invoked by agents.

mod agent;
mod attach;
mod config;
mod daemon;
mod hook;
mod host;
mod ipc;
mod launch;
mod machine;
mod paths;
mod session;
mod term;
mod usage;

use anyhow::Result;
use clap::{Parser, Subcommand};
use cq_proto::{SessionStatus, Size};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "cq", version, about = "CodeQuilt: orchestrate parallel coding agent sessions")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run claude inside a detachable CodeQuilt session.
    #[command(disable_help_flag = true)]
    Claude {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Attach this terminal to a running session (id or unique prefix). Ctrl+\ detaches.
    Attach { id: String },
    /// List sessions.
    Ls,
    /// Manage remote machines shown in the dashboard.
    Machine {
        #[command(subcommand)]
        action: MachineCmd,
    },
    /// Run the machine daemon that relays sessions to dashboards.
    Daemon,
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
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Agent hook endpoint (internal).
    #[command(hide = true)]
    Hook,
    /// Agent status line endpoint (internal).
    #[command(hide = true)]
    Statusline,
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
    },
    /// Unregister a machine.
    Remove { id: String },
}

fn main() {
    let cli = Cli::parse();
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
        Cmd::Claude { args } => launch::claude(args).await,
        Cmd::Attach { id } => {
            let meta = session::resolve(&id)?;
            if meta.status != SessionStatus::Running {
                anyhow::bail!("session {} has exited", meta.id);
            }
            let outcome = attach::attach(&meta.pipe, Duration::from_secs(2)).await?;
            Ok(attach::report(&meta.id, outcome))
        }
        Cmd::Ls => {
            println!("{:<12}  {:<7}  {:<8}  {:<36}  CWD", "ID", "AGENT", "STATUS", "AGENT SESSION");
            for m in session::list()? {
                let status = match m.status {
                    SessionStatus::Running => "running",
                    SessionStatus::Exited => "exited",
                };
                println!(
                    "{:<12}  {:<7}  {:<8}  {:<36}  {}",
                    m.id,
                    m.agent,
                    status,
                    m.agent_session_id.as_deref().unwrap_or("-"),
                    m.cwd
                );
            }
            Ok(0)
        }
        Cmd::Machine { action } => {
            match action {
                MachineCmd::List => machine::list()?,
                MachineCmd::Add { host, label, cq_path } => machine::add(host, label, cq_path)?,
                MachineCmd::Remove { id } => machine::remove(&id)?,
            }
            Ok(0)
        }
        Cmd::Daemon => {
            daemon::run().await?;
            Ok(0)
        }
        Cmd::Bridge => {
            daemon::bridge().await?;
            Ok(0)
        }
        Cmd::Host { id, agent, cwd, cols, rows, command } => {
            host::run(host::HostArgs { id, agent, cwd, size: Size { cols, rows }, command })
                .await?;
            Ok(0)
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
