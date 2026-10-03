//! `cq machine`: registry of remote machines the dashboard connects to via `ssh <host> cq bridge`.

use crate::config;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};

/// One `[[machines]]` entry of `~/.codequilt/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineEntry {
    pub id: String,
    pub label: String,
    pub ssh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cq_path: Option<String>,
    /// Start `cq bridge` through a login shell (`sh -lc`) so the remote PATH set up in
    /// `~/.profile` (e.g. Volta, `~/.local/bin`) reaches the daemon and the agents it starts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub login_shell: bool,
}

/// Loads the registered machines.
pub fn load() -> Vec<MachineEntry> {
    config::load().machines
}

/// The `machines` array of the configuration document, created when missing.
fn machines_table(doc: &mut toml_edit::DocumentMut) -> Result<&mut toml_edit::ArrayOfTables> {
    doc.entry("machines")
        .or_insert(toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .context("`machines` in config.toml must be a list of [[machines]] tables")
}

/// Concrete host aliases from `~/.ssh/config` (wildcard patterns are skipped).
pub fn ssh_config_hosts() -> Vec<String> {
    let Some(path) = dirs::home_dir().map(|h| h.join(".ssh").join("config")) else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let mut hosts = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let mut parts = line.split_whitespace();
        if !parts.next().is_some_and(|k| k.eq_ignore_ascii_case("host")) {
            continue;
        }
        for h in parts {
            if !h.contains(['*', '?', '!']) && !hosts.iter().any(|x| x == h) {
                hosts.push(h.to_owned());
            }
        }
    }
    hosts
}

/// Prints registered machines.
pub fn list() -> Result<()> {
    let list = load();
    if list.is_empty() {
        println!("no remote machines registered; add one with `cq machine add`");
    }
    for m in list {
        println!("{:<16} ssh {:<24} cq {}", m.label, m.ssh.unwrap_or_default(), m.cq_path.unwrap_or("cq".into()));
    }
    Ok(())
}

/// Registers a machine. Without `host`, offers the hosts from `~/.ssh/config` to choose from.
pub fn add(host: Option<String>, label: Option<String>, cq_path: Option<String>, login_shell: bool) -> Result<()> {
    let host = match host {
        Some(h) => h,
        None => {
            let candidates = ssh_config_hosts();
            if candidates.is_empty() {
                bail!("no hosts in ~/.ssh/config; pass one explicitly: cq machine add <host>");
            }
            for (i, h) in candidates.iter().enumerate() {
                println!("{:>3}. {h}", i + 1);
            }
            print!("select a host [1-{}]: ", candidates.len());
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line)?;
            let n: usize = line.trim().parse().context("not a number")?;
            candidates.get(n.wrapping_sub(1)).cloned().context("out of range")?
        }
    };
    if host == "local" || load().iter().any(|m| m.id == host) {
        bail!("machine '{host}' is already registered");
    }
    config::edit(|doc| {
        let mut t = toml_edit::Table::new();
        t["id"] = toml_edit::value(host.as_str());
        t["label"] = toml_edit::value(label.unwrap_or_else(|| host.clone()));
        t["ssh"] = toml_edit::value(host.as_str());
        if let Some(p) = cq_path {
            t["cq_path"] = toml_edit::value(p);
        }
        if login_shell {
            t["login_shell"] = toml_edit::value(true);
        }
        machines_table(doc)?.push(t);
        Ok(())
    })?;
    println!("added {host}; the dashboard connects with `ssh {host} cq bridge` (restart it to pick up changes)");
    Ok(())
}

/// Unregisters a machine.
pub fn remove(id: &str) -> Result<()> {
    config::edit(|doc| {
        let machines = machines_table(doc)?;
        let before = machines.len();
        machines.retain(|t| t.get("id").and_then(|v| v.as_str()) != Some(id));
        if machines.len() == before {
            bail!("no machine '{id}'");
        }
        Ok(())
    })?;
    println!("removed {id}");
    Ok(())
}
