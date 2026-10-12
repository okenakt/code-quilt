//! User configuration (`~/.codequilt/config.toml`): launch directory policy, subcommand aliases
//! and remote machines.

use crate::machine::MachineEntry;
use crate::paths;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    /// Directories under which the dashboard may start sessions. Defaults to the home directory.
    #[serde(default)]
    allowed_roots: Vec<String>,
    /// Subcommand aliases (`o = "orchestrator"`); the value may hold several words.
    #[serde(default)]
    aliases: HashMap<String, String>,
    /// Remote machines the daemon connects to.
    #[serde(default)]
    pub machines: Vec<MachineEntry>,
}

const FILE: &str = "config.toml";

/// Reads the configuration. A broken file is reported and treated as empty, so a typo cannot
/// stop sessions from running.
pub fn load() -> Config {
    match paths::root().and_then(|r| load_in(&r)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cq: ignoring the configuration: {e:#}");
            Config::default()
        }
    }
}

/// Edits the configuration file in place, keeping the user's comments and layout.
pub fn edit(f: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<()>) -> Result<()> {
    edit_in(&paths::root()?, f)
}

fn load_in(root: &Path) -> Result<Config> {
    let path = root.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("invalid {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e.into()),
    }
}

fn edit_in(root: &Path, f: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<()>) -> Result<()> {
    let path = root.join(FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("invalid {}", path.display()))?;
    f(&mut doc)?;
    std::fs::create_dir_all(root)?;
    std::fs::write(&path, doc.to_string())?;
    Ok(())
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

/// Existing directories the user trusted in Claude Code or Codex, outside CodeQuilt's own state
/// (e.g. orchestrator directories), canonical and sorted. Read anew on every call.
pub fn trusted_dirs() -> Vec<PathBuf> {
    let own = paths::root().ok().and_then(|r| r.canonicalize().ok());
    let mut dirs: Vec<PathBuf> = crate::usage::trusted_dirs()
        .into_iter()
        .filter_map(|d| d.canonicalize().ok())
        .filter(|d| d.is_dir() && !own.as_ref().is_some_and(|o| d.starts_with(o)))
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Resolves `path` and verifies it is a directory inside an allowed root or one the user trusted
/// in an agent. Both are read anew on every call, so edits apply right away.
pub fn check_allowed(path: &str) -> Result<PathBuf> {
    let p = Path::new(path).canonicalize().with_context(|| format!("no such directory: {path}"))?;
    if !p.is_dir() {
        bail!("not a directory: {path}");
    }
    if !allowed_roots().iter().any(|r| p.starts_with(r)) && !trusted_dirs().contains(&p) {
        bail!("{path} is neither inside the allowed roots (see ~/.codequilt/config.toml) nor trusted in an agent");
    }
    Ok(display_path(&p))
}

/// Lists subdirectories of an allowed directory, or for an empty path the roots and the trusted
/// directories.
pub fn list_dir(path: &str) -> Result<cq_protocol::DirList> {
    let shown = |ds: Vec<PathBuf>| -> Vec<String> { ds.iter().map(|d| display_path(d).to_string_lossy().into_owned()).collect() };
    let roots = shown(allowed_roots());
    if path.is_empty() {
        let known = shown(trusted_dirs());
        return Ok(cq_protocol::DirList { id: 0, path: String::new(), parent: None, entries: roots.clone(), roots, known });
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
    Ok(cq_protocol::DirList { id: 0, path: dir.to_string_lossy().into_owned(), parent, entries, roots, known: Vec::new() })
}

/// Subcommand aliases: the built-in `o = orchestrator` plus the user's own, which take precedence.
pub fn aliases() -> HashMap<String, String> {
    let mut all = HashMap::from([("o".to_owned(), "orchestrator".to_owned())]);
    all.extend(load().aliases);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cq-config-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn edit_keeps_comments() {
        let root = temp_root("edit");
        std::fs::write(root.join(FILE), "# my roots\nallowed_roots = [\"/w\"] # trailing\n").unwrap();
        edit_in(&root, |doc| {
            doc["aliases"]["x"] = toml_edit::value("orchestrator");
            Ok(())
        })
        .unwrap();
        let text = std::fs::read_to_string(root.join(FILE)).unwrap();
        assert!(text.contains("# my roots") && text.contains("# trailing"), "{text}");
        assert_eq!(load_in(&root).unwrap().aliases["x"], "orchestrator");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_file_is_empty_config() {
        let root = temp_root("empty");
        assert!(load_in(&root).unwrap().machines.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
