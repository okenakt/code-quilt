//! User interface settings (`~/.codequilt/ui.toml`) and key bindings
//! (`~/.codequilt/keybindings.toml`), watched and pushed to the frontend as `ui-config`.

use serde::Serialize;
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
    cq_proto::local::state_root().ok().map(|r| r.join(name))
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
