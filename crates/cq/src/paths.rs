//! Filesystem and IPC endpoint locations under `~/.codequilt`.

use anyhow::Result;
use std::path::PathBuf;

pub use cq_proto::local::{daemon_endpoint, session_endpoint};

/// Root directory of CodeQuilt state (`~/.codequilt`).
pub fn root() -> Result<PathBuf> {
    cq_proto::local::state_root()
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

/// Creates all state directories.
pub fn ensure_dirs() -> Result<()> {
    for d in [sessions_dir()?, spool_dir()?, status_dir()?, root()?.join("logs")] {
        std::fs::create_dir_all(&d)?;
    }
    #[cfg(unix)]
    std::fs::create_dir_all(root()?.join("run"))?;
    Ok(())
}
