//! `cq claude`: starts a detached session host for claude and attaches the current terminal.

use crate::{attach, paths, session};
use anyhow::{Context, Result};
use cq_proto::Size;
use serde_json::json;
use std::process::Command;
use std::time::Duration;

/// Writes `~/.codequilt/claude.json`, the settings injected via `--settings`.
///
/// Only hooks and the status line are injected; the user's `~/.claude` is never modified.
pub fn write_claude_settings() -> Result<std::path::PathBuf> {
    let path = paths::claude_settings()?;
    std::fs::write(&path, serde_json::to_vec_pretty(&cq_settings()?)?)?;
    Ok(path)
}

fn cq_settings() -> Result<serde_json::Value> {
    let exe = std::env::current_exe()?.to_string_lossy().replace('\\', "/");
    let hook = json!([{ "hooks": [{ "type": "command", "command": format!("\"{exe}\" hook") }] }]);
    let settings = json!({
        "hooks": {
            "SessionStart": hook,
            "UserPromptSubmit": hook,
            "Stop": hook,
            "Notification": hook,
        },
        "statusLine": { "type": "command", "command": format!("\"{exe}\" statusline"), "padding": 0 },
    });
    Ok(settings)
}

/// Spawns `cq host` for a new session.
pub fn spawn_host(id: &str, agent: &str, cwd: &str, size: Size, name: Option<&str>, command: &[String]) -> Result<()> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["host", "--id", id, "--agent", agent, "--cwd", cwd])
        .args(["--cols", &size.cols.to_string(), "--rows", &size.rows.to_string()]);
    if let Some(n) = name {
        if let Some(other) = session::name_taken(n, "") {
            anyhow::bail!("session {other} is already named '{n}'");
        }
        cmd.args(["--name", n]);
    }
    cmd.arg("--").args(command);
    cq_client::spawn_detached(&mut cmd, &cq_client::log_file(id)?)
}

/// `cq` with forward slashes, as the agents' configs take it.
fn cq_exe() -> Result<String> {
    Ok(std::env::current_exe()?.to_string_lossy().replace('\\', "/"))
}

/// `cq` for Codex's `-c` overrides, which hold it unquoted.
fn codex_exe() -> Result<String> {
    let exe = cq_exe()?;
    if exe.contains(' ') {
        anyhow::bail!("cq must be installed in a path without spaces to hook into codex ({exe})");
    }
    Ok(exe)
}

/// Codex has no `--settings`; its hooks are injected as `-c` config overrides. They run next to
/// the user's own hooks from `~/.codex/hooks.json`.
fn codex_hook_args() -> Result<Vec<String>> {
    let exe = codex_exe()?;
    let mut args = Vec::new();
    for event in ["SessionStart", "UserPromptSubmit", "Stop", "PermissionRequest"] {
        args.push("-c".to_owned());
        args.push(format!(r#"hooks.{event}=[{{hooks=[{{type="command",command="{exe} hook",timeout=10}}]}}]"#));
    }
    Ok(args)
}

/// Agent arguments that give a session CodeQuilt's worker MCP server (`cq mcp --worker`), whose
/// tools need no confirmation.
fn worker_mcp_args(agent: &str) -> Result<Vec<String>> {
    let tools = crate::mcp::WORKER_TOOLS;
    if agent == "codex" {
        let exe = codex_exe()?;
        let mut set = vec![format!(r#"mcp_servers.cq.command="{exe}""#), r#"mcp_servers.cq.args=["mcp","--worker"]"#.to_owned()];
        set.extend(tools.iter().map(|t| format!(r#"mcp_servers.cq.tools.{t}.approval_mode="approve""#)));
        return Ok(set.into_iter().flat_map(|s| ["-c".to_owned(), s]).collect());
    }
    let path = paths::root()?.join("worker.mcp.json");
    let config = json!({ "mcpServers": { "cq": { "command": cq_exe()?, "args": ["mcp", "--worker"] } } });
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
    let allowed: Vec<String> = tools.iter().map(|t| format!("mcp__cq__{t}")).collect();
    Ok(vec!["--mcp-config".to_owned(), path.to_string_lossy().into_owned(), "--allowedTools".to_owned(), allowed.join(",")])
}

/// The agent command line for a new session, with CodeQuilt's hooks wired in, and its worker MCP
/// server unless the session brings its own (an orchestrator).
fn agent_command(agent: &str, resume: Option<&str>, mcp: bool, args: &[String]) -> Result<Vec<String>> {
    let mut command = match agent {
        "claude" => {
            let settings = write_claude_settings()?;
            let mut c = vec!["claude".to_owned(), "--settings".to_owned(), settings.to_string_lossy().into_owned()];
            if let Some(r) = resume {
                c.extend(["--resume".to_owned(), r.to_owned()]);
            }
            c
        }
        "codex" => {
            let mut c = vec!["codex".to_owned()];
            if let Some(r) = resume {
                c.extend(["resume".to_owned(), r.to_owned()]);
            }
            c.extend(codex_hook_args()?);
            c
        }
        other => anyhow::bail!("unsupported agent '{other}'"),
    };
    if mcp {
        command.extend(worker_mcp_args(agent)?);
    }
    command.extend(args.iter().cloned());
    Ok(command)
}

/// Starts a session without a local terminal (dashboard launch or resume). Returns the id.
pub fn launch_detached(req: &cq_proto::Launch) -> Result<String> {
    let cwd = crate::config::check_allowed(&req.cwd)?;
    paths::ensure_dirs()?;
    let id = session::new_id();
    let command = agent_command(&req.agent, None, !req.no_mcp, &req.args)?;
    let size = Size { cols: req.cols.max(20), rows: req.rows.max(5) };
    spawn_host(&id, &req.agent, &cwd.to_string_lossy(), size, req.name.as_deref(), &command)?;
    Ok(id)
}

/// Starts the ended session `key` again under its id, in its directory and with its name,
/// resuming the agent's conversation. A session that ended before its first turn has no
/// conversation yet (Codex starts one only then), so the agent starts afresh. Returns the id.
pub fn resume_detached(key: &str, size: Size) -> Result<String> {
    let list = session::list()?;
    let m = session::pick_ended(&list, key)?;
    let cwd = crate::config::check_allowed(&m.cwd)?;
    let command = agent_command(&m.agent, m.agent_session_id.as_deref(), true, &[])?;
    spawn_host(&m.id, &m.agent, &cwd.to_string_lossy(), size, m.name.as_deref(), &command)?;
    Ok(m.id.clone())
}

/// Runs `cq claude|codex [args...]` in this terminal and returns the process exit code.
pub async fn run_agent(agent: &str, name: Option<String>, mcp: bool, args: Vec<String>) -> Result<i32> {
    paths::ensure_dirs()?;
    let id = session::new_id();
    let command = agent_command(agent, None, mcp, &args)?;
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    spawn_host(&id, agent, &cwd, crate::term::size(), name.as_deref(), &command)?;
    attach_new(&id).await
}

/// `cq resume KEY`: resumes an ended session and attaches this terminal to it.
pub async fn resume(key: &str) -> Result<i32> {
    let id = resume_detached(key, crate::term::size())?;
    attach_new(&id).await
}

/// Attaches this terminal to a session whose host was just spawned.
async fn attach_new(id: &str) -> Result<i32> {
    let endpoint = paths::session_endpoint(id)?;
    let outcome = attach::attach(&endpoint, Duration::from_secs(10))
        .await
        .with_context(|| format!("session host did not start; see ~/.codequilt/logs/{id}.log"))?;
    Ok(attach::report(id, outcome))
}
