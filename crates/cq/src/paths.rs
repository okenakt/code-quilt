//! Filesystem and IPC endpoint locations under `~/.codequilt`.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Root directory of CodeQuilt state (`~/.codequilt`).
pub fn root() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".codequilt"))
}

/// Directory holding `<id>.json` session metadata.
pub fn sessions_dir() -> Result<PathBuf> {
    Ok(root()?.join("sessions"))
}

/// Directory holding `<id>.jsonl` hook event spools.
pub fn spool_dir() -> Result<PathBuf> {
    Ok(root()?.join("spool"))
}

/// Directory holding the latest status line payload per session.
pub fn status_dir() -> Result<PathBuf> {
    Ok(root()?.join("status"))
}

/// Settings file injected into claude via `--settings`.
pub fn claude_settings() -> Result<PathBuf> {
    Ok(root()?.join("claude.json"))
}

/// Path of a session's metadata file.
pub fn session_file(id: &str) -> Result<PathBuf> {
    Ok(sessions_dir()?.join(format!("{id}.json")))
}

/// IPC endpoint name for a session host.
#[cfg(windows)]
pub fn session_endpoint(id: &str) -> Result<String> {
    Ok(format!(r"\\.\pipe\codequilt-{id}"))
}

/// IPC endpoint name for a session host.
#[cfg(unix)]
pub fn session_endpoint(id: &str) -> Result<String> {
    Ok(root()?.join("run").join(format!("{id}.sock")).to_string_lossy().into_owned())
}

fn user_tag() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".into())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// IPC endpoint of the machine daemon. Pipe names are machine-global on Windows, so the user name
/// is part of the name.
#[cfg(windows)]
pub fn daemon_endpoint() -> Result<String> {
    Ok(format!(r"\\.\pipe\codequilt-cqd-{}", user_tag()))
}

/// IPC endpoint of the machine daemon.
#[cfg(unix)]
pub fn daemon_endpoint() -> Result<String> {
    let _ = user_tag;
    Ok(root()?.join("run").join("cqd.sock").to_string_lossy().into_owned())
}

/// Creates all state directories.
pub fn ensure_dirs() -> Result<()> {
    for d in [sessions_dir()?, spool_dir()?, status_dir()?] {
        std::fs::create_dir_all(&d).with_context(|| format!("create {}", d.display()))?;
    }
    #[cfg(unix)]
    std::fs::create_dir_all(root()?.join("run"))?;
    Ok(())
}
