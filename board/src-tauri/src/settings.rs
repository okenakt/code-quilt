//! User interface settings (`~/.codequilt/ui.toml`) and key bindings
//! (`~/.codequilt/keybindings.toml`), watched and pushed to the frontend as `ui-config`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Emitter};

/// Both files as JSON, plus problems found while reading them.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UiConfig {
    pub ui: serde_json::Value,
    pub keybindings: Vec<serde_json::Value>,
    pub errors: Vec<String>,
}

fn file(name: &str) -> Option<PathBuf> {
    cq_protocol::local::state_root().ok().map(|r| r.join(name))
}

/// Reads a TOML file as JSON; a missing file is empty, a broken one is reported.
fn read_toml(name: &str, errors: &mut Vec<String>) -> serde_json::Value {
    let Some(path) = file(name) else { return serde_json::Value::Null };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return serde_json::Value::Null,
        Err(e) => {
            errors.push(format!("{name}: {e}"));
            return serde_json::Value::Null;
        }
    };
    match toml::from_str::<toml::Value>(&text) {
        Ok(v) => serde_json::to_value(v).unwrap_or_default(),
        Err(e) => {
            errors.push(format!("{name}: {e}"));
            serde_json::Value::Null
        }
    }
}

const DEFAULT_UI: &str = r#"# CodeQuilt board settings (docs/features.md, "~/.codequilt/ui.toml").

# Commands run in order when the board starts; failures are shown as toasts.
# The orchestrator uses Claude and asks you to confirm its channel when it starts.
[[on_start]]
command = "orchestrator.start"
args = { name = "orchestrator" }

[terminal]
# Fonts tried before the built-in fallbacks, e.g. ["HackGen Console NF"].
font_family = []
font_size = 13
"#;

/// Writes the default `ui.toml` when there is none, so every setting in effect is visible.
pub fn write_defaults() {
    if let Some(path) = file("ui.toml").filter(|p| !p.exists()) {
        let _ = path.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(path, DEFAULT_UI);
    }
}

/// Reads both files now.
pub fn load() -> UiConfig {
    let mut errors = Vec::new();
    let ui = read_toml("ui.toml", &mut errors);
    let keys = read_toml("keybindings.toml", &mut errors);
    let keybindings = match keys.get("binding") {
        Some(serde_json::Value::Array(list)) => list.clone(),
        Some(_) => {
            errors.push("keybindings.toml: `binding` must be a list of [[binding]] tables".into());
            Vec::new()
        }
        None => Vec::new(),
    };
    UiConfig { ui, keybindings, errors }
}

/// Edits `keybindings.toml` in place, keeping the user's comments and layout.
fn edit_bindings(f: impl FnOnce(&mut toml_edit::ArrayOfTables)) -> Result<(), String> {
    let path = file("keybindings.toml").ok_or("no home directory")?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("keybindings.toml: {e}"))?;
    let bindings = doc
        .entry("binding")
        .or_insert(toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or("keybindings.toml: `binding` must be a list of [[binding]] tables")?;
    f(bindings);
    std::fs::write(&path, doc.to_string()).map_err(|e| e.to_string())
}

fn is_binding(t: &toml_edit::Table, key: &str, command: &str) -> bool {
    t.get("key").and_then(|v| v.as_str()) == Some(key) && t.get("command").and_then(|v| v.as_str()) == Some(command)
}

/// A key to take away from a command.
#[derive(Deserialize)]
pub struct KeyOf {
    pub key: String,
    pub command: String,
}

/// Unbinds `key` from `command`: deletes the user's own binding, or for a default binding adds
/// a `-command` entry that removes it.
fn unbind(bindings: &mut toml_edit::ArrayOfTables, key: &str, command: &str) {
    let before = bindings.len();
    bindings.retain(|t| !is_binding(t, key, command));
    if bindings.len() == before {
        let mut t = toml_edit::Table::new();
        t["key"] = toml_edit::value(key);
        t["command"] = toml_edit::value(format!("-{command}"));
        bindings.push(t);
    }
}

/// Binds `key` to `command` with `args` after taking away the keys in `unbind` (the command's
/// old keys, and the key's previous command), in one edit of the file.
pub fn bind_key(
    key: &str,
    command: &str,
    args: &BTreeMap<String, String>,
    unbind_first: &[KeyOf],
) -> Result<(), String> {
    edit_bindings(|bindings| {
        for k in unbind_first {
            unbind(bindings, &k.key, &k.command);
        }
        // Drop the user's other bindings of this key, but keep `-command` entries removing defaults.
        bindings.retain(|t| {
            t.get("key").and_then(|v| v.as_str()) != Some(key)
                || t.get("command").and_then(|v| v.as_str()).is_some_and(|c| c.starts_with('-'))
        });
        let mut t = toml_edit::Table::new();
        t["key"] = toml_edit::value(key);
        t["command"] = toml_edit::value(command);
        if !args.is_empty() {
            let mut a = toml_edit::InlineTable::new();
            for (k, v) in args {
                a.insert(k, v.as_str().into());
            }
            t["args"] = toml_edit::value(a);
        }
        bindings.push(t);
    })
}

/// Takes `key` away from `command` (see `unbind`).
pub fn unbind_key(key: &str, command: &str) -> Result<(), String> {
    edit_bindings(|bindings| unbind(bindings, key, command))
}

fn stamp() -> [Option<SystemTime>; 2] {
    ["ui.toml", "keybindings.toml"].map(|n| file(n).and_then(|p| p.metadata().ok()).and_then(|m| m.modified().ok()))
}

/// Polls both files and emits `ui-config` whenever one changes.
pub fn watch(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last = stamp();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let now = stamp();
            if now != last {
                last = now;
                let _ = app.emit("ui-config", load());
            }
        }
    });
}
