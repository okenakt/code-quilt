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
    for d in [sessions_dir()?, root()?.join("logs")] {
        std::fs::create_dir_all(&d)?;
    }
    #[cfg(unix)]
    std::fs::create_dir_all(root()?.join("run"))?;
    Ok(())
}

/// Finds an executable named `prog` in `dirs`. On Windows the `PATHEXT` extensions are tried
/// first, because npm and Volta put an extensionless shell shim next to the real `.exe` / `.cmd`.
pub fn find_program(prog: &str, dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    dirs.into_iter()
        .flat_map(|dir| exts.iter().map(move |ext| dir.join(format!("{prog}{ext}"))))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}

/// Directories listed in `PATH`.
pub fn path_dirs() -> Vec<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect()
}
