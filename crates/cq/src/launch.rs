//! `cq claude`: starts a detached session host for claude and attaches the current terminal.

use crate::{attach, paths, session};
use anyhow::{Context, Result};
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

/// Spawns `cq host` detached from the current console and process tree.
fn spawn_host(id: &str, agent: &str, command: &[String]) -> Result<()> {
    let exe = std::env::current_exe()?;
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let size = crate::term::size();
    let log_dir = paths::root()?.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let log = std::fs::File::create(log_dir.join(format!("{id}.log")))?;

    let mut cmd = Command::new(exe);
    cmd.args(["host", "--id", id, "--agent", agent, "--cwd", &cwd])
        .args(["--cols", &size.cols.to_string(), "--rows", &size.rows.to_string(), "--"])
        .args(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
        };
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        // Break away from the terminal's job object so closing the terminal does not kill the
        // host; fall back if the job forbids breakaway.
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
    cmd.spawn().context("spawn session host")?;
    Ok(())
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
    spawn_host(&id, "claude", &command)?;
    let endpoint = paths::session_endpoint(&id)?;
    let outcome = attach::attach(&endpoint, Duration::from_secs(10))
        .await
        .with_context(|| format!("session host did not start; see ~/.codequilt/logs/{id}.log"))?;
    Ok(attach::report(&id, outcome))
}
