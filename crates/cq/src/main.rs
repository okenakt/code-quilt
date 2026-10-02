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
mod remote;
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
    /// Run claude inside a detachable CodeQuilt session. `--cq-name NAME` and `--cq-tag TAG`
    /// label the session; all other arguments go to claude.
    #[command(disable_help_flag = true)]
    Claude {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Attach this terminal to a running session (id or unique prefix). Ctrl+\ detaches.
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
    /// Name a running session (`[machine:]id`); `--clear` removes the name.
    Rename {
        target: String,
        name: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// Add (`+tag`) or remove (`-tag`) tags of a running session (`[machine:]id`).
    Tag {
        target: String,
        #[arg(allow_hyphen_values = true, required = true)]
        changes: Vec<String>,
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
    /// Manage remote machines shown in the dashboard.
    Machine {
        #[command(subcommand)]
        action: MachineCmd,
    },
    /// MCP server (stdio) exposing the sessions to an agent, e.g. an orchestrator.
    Mcp {
        /// Limit to sessions with a tag or on a machine: `tag:NAME` or `machine:ID` (repeatable).
        #[arg(long)]
        scope: Vec<String>,
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
        #[arg(long)]
        name: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
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

/// Takes `--cq-name NAME` and `--cq-tag TAG` out of the agent arguments.
fn split_labels(args: Vec<String>) -> (launch::Labels, Vec<String>) {
    let mut labels = launch::Labels::default();
    let mut rest = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cq-name" => labels.name = it.next(),
            "--cq-tag" => labels.tags.extend(it.next()),
            _ => rest.push(a),
        }
    }
    (labels, rest)
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
        Cmd::Claude { args } => {
            let (labels, args) = split_labels(args);
            launch::claude(labels, args).await
        }
        Cmd::Rename { target, name, clear } => {
            if name.is_none() && !clear {
                anyhow::bail!("give a name or --clear");
            }
            let m = cq_proto::SetMeta { name, clear_name: clear, add_tags: vec![], remove_tags: vec![] };
            let id = cli::session_op(&target, cq_proto::Message::SetMeta(m), "meta").await?;
            println!("renamed {id}");
            Ok(0)
        }
        Cmd::Tag { target, changes } => {
            let mut m = cq_proto::SetMeta { name: None, clear_name: false, add_tags: vec![], remove_tags: vec![] };
            for c in changes {
                match c.strip_prefix('-') {
                    Some(t) => m.remove_tags.push(t.to_owned()),
                    None => m.add_tags.push(c.trim_start_matches('+').to_owned()),
                }
            }
            let id = cli::session_op(&target, cq_proto::Message::SetMeta(m), "meta").await?;
            println!("tagged {id}");
            Ok(0)
        }
        Cmd::Inject { target, text, submit, quiet_ms } => {
            let inj = cq_proto::Inject { id: session::now_ms(), text, submit, quiet_ms };
            let id = cli::session_op(&target, cq_proto::Message::Inject(inj), "inject_queued").await?;
            println!("queued for {id}; it is typed once the session is idle");
            Ok(0)
        }
        Cmd::Attach { id } => {
            let meta = session::resolve(&id)?;
            if meta.status != SessionStatus::Running {
                anyhow::bail!("session {} has exited", meta.id);
            }
            let outcome = attach::attach(&meta.pipe, Duration::from_secs(2)).await?;
            Ok(attach::report(&meta.id, outcome))
        }
        Cmd::Ls => {
            println!("{:<12}  {:<16}  {:<7}  {:<8}  {:<16}  CWD", "ID", "NAME", "AGENT", "STATUS", "TAGS");
            for m in session::list()? {
                let status = match m.status {
                    SessionStatus::Running => "running",
                    SessionStatus::Exited => "exited",
                };
                println!(
                    "{:<12}  {:<16}  {:<7}  {:<8}  {:<16}  {}",
                    m.id,
                    m.name.as_deref().unwrap_or("-"),
                    m.agent,
                    status,
                    if m.tags.is_empty() { "-".to_owned() } else { m.tags.join(",") },
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
        Cmd::Usage { local, json } => {
            cli::usage(local, json).await?;
            Ok(0)
        }
        Cmd::Mcp { scope } => {
            mcp::run(mcp::Scope::parse(&scope)?).await?;
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
        Cmd::Host { id, agent, cwd, cols, rows, name, tags, command } => {
            host::run(host::HostArgs { id, agent, cwd, size: Size { cols, rows }, name, tags, command })
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
