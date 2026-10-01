//! User configuration (`~/.codequilt/config.json`) and launch directory policy.

use crate::paths;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
struct Config {
    /// Directories under which the dashboard may start sessions. Defaults to the home directory.
    #[serde(default)]
    allowed_roots: Vec<String>,
}

fn load() -> Config {
    paths::root()
        .ok()
        .and_then(|r| std::fs::read(r.join("config.json")).ok())
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default()
}

/// Strips the `\\?\` prefix that `canonicalize` adds on Windows.
fn display_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

/// Canonical allowed roots (existing directories only).
pub fn allowed_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = load().allowed_roots.into_iter().map(PathBuf::from).collect();
    if roots.is_empty() {
        roots.extend(dirs::home_dir());
    }
    roots.into_iter().filter_map(|r| r.canonicalize().ok()).collect()
}

/// Resolves `path` and verifies it is a directory inside an allowed root.
pub fn check_allowed(path: &str) -> Result<PathBuf> {
    let p = Path::new(path).canonicalize().with_context(|| format!("no such directory: {path}"))?;
    if !p.is_dir() {
        bail!("not a directory: {path}");
    }
    if !allowed_roots().iter().any(|r| p.starts_with(r)) {
        bail!("{path} is outside the allowed roots (see ~/.codequilt/config.json)");
    }
    Ok(display_path(&p))
}

/// Lists subdirectories of an allowed directory, or the roots themselves for an empty path.
pub fn list_dir(path: &str) -> Result<cq_proto::DirList> {
    let roots: Vec<String> =
        allowed_roots().iter().map(|r| display_path(r).to_string_lossy().into_owned()).collect();
    if path.is_empty() {
        return Ok(cq_proto::DirList { path: String::new(), parent: None, entries: roots.clone(), roots });
    }
    let dir = check_allowed(path)?;
    let mut entries: Vec<String> = std::fs::read_dir(&dir)?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && !n.starts_with('$'))
        .collect();
    entries.sort_by_key(|n| n.to_lowercase());
    entries.truncate(1000);
    let parent = dir
        .parent()
        .filter(|p| check_allowed(&p.to_string_lossy()).is_ok())
        .map(|p| p.to_string_lossy().into_owned());
    Ok(cq_proto::DirList { path: dir.to_string_lossy().into_owned(), parent, entries, roots })
}
