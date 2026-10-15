//! `cq machine`: registry of remote machines the daemon connects to via `ssh <host> cq bridge`.

use crate::config;
use crate::remote;
use anyhow::{Context, Result, bail};
use cq_protocol::LOCAL_MACHINE;
use serde::Deserialize;

/// Operating system of a machine, which decides how `cq bridge` is started there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    /// Started through `sh -lc` so the PATH set up in `~/.profile` reaches the agents.
    Unix,
    Windows,
}

impl Os {
    fn as_str(self) -> &'static str {
        match self {
            Os::Unix => "unix",
            Os::Windows => "windows",
        }
    }
}

/// One `[[machines]]` entry of `~/.codequilt/config.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct MachineEntry {
    /// Fixed at registration; what sessions, panes and pins refer to.
    pub id: String,
    /// What users see and type (`NAME:session`).
    pub name: String,
    /// `[user@]host` as given to ssh (an IP address, a host name or a `~/.ssh/config` alias).
    pub ssh: String,
    #[serde(default)]
    pub port: Option<u16>,
    pub os: Os,
    #[serde(default)]
    pub cq_path: Option<String>,
}

impl MachineEntry {
    /// Path of `cq` on the machine.
    pub fn cq(&self) -> &str {
        self.cq_path.as_deref().unwrap_or("cq")
    }

    fn destination(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{p}", self.ssh),
            None => self.ssh.clone(),
        }
    }
}

/// Loads the registered machines.
pub fn load() -> Vec<MachineEntry> {
    config::load().machines
}

fn find(machines: &[MachineEntry], name: &str) -> Result<MachineEntry> {
    machines.iter().find(|m| m.name == name).cloned().with_context(|| format!("no machine '{name}'"))
}

/// The `machines` array of the configuration document, created when missing.
fn machines_table(doc: &mut toml_edit::DocumentMut) -> Result<&mut toml_edit::ArrayOfTables> {
    doc.entry("machines")
        .or_insert(toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .context("`machines` in config.toml must be a list of [[machines]] tables")
}

/// Writes `e` over the entry with its id, or appends it.
fn save(e: &MachineEntry) -> Result<()> {
    config::edit(|doc| {
        let machines = machines_table(doc)?;
        let found = machines.iter().position(|t| t.get("id").and_then(|v| v.as_str()) == Some(e.id.as_str()));
        let i = match found {
            Some(i) => i,
            None => {
                machines.push(toml_edit::Table::new());
                machines.len() - 1
            }
        };
        let t = machines.get_mut(i).expect("present");
        t["id"] = toml_edit::value(e.id.as_str());
        t["name"] = toml_edit::value(e.name.as_str());
        t["ssh"] = toml_edit::value(e.ssh.as_str());
        match e.port {
            Some(p) => t["port"] = toml_edit::value(i64::from(p)),
            None => drop(t.remove("port")),
        }
        t["os"] = toml_edit::value(e.os.as_str());
        match &e.cq_path {
            Some(p) => t["cq_path"] = toml_edit::value(p.as_str()),
            None => drop(t.remove("cq_path")),
        }
        Ok(())
    })
}

/// Rejects names that cannot be typed in `NAME:session` or that another machine has.
fn check_name(machines: &[MachineEntry], name: &str, id: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        bail!("invalid machine name '{name}' (use letters, digits, '-', '_' and '.'); choose one with --name");
    }
    if name == LOCAL_MACHINE || machines.iter().any(|m| m.name == name && m.id != id) {
        bail!("machine name '{name}' is taken; choose another with --name");
    }
    Ok(())
}

/// A new id unlike any registered one.
fn new_id(machines: &[MachineEntry]) -> String {
    let mut n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_nanos();
    loop {
        let id = format!("m{:06x}", n & 0xff_ffff);
        if machines.iter().all(|m| m.id != id) {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

/// Connects to the machine as the daemon will and learns its OS. Returns the remote host name.
async fn verify(e: &mut MachineEntry) -> Result<String> {
    println!("connecting to {} ...", e.destination());
    let (hostname, os) = remote::probe(e).await?;
    e.os = os;
    remote::check(e).await?;
    Ok(hostname)
}

/// Prints registered machines.
pub fn list() -> Result<()> {
    let list = load();
    if list.is_empty() {
        println!("no remote machines registered; add one with `cq machine add`");
        return Ok(());
    }
    println!("{:<16} {:<32} {:<8} CQ", "NAME", "SSH", "OS");
    for m in list {
        println!("{:<16} {:<32} {:<8} {}", m.name, m.destination(), m.os.as_str(), m.cq());
    }
    Ok(())
}

/// Registers a machine after connecting to it. The name defaults to the remote host name.
pub async fn add(host: String, port: Option<u16>, name: Option<String>, cq_path: Option<String>) -> Result<()> {
    let machines = load();
    let mut e = MachineEntry { id: new_id(&machines), name: String::new(), ssh: host, port, os: Os::Unix, cq_path };
    if let Some(n) = &name {
        check_name(&machines, n, &e.id)?;
    }
    let hostname = verify(&mut e).await?;
    e.name = name.unwrap_or(hostname);
    check_name(&machines, &e.name, &e.id)?;
    save(&e)?;
    println!("added {} ({}, {})", e.name, e.destination(), e.os.as_str());
    println!("restart the board or run `cq daemon restart` to connect to it");
    Ok(())
}

/// Changes a registered machine. Changing how it is reached connects to it again first.
/// `port` 0 and an empty `cq_path` go back to the defaults.
pub async fn set(machine: &str, name: Option<String>, host: Option<String>, port: Option<u16>, cq_path: Option<String>) -> Result<()> {
    if name.is_none() && host.is_none() && port.is_none() && cq_path.is_none() {
        bail!("nothing to change; pass --name, --host, --port or --cq-path");
    }
    let machines = load();
    let mut e = find(&machines, machine)?;
    if let Some(n) = name {
        check_name(&machines, &n, &e.id)?;
        e.name = n;
    }
    let reconnect = host.is_some() || port.is_some() || cq_path.is_some();
    if let Some(h) = host {
        e.ssh = h;
    }
    if let Some(p) = port {
        e.port = (p != 0).then_some(p);
    }
    if let Some(p) = cq_path {
        e.cq_path = (!p.is_empty()).then_some(p);
    }
    if reconnect {
        verify(&mut e).await?;
    }
    save(&e)?;
    println!("updated {} ({}, {})", e.name, e.destination(), e.os.as_str());
    println!("restart the board or run `cq daemon restart` to apply it");
    Ok(())
}

/// Unregisters a machine.
pub fn remove(name: &str) -> Result<()> {
    let id = find(&load(), name)?.id;
    config::edit(|doc| {
        machines_table(doc)?.retain(|t| t.get("id").and_then(|v| v.as_str()) != Some(&id));
        Ok(())
    })?;
    println!("removed {name}");
    Ok(())
}

/// The name of machine `id` (the id itself when it is not registered).
pub fn name_of(id: &str) -> String {
    load().into_iter().find(|m| m.id == id).map_or_else(|| id.to_owned(), |m| m.name)
}

/// The id of the machine called `name`; [`LOCAL_MACHINE`] is this machine.
pub fn id_of(name: &str) -> Result<String> {
    if name == LOCAL_MACHINE {
        return Ok(name.to_owned());
    }
    Ok(find(&load(), name)?.id)
}
