//! `cq machine`: registry of remote machines the dashboard connects to via `ssh <host> cq bridge`.

use crate::paths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};

/// One entry of `~/.codequilt/machines.json` (read by the dashboard backend).
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

fn file() -> Result<std::path::PathBuf> {
    Ok(paths::root()?.join("machines.json"))
}

/// Loads the registered machines.
pub fn load() -> Result<Vec<MachineEntry>> {
    match std::fs::read(file()?) {
        Ok(d) => Ok(serde_json::from_slice(&d).context("invalid machines.json")?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

fn save(list: &[MachineEntry]) -> Result<()> {
    std::fs::create_dir_all(paths::root()?)?;
    std::fs::write(file()?, serde_json::to_vec_pretty(list)?)?;
    Ok(())
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
    let list = load()?;
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
    let mut list = load()?;
    if host == "local" || list.iter().any(|m| m.id == host) {
        bail!("machine '{host}' is already registered");
    }
    list.push(MachineEntry { id: host.clone(), label: label.unwrap_or_else(|| host.clone()), ssh: Some(host.clone()), cq_path, login_shell });
    save(&list)?;
    println!("added {host}; the dashboard connects with `ssh {host} cq bridge` (restart it to pick up changes)");
    Ok(())
}

/// Unregisters a machine.
pub fn remove(id: &str) -> Result<()> {
    let mut list = load()?;
    let before = list.len();
    list.retain(|m| m.id != id);
    if list.len() == before {
        bail!("no machine '{id}'");
    }
    save(&list)?;
    println!("removed {id}");
    Ok(())
}
