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
    let path = paths::claude_settings()?;
    std::fs::write(&path, serde_json::to_vec_pretty(&settings)?)?;
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

/// Spawns `cq host` for a new session.
pub fn spawn_host(id: &str, agent: &str, cwd: &str, size: Size, command: &[String]) -> Result<()> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["host", "--id", id, "--agent", agent, "--cwd", cwd])
        .args(["--cols", &size.cols.to_string(), "--rows", &size.rows.to_string(), "--"])
        .args(command);
    spawn_detached(&mut cmd, &paths::root()?.join("logs").join(format!("{id}.log")))
}

/// Starts a session without a local terminal (dashboard launch or resume). Returns the id.
pub fn launch_detached(req: &cq_proto::Launch) -> Result<String> {
    if req.agent != "claude" {
        anyhow::bail!("unsupported agent '{}'", req.agent);
    }
    let cwd = crate::config::check_allowed(&req.cwd)?;
    paths::ensure_dirs()?;
    let settings = write_claude_settings()?;
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
    let id = session::new_id();
    spawn_host(&id, &req.agent, &cwd.to_string_lossy(), size, &command)?;
    Ok(id)
}

/// Runs `cq claude [args...]` and returns the process exit code.
pub async fn claude(args: Vec<String>) -> Result<i32> {
    paths::ensure_dirs()?;
    let settings = write_claude_settings()?;
    let id = session::new_id();
    let mut command = vec![
        "claude".to_owned(),
        "--settings".to_owned(),
        settings.to_string_lossy().into_owned(),
    ];
    command.extend(args);
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    spawn_host(&id, "claude", &cwd, crate::term::size(), &command)?;
    let endpoint = paths::session_endpoint(&id)?;
    let outcome = attach::attach(&endpoint, Duration::from_secs(10))
        .await
        .with_context(|| format!("session host did not start; see ~/.codequilt/logs/{id}.log"))?;
    Ok(attach::report(&id, outcome))
}
