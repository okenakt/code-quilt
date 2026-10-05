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

/// Settings for one session: CodeQuilt's hooks plus those of `extra`. Claude Code applies only the
/// last `--settings`, so the files are merged: hook lists are concatenated per event, other keys
/// of `extra` are added, and CodeQuilt's status line is kept.
fn session_settings(id: &str, extra: Option<&str>) -> Result<std::path::PathBuf> {
    let Some(extra) = extra else { return write_claude_settings() };
    let mut base = cq_settings()?;
    let extra: serde_json::Value = serde_json::from_slice(&std::fs::read(extra).with_context(|| format!("read {extra}"))?)
        .with_context(|| format!("invalid settings file {extra}"))?;
    if let Some(obj) = extra.as_object() {
        for (k, v) in obj {
            match k.as_str() {
                "hooks" => {
                    for (event, list) in v.as_object().into_iter().flatten() {
                        let slot = &mut base["hooks"][event];
                        if !slot.is_array() {
                            *slot = json!([]);
                        }
                        let target = slot.as_array_mut().expect("array");
                        target.extend(list.as_array().cloned().unwrap_or_default());
                    }
                }
                "statusLine" => {}
                _ => base[k] = v.clone(),
            }
        }
    }
    let dir = paths::root()?.join("settings");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{id}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&base)?)?;
    Ok(path)
}

/// Name and extra settings given to a session at start.
#[derive(Default)]
pub struct Labels {
    pub name: Option<String>,
    /// Extra settings file merged with CodeQuilt's hooks.
    pub settings_extra: Option<String>,
}

/// Spawns `cq host` for a new session.
pub fn spawn_host(id: &str, agent: &str, cwd: &str, size: Size, labels: &Labels, command: &[String]) -> Result<()> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["host", "--id", id, "--agent", agent, "--cwd", cwd])
        .args(["--cols", &size.cols.to_string(), "--rows", &size.rows.to_string()]);
    if let Some(n) = &labels.name {
        if let Some(other) = session::name_taken(n, "") {
            anyhow::bail!("session {other} is already named '{n}'");
        }
        cmd.args(["--name", n]);
    }
    cmd.arg("--").args(command);
    cq_client::spawn_detached(&mut cmd, &cq_client::log_file(id)?)
}

/// Starts a session without a local terminal (dashboard launch or resume). Returns the id.
/// Codex has no `--settings`; its hooks are injected as `-c` config overrides. They run next to
/// the user's own hooks from `~/.codex/hooks.json`.
fn codex_hook_args() -> Result<Vec<String>> {
    let exe = std::env::current_exe()?.to_string_lossy().replace('\\', "/");
    if exe.contains(' ') {
        anyhow::bail!("cq must be installed in a path without spaces to hook into codex ({exe})");
    }
    let mut args = Vec::new();
    for event in ["SessionStart", "UserPromptSubmit", "Stop", "PermissionRequest"] {
        args.push("-c".to_owned());
        args.push(format!(r#"hooks.{event}=[{{hooks=[{{type="command",command="{exe} hook",timeout=10}}]}}]"#));
    }
    Ok(args)
}

/// The agent command line for a new session, with CodeQuilt's hooks wired in.
fn agent_command(agent: &str, id: &str, settings_extra: Option<&str>, resume: Option<&str>, args: &[String]) -> Result<Vec<String>> {
    let mut command = match agent {
        "claude" => {
            let settings = session_settings(id, settings_extra)?;
            let mut c = vec!["claude".to_owned(), "--settings".to_owned(), settings.to_string_lossy().into_owned()];
            if let Some(r) = resume {
                c.extend(["--resume".to_owned(), r.to_owned()]);
            }
            c
        }
        "codex" => {
            if settings_extra.is_some() {
                anyhow::bail!("extra settings are not supported for codex sessions yet");
            }
            let mut c = vec!["codex".to_owned()];
            if let Some(r) = resume {
                c.extend(["resume".to_owned(), r.to_owned()]);
            }
            c.extend(codex_hook_args()?);
            c
        }
        other => anyhow::bail!("unsupported agent '{other}'"),
    };
    command.extend(args.iter().cloned());
    Ok(command)
}

pub fn launch_detached(req: &cq_proto::Launch) -> Result<String> {
    let cwd = crate::config::check_allowed(&req.cwd)?;
    paths::ensure_dirs()?;
    let id = session::new_id();
    let command = agent_command(&req.agent, &id, req.settings_extra.as_deref(), req.resume.as_deref(), &req.args)?;
    let size = Size { cols: req.cols.max(20), rows: req.rows.max(5) };
    let labels = Labels { name: req.name.clone(), settings_extra: None };
    spawn_host(&id, &req.agent, &cwd.to_string_lossy(), size, &labels, &command)?;
    Ok(id)
}

/// Runs `cq claude|codex [args...]` in this terminal and returns the process exit code.
pub async fn run_agent(agent: &str, labels: Labels, args: Vec<String>) -> Result<i32> {
    paths::ensure_dirs()?;
    let id = session::new_id();
    let command = agent_command(agent, &id, labels.settings_extra.as_deref(), None, &args)?;
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    spawn_host(&id, agent, &cwd, crate::term::size(), &labels, &command)?;
    let endpoint = paths::session_endpoint(&id)?;
    let outcome = attach::attach(&endpoint, Duration::from_secs(10))
        .await
        .with_context(|| format!("session host did not start; see ~/.codequilt/logs/{id}.log"))?;
    Ok(attach::report(&id, outcome))
}
