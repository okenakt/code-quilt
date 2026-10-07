//! `cq-queen`: CodeQuilt's official orchestrator. A Queen is an ordinary agent session tagged
//! `queen`; this program starts it with the right profile and runs the courier that brings it
//! news from the other sessions.

mod config;
mod courier;
mod hook;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::{Delivery, QueenConfig};
use cq_proto::{LOCAL_MACHINE, Launch, Message, SessionStatus};
use serde_json::json;

#[derive(Parser)]
#[command(name = "cq-queen", version, about = "CodeQuilt's official orchestrator")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start a Queen session (and its courier). Settings: ~/.codequilt/queens/<NAME>.toml.
    Start {
        #[arg(default_value = "queen")]
        name: String,
        /// Start without attaching this terminal (answer the dashboard's dialogs instead).
        #[arg(long)]
        detach: bool,
        /// Extra arguments for the agent.
        #[arg(last = true)]
        extra: Vec<String>,
    },
    /// Run the courier for a Queen (started automatically by `start`).
    #[command(hide = true)]
    Run { name: String },
    /// UserPromptSubmit hook of a Queen session: sends `@name text` directly (internal).
    #[command(hide = true)]
    Hook,
    /// Print the settings file of a Queen, creating it with defaults if missing.
    Config {
        #[arg(default_value = "queen")]
        name: String,
    },
    /// Print the commands this plugin provides (read by `cq plugin`).
    #[command(hide = true)]
    Manifest,
}

/// Commands for the palette and key bindings. `queen.start` runs `cq-queen start NAME --detach`.
fn manifest() -> serde_json::Value {
    serde_json::json!({
        "name": "queen",
        "commands": [{
            "name": "queen.start",
            "title": "Queen: Start",
            "args": [{ "name": "name", "title": "Queen name", "default": "queen" }],
            "run": { "exec": ["start", "${name}", "--detach"] },
        }],
    })
}

/// Writes the Queen's extra settings: a UserPromptSubmit hook that sends `@name text` directly.
fn write_settings(name: &str) -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?.to_string_lossy().replace('\\', "/");
    let settings = json!({
        "hooks": { "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": format!("\"{exe}\" hook") }] }] }
    });
    let path = config::queens_dir()?.join(format!("{name}.settings.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&settings)?)?;
    Ok(path)
}

/// Agent arguments that give the session its Queen profile.
fn agent_args(name: &str, cfg: &QueenConfig) -> Result<Vec<String>> {
    if cfg.agent != "claude" {
        bail!("agent '{}' is not supported as a Queen yet", cfg.agent);
    }
    let dir = config::queens_dir()?;
    let mut mcp_args = vec!["mcp".to_owned()];
    for s in &cfg.scope {
        mcp_args.extend(["--scope".to_owned(), s.clone()]);
    }
    if cfg.delivery == Delivery::Channels {
        mcp_args.push("--channel".into());
    }
    // A bare name would let Claude Code pick up whatever cq is on PATH, possibly an older one.
    let cq = cq_client::cq_binary();
    if !cq.is_file() {
        bail!("cq was not found next to cq-queen; build or install cq, or set CQ_BIN");
    }
    let mcp = json!({ "mcpServers": { "cq": { "command": cq, "args": mcp_args } } });
    let mcp_path = dir.join(format!("{name}.mcp.json"));
    std::fs::write(&mcp_path, serde_json::to_vec_pretty(&mcp)?)?;

    let mut tools = vec!["mcp__cq__list_sessions", "mcp__cq__get_turns", "mcp__cq__send"];
    if cfg.allow_approve {
        tools.push("mcp__cq__approve");
    }
    let mut args = vec![
        "--model".to_owned(),
        cfg.model.clone(),
        "--mcp-config".to_owned(),
        mcp_path.to_string_lossy().into_owned(),
        "--append-system-prompt".to_owned(),
        config::persona(cfg)?,
        "--allowedTools".to_owned(),
        tools.join(","),
    ];
    if cfg.delivery == Delivery::Channels {
        // cq is not on the approved channel list; Claude Code asks to confirm this at startup.
        args.extend(["--dangerously-load-development-channels".to_owned(), "server:cq".to_owned()]);
    }
    Ok(args)
}

fn queen_cwd(name: &str, cfg: &QueenConfig) -> Result<std::path::PathBuf> {
    let dir = match &cfg.cwd {
        Some(c) => c.into(),
        None => config::queens_dir()?.join(name),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

fn spawn_courier(name: &str) -> Result<()> {
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.args(["run", name]);
    cq_client::spawn_detached(&mut cmd, &cq_client::log_file(&format!("queen-{name}"))?)
}

/// Launches the Queen session through the daemon without a terminal.
async fn launch_detached(name: &str, cwd: &std::path::Path, args: Vec<String>) -> Result<String> {
    let mut conn = cq_client::DaemonClient::open(false).await?;
    let launch = Launch {
        id: 1,
        machine: LOCAL_MACHINE.into(),
        agent: "claude".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        args,
        resume: None,
        name: Some(name.into()),
        settings_extra: Some(write_settings(name)?.to_string_lossy().into_owned()),
        cols: 120,
        rows: 36,
    };
    conn.send(&Message::Launch(launch)).await?;
    while let Some(msg) = conn.msgs.recv().await {
        match msg? {
            Message::Launched(l) if l.id == 1 => return Ok(l.session),
            Message::RequestFailed(f) if f.id == 1 => bail!(f.error),
            _ => {}
        }
    }
    bail!("daemon connection closed")
}

/// The running local session of the Queen `name`, if any (session names are unique).
async fn running_queen(name: &str) -> Result<Option<String>> {
    let mut conn = cq_client::DaemonClient::open(false).await?;
    conn.send(&Message::ListSessions).await?;
    while let Some(msg) = conn.msgs.recv().await {
        if let Message::SessionList(l) = msg? {
            return Ok(l
                .sessions
                .into_iter()
                .find(|s| s.status == SessionStatus::Running && s.name.as_deref() == Some(name))
                .map(|s| s.id));
        }
    }
    bail!("daemon connection closed")
}

async fn start(name: &str, detach: bool, extra: Vec<String>) -> Result<i32> {
    if let Some(id) = running_queen(name).await? {
        println!("queen '{name}' is already running as session {id}");
        return Ok(0);
    }
    let (cfg, path) = config::load_or_create(name)?;
    let mut args = agent_args(name, &cfg)?;
    args.extend(extra);
    let cwd = queen_cwd(name, &cfg)?;
    spawn_courier(name)?;
    eprintln!("[cq-queen] {name}: settings {}", path.display());
    if cfg.delivery == Delivery::Channels {
        eprintln!("[cq-queen] confirm the development channel prompt (here or in the dashboard) to let updates in");
    }
    if detach {
        let id = launch_detached(name, &cwd, args).await?;
        println!("started queen '{name}' as session {id}; attach with `cq attach {id}`");
        return Ok(0);
    }
    let status = std::process::Command::new(cq_client::cq_binary())
        .args(["claude", "--cq-name", name])
        .arg("--cq-settings")
        .arg(write_settings(name)?)
        .args(&args)
        .current_dir(&cwd)
        .status()
        .context("run cq claude")?;
    Ok(status.code().unwrap_or(1))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Start { name, detach, extra } => start(&name, detach, extra).await,
        Cmd::Run { name } => match config::load_or_create(&name) {
            Ok((cfg, _)) => courier::run(&name, cfg).await.map(|()| 0),
            Err(e) => Err(e),
        },
        Cmd::Hook => hook::run().await.map(|()| 0),
        Cmd::Config { name } => config::load_or_create(&name).map(|(_, p)| {
            println!("{}", p.display());
            0
        }),
        Cmd::Manifest => {
            println!("{}", manifest());
            Ok(0)
        }
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("cq-queen: {e:#}");
            std::process::exit(1);
        }
    }
}
