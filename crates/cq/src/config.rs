//! User configuration (`~/.codequilt/config.toml`): launch directory policy, subcommand aliases
//! and remote machines.

use crate::machine::MachineEntry;
use crate::paths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Directories under which the dashboard may start sessions. Defaults to the home directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allowed_roots: Vec<String>,
    /// Subcommand aliases (`q = "queen"`); the value may hold several words.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    aliases: HashMap<String, String>,
    /// Remote machines the daemon connects to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    migrate_legacy(root)?;
    let path = root.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("invalid {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e.into()),
    }
}

fn edit_in(root: &Path, f: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<()>) -> Result<()> {
    migrate_legacy(root)?;
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

/// Converts the former `config.json` and `machines.json` into `config.toml` once, keeping the
/// old files as `.bak`.
fn migrate_legacy(root: &Path) -> Result<()> {
    #[derive(Default, Deserialize)]
    struct Legacy {
        #[serde(default)]
        allowed_roots: Vec<String>,
        #[serde(default)]
        aliases: HashMap<String, String>,
    }
    let (old_config, old_machines) = (root.join("config.json"), root.join("machines.json"));
    if root.join(FILE).exists() || !(old_config.exists() || old_machines.exists()) {
        return Ok(());
    }
    let legacy: Legacy = match std::fs::read(&old_config) {
        Ok(d) => serde_json::from_slice(&d).context("invalid config.json")?,
        Err(_) => Legacy::default(),
    };
    let machines: Vec<MachineEntry> = match std::fs::read(&old_machines) {
        Ok(d) => serde_json::from_slice(&d).context("invalid machines.json")?,
        Err(_) => Vec::new(),
    };
    let config = Config { allowed_roots: legacy.allowed_roots, aliases: legacy.aliases, machines };
    let text = format!(
        "# CodeQuilt configuration (converted from config.json / machines.json).\n\n{}",
        toml::to_string_pretty(&config)?
    );
    std::fs::write(root.join(FILE), text)?;
    for old in [old_config, old_machines] {
        if old.exists() {
            std::fs::rename(&old, old.with_extension("json.bak"))?;
        }
    }
    eprintln!("cq: moved the configuration to {}", root.join(FILE).display());
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

/// Resolves `path` and verifies it is a directory inside an allowed root.
pub fn check_allowed(path: &str) -> Result<PathBuf> {
    let p = Path::new(path).canonicalize().with_context(|| format!("no such directory: {path}"))?;
    if !p.is_dir() {
        bail!("not a directory: {path}");
    }
    if !allowed_roots().iter().any(|r| p.starts_with(r)) {
        bail!("{path} is outside the allowed roots (see ~/.codequilt/config.toml)");
    }
    Ok(display_path(&p))
}

/// Lists subdirectories of an allowed directory, or the roots themselves for an empty path.
pub fn list_dir(path: &str) -> Result<cq_proto::DirList> {
    let roots: Vec<String> =
        allowed_roots().iter().map(|r| display_path(r).to_string_lossy().into_owned()).collect();
    if path.is_empty() {
        return Ok(cq_proto::DirList { id: 0, path: String::new(), parent: None, entries: roots.clone(), roots });
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
    Ok(cq_proto::DirList { id: 0, path: dir.to_string_lossy().into_owned(), parent, entries, roots })
}

/// Subcommand aliases: the built-in `q = queen` plus the user's own, which take precedence.
pub fn aliases() -> HashMap<String, String> {
    let mut all = HashMap::from([("q".to_owned(), "queen".to_owned())]);
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
    fn migrates_json_files_once() {
        let root = temp_root("migrate");
        std::fs::write(root.join("config.json"), r#"{"allowed_roots": ["C:\\work"], "aliases": {"qs": "queen start"}}"#).unwrap();
        std::fs::write(
            root.join("machines.json"),
            r#"[{"id": "wsl", "label": "wsl", "ssh": "wsl", "login_shell": true}]"#,
        )
        .unwrap();
        let c = load_in(&root).unwrap();
        assert_eq!(c.allowed_roots, [r"C:\work"]);
        assert_eq!(c.aliases["qs"], "queen start");
        assert_eq!(c.machines[0].id, "wsl");
        assert!(c.machines[0].login_shell);
        assert!(root.join("config.json.bak").exists() && root.join("machines.json.bak").exists());
        assert!(!root.join("config.json").exists());
        // A second load reads the TOML file only.
        assert_eq!(load_in(&root).unwrap().machines.len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn edit_keeps_comments() {
        let root = temp_root("edit");
        std::fs::write(root.join(FILE), "# my roots\nallowed_roots = [\"/w\"] # trailing\n").unwrap();
        edit_in(&root, |doc| {
            doc["aliases"]["x"] = toml_edit::value("queen");
            Ok(())
        })
        .unwrap();
        let text = std::fs::read_to_string(root.join(FILE)).unwrap();
        assert!(text.contains("# my roots") && text.contains("# trailing"), "{text}");
        assert_eq!(load_in(&root).unwrap().aliases["x"], "queen");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_file_is_empty_config() {
        let root = temp_root("empty");
        assert!(load_in(&root).unwrap().machines.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
