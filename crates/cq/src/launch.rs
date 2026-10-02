//! `cq claude`: starts a detached session host for claude and attaches the current terminal.

use crate::{attach, paths, session};
use anyhow::{Context, Result};
use cq_proto::Size;
use serde_json::json;
use std::process::{Command, Stdio};
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

/// Spawns a command detached from the current console and process tree, with stderr to `log`.
pub fn spawn_detached(cmd: &mut Command, log: &std::path::Path) -> Result<()> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(std::fs::File::create(log)?);

    #[cfg(windows)]
    {
        cq_client::disinherit_std_handles();
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
        };
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        // Break away from the terminal's job object so closing the terminal does not kill the
        // process; fall back if the job forbids breakaway.
        cmd.creation_flags(base | CREATE_BREAKAWAY_FROM_JOB);
        if cmd.spawn().is_ok() {
            return Ok(());
        }
        cmd.creation_flags(base);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    cmd.spawn().context("spawn detached process")?;
    Ok(())
}

/// Name and tags given to a session at start.
#[derive(Default)]
pub struct Labels {
    pub name: Option<String>,
    pub tags: Vec<String>,
    /// Extra settings file merged with CodeQuilt's hooks.
    pub settings_extra: Option<String>,
}

/// Spawns `cq host` for a new session.
pub fn spawn_host(id: &str, agent: &str, cwd: &str, size: Size, labels: &Labels, command: &[String]) -> Result<()> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["host", "--id", id, "--agent", agent, "--cwd", cwd])
        .args(["--cols", &size.cols.to_string(), "--rows", &size.rows.to_string()]);
    if let Some(n) = &labels.name {
        cmd.args(["--name", n]);
    }
    for t in &labels.tags {
        cmd.args(["--tag", t]);
    }
    cmd.arg("--").args(command);
    spawn_detached(&mut cmd, &paths::root()?.join("logs").join(format!("{id}.log")))
}

/// Starts a session without a local terminal (dashboard launch or resume). Returns the id.
pub fn launch_detached(req: &cq_proto::Launch) -> Result<String> {
    if req.agent != "claude" {
        anyhow::bail!("unsupported agent '{}'", req.agent);
    }
    let cwd = crate::config::check_allowed(&req.cwd)?;
    paths::ensure_dirs()?;
    let id = session::new_id();
    let settings = session_settings(&id, req.settings_extra.as_deref())?;
    let mut command = vec![
        "claude".to_owned(),
        "--settings".to_owned(),
        settings.to_string_lossy().into_owned(),
    ];
    if let Some(id) = &req.resume {
        command.extend(["--resume".to_owned(), id.clone()]);
    }
    command.extend(req.args.iter().cloned());
    let size = Size { cols: req.cols.max(20), rows: req.rows.max(5) };
    let labels = Labels { name: req.name.clone(), tags: req.tags.clone(), settings_extra: None };
    spawn_host(&id, &req.agent, &cwd.to_string_lossy(), size, &labels, &command)?;
    Ok(id)
}

/// Runs `cq claude [args...]` and returns the process exit code.
pub async fn claude(labels: Labels, args: Vec<String>) -> Result<i32> {
    paths::ensure_dirs()?;
    let id = session::new_id();
    let settings = session_settings(&id, labels.settings_extra.as_deref())?;
    let mut command = vec![
        "claude".to_owned(),
        "--settings".to_owned(),
        settings.to_string_lossy().into_owned(),
    ];
    command.extend(args);
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    spawn_host(&id, "claude", &cwd, crate::term::size(), &labels, &command)?;
    let endpoint = paths::session_endpoint(&id)?;
    let outcome = attach::attach(&endpoint, Duration::from_secs(10))
        .await
        .with_context(|| format!("session host did not start; see ~/.codequilt/logs/{id}.log"))?;
    Ok(attach::report(&id, outcome))
}
