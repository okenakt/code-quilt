//! `cq-orchestrator`: CodeQuilt's official orchestrator. An orchestrator is an ordinary named agent
//! session; this program starts it with the right profile and runs the courier that brings it
//! news from the other sessions.

mod config;
mod courier;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::{Delivery, OrchestratorConfig};
use cq_protocol::{LOCAL_MACHINE, Launch, Message, SessionStatus};
use serde_json::json;

#[derive(Parser)]
#[command(name = "cq-orchestrator", version, about = "CodeQuilt's official orchestrator")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start an orchestrator session (and its courier). Settings: ~/.codequilt/orchestrators/<NAME>.toml, each
    /// overridable for this start with KEY=VALUE (e.g. `agent=codex`).
    Start {
        /// [NAME] [KEY=VALUE]…; the name defaults to `orchestrator`.
        args: Vec<String>,
        /// Start without attaching this terminal (answer the dashboard's dialogs instead).
        #[arg(long)]
        detach: bool,
        /// Extra arguments for the agent.
        #[arg(last = true)]
        extra: Vec<String>,
    },
    /// Run the courier for an orchestrator (started automatically by `start`).
    #[command(hide = true)]
    Run { name: String, overrides: Vec<String> },
    /// Print the settings file of an orchestrator, creating it with defaults if missing.
    Config {
        #[arg(default_value = "orchestrator")]
        name: String,
    },
    /// Print the commands this plugin provides (read by `cq plugin`).
    #[command(hide = true)]
    Manifest,
}

/// Commands for the palette and key bindings. `orchestrator.start` runs `cq-orchestrator start NAME --detach`;
/// an empty agent keeps the one in the settings file.
fn manifest() -> serde_json::Value {
    serde_json::json!({
        "name": "orchestrator",
        "commands": [{
            "name": "orchestrator.start",
            "title": "Orchestrator: Start",
            "args": [
                { "name": "name", "title": "Orchestrator name", "default": "orchestrator" },
                { "name": "agent", "title": "Agent (claude / codex; empty: as in the settings)", "default": "" },
            ],
            "run": { "exec": ["start", "${name}", "agent=${agent}", "--detach"] },
        }],
    })
}

/// Agent arguments that give the session its orchestrator profile: the cq MCP server, its tools that
/// need no confirmation, the persona and the model.
fn agent_args(name: &str, cfg: &OrchestratorConfig) -> Result<Vec<String>> {
    let mut mcp_args = vec!["mcp".to_owned()];
    for s in &cfg.scope {
        mcp_args.extend(["--scope".to_owned(), s.clone()]);
    }
    if cfg.delivery() == Delivery::Channels {
        mcp_args.push("--channel".into());
    }
    // A bare name would let the agent pick up whatever cq is on PATH, possibly an older one.
    let cq = cq_client::cq_binary();
    if !cq.is_file() {
        bail!("cq was not found next to cq-orchestrator; build or install cq, or set CQ_BIN");
    }
    let mut tools = vec!["list_sessions", "get_turns", "send", "launch"];
    if cfg.allow_approve {
        tools.push("approve");
    }
    if cfg.agent == "codex" {
        return codex_args(cfg, &cq, &mcp_args, &tools);
    }

    let mcp = json!({ "mcpServers": { "cq": { "command": cq, "args": mcp_args } } });
    let mcp_path = config::orchestrators_dir()?.join(format!("{name}.mcp.json"));
    std::fs::write(&mcp_path, serde_json::to_vec_pretty(&mcp)?)?;
    let tools: Vec<String> = tools.iter().map(|t| format!("mcp__cq__{t}")).collect();
    let mut args = vec![
        "--model".to_owned(),
        cfg.model.clone().unwrap_or_else(|| "sonnet".into()),
        "--mcp-config".to_owned(),
        mcp_path.to_string_lossy().into_owned(),
        "--append-system-prompt".to_owned(),
        config::persona(cfg)?,
        "--allowedTools".to_owned(),
        tools.join(","),
    ];
    if cfg.delivery() == Delivery::Channels {
        // cq is not on the approved channel list; Claude Code asks to confirm this at startup.
        args.extend(["--dangerously-load-development-channels".to_owned(), "server:cq".to_owned()]);
    }
    Ok(args)
}

/// A TOML string that survives the command line. On Windows Codex starts through `codex.cmd`, so
/// `cmd.exe` would act on characters such as `<`, `>` and `"` (and a line break ends the command);
/// every character outside a plain set is written as a `\u` escape instead.
fn toml_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || " .,:;'-_/()?*+=@#[]{}~".contains(c) {
            out.push(c);
        } else if (c as u32) <= 0xFFFF {
            out.push_str(&format!("\\u{:04X}", c as u32));
        } else {
            out.push_str(&format!("\\U{:08X}", c as u32));
        }
    }
    out.push('"');
    out
}

/// Codex takes the same profile as `-c` config overrides.
fn codex_args(cfg: &OrchestratorConfig, cq: &std::path::Path, mcp_args: &[String], tools: &[&str]) -> Result<Vec<String>> {
    let args_toml: Vec<String> = mcp_args.iter().map(|a| toml_string(a)).collect();
    let mut set = vec![
        format!("mcp_servers.cq.command={}", toml_string(&cq.to_string_lossy().replace('\\', "/"))),
        format!("mcp_servers.cq.args=[{}]", args_toml.join(",")),
        r#"mcp_servers.cq.default_tools_approval_mode="prompt""#.to_owned(),
        format!("developer_instructions={}", toml_string(&config::persona(cfg)?)),
    ];
    set.extend(tools.iter().map(|t| format!(r#"mcp_servers.cq.tools.{t}.approval_mode="approve""#)));
    let mut args: Vec<String> = set.into_iter().flat_map(|s| ["-c".to_owned(), s]).collect();
    if let Some(m) = &cfg.model {
        args.extend(["-m".to_owned(), m.clone()]);
    }
    Ok(args)
}

fn orchestrator_cwd(name: &str, cfg: &OrchestratorConfig) -> Result<std::path::PathBuf> {
    let dir = match &cfg.cwd {
        Some(c) => c.into(),
        None => config::orchestrators_dir()?.join(name),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

fn spawn_courier(name: &str, overrides: &[String]) -> Result<()> {
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.args(["run", name]).args(overrides);
    cq_client::spawn_detached(&mut cmd, &cq_client::log_file(&format!("orchestrator-{name}"))?)
}

/// Launches the orchestrator session through the daemon without a terminal.
async fn launch_detached(name: &str, agent: &str, cwd: &std::path::Path, args: Vec<String>) -> Result<String> {
    let mut conn = cq_client::DaemonClient::open(false).await?;
    let launch = Launch {
        id: 1,
        machine: LOCAL_MACHINE.into(),
        agent: agent.into(),
        cwd: cwd.to_string_lossy().into_owned(),
        args,
        name: Some(name.into()),
        // The orchestrator has its own cq MCP server (`agent_args`).
        no_mcp: true,
        cols: 120,
        rows: 36,
    };
    conn.launch(launch).await
}

/// The running local session of the orchestrator `name`, if any (session names are unique).
async fn running_orchestrator(name: &str) -> Result<Option<cq_protocol::SessionMeta>> {
    let mut conn = cq_client::DaemonClient::open(false).await?;
    conn.send(&Message::ListSessions).await?;
    while let Some(msg) = conn.msgs.recv().await {
        if let Message::SessionList(l) = msg? {
            return Ok(l.sessions.into_iter().find(|s| s.status == SessionStatus::Running && s.name.as_deref() == Some(name)));
        }
    }
    bail!("daemon connection closed")
}

/// Splits `[NAME] [KEY=VALUE]…` into the orchestrator name and the settings overrides.
fn name_and_overrides(args: Vec<String>) -> (String, Vec<String>) {
    let (overrides, names): (Vec<_>, Vec<_>) = args.into_iter().partition(|a| a.contains('='));
    (names.into_iter().next().unwrap_or_else(|| "orchestrator".into()), overrides)
}

async fn start(args: Vec<String>, detach: bool, extra: Vec<String>) -> Result<i32> {
    let (name, overrides) = name_and_overrides(args);
    let name = name.as_str();
    if let Some(s) = running_orchestrator(name).await? {
        println!("orchestrator '{name}' is already running on {} as session {}", s.agent, s.id);
        return Ok(0);
    }
    let (cfg, path) = config::load(name, &overrides)?;
    let mut args = agent_args(name, &cfg)?;
    args.extend(extra);
    let cwd = orchestrator_cwd(name, &cfg)?;
    spawn_courier(name, &overrides)?;
    eprintln!("[cq-orchestrator] {name} ({}): settings {}", cfg.agent, path.display());
    if cfg.delivery() == Delivery::Channels {
        eprintln!("[cq-orchestrator] confirm the development channel prompt (here or in the dashboard) to let updates in");
    }
    if detach {
        let id = launch_detached(name, &cfg.agent, &cwd, args).await?;
        println!("started orchestrator '{name}' on {} as session {id}; attach with `cq attach {id}`", cfg.agent);
        return Ok(0);
    }
    let status = std::process::Command::new(cq_client::cq_binary())
        .args([cfg.agent.as_str(), "--cq-name", name, "--cq-no-mcp"])
        .args(&args)
        .current_dir(&cwd)
        .status()
        .with_context(|| format!("run cq {}", cfg.agent))?;
    Ok(status.code().unwrap_or(1))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Start { args, detach, extra } => start(args, detach, extra).await,
        Cmd::Run { name, overrides } => match config::load(&name, &overrides) {
            Ok((cfg, _)) => courier::run(&name, cfg).await.map(|()| 0),
            Err(e) => Err(e),
        },
        Cmd::Config { name } => config::load(&name, &[]).map(|(_, p)| {
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
            eprintln!("cq-orchestrator: {e:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_string_round_trips_with_only_plain_characters() {
        let text = config::DEFAULT_PERSONA.to_owned() + " 日本語 😀 %PATH% a|b&c^";
        let s = toml_string(&text);
        assert!(s[1..s.len() - 1].chars().all(|c| c.is_ascii_alphanumeric() || " .,:;'-_/()?*+=@#[]{}~\\".contains(c)), "{s}");
        let parsed: toml::Table = toml::from_str(&format!("v = {s}")).unwrap();
        assert_eq!(parsed["v"].as_str(), Some(text.as_str()));
    }
}
