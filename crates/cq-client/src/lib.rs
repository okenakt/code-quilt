//! Client library for CodeQuilt: connect to the local daemon and keep a model of every session
//! built from the daemon's messages. Used by `cq mcp`, `cq-queen`, and other clients.

use anyhow::{Context, Result};
use cq_proto::local::{self, BoxStream};
use cq_proto::{AgentEvent, MachineInfo, Message, SessionMeta, SessionStatus};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

/// Turns kept per session.
pub const MAX_TURNS: usize = 20;

/// One exchange: what the user (or an orchestrator) asked, and the agent's final reply.
#[derive(Debug, Clone, Default)]
pub struct Turn {
    pub prompt: Option<String>,
    pub reply: Option<String>,
    pub at_ms: u64,
}

/// What a session is doing, derived from its events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Starting,
    Working,
    Idle,
    /// A permission prompt or confirmation dialog is waiting.
    Waiting,
    Exited,
}

impl Activity {
    /// Short lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Activity::Starting => "starting",
            Activity::Working => "working",
            Activity::Idle => "idle",
            Activity::Waiting => "approval",
            Activity::Exited => "exited",
        }
    }
}

/// A session as seen by clients.
#[derive(Debug, Clone)]
pub struct Session {
    pub machine: String,
    pub meta: SessionMeta,
    pub activity: Activity,
    /// Pending prompt (`id`, `kind`, `text`, `options`) as reported by the host.
    pub prompt: Option<Value>,
    pub turns: VecDeque<Turn>,
}

impl Session {
    /// `machine:id`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.machine, self.meta.id)
    }

    /// The user-given name, or the id.
    pub fn label(&self) -> &str {
        self.meta.name.as_deref().unwrap_or(&self.meta.id)
    }
}

/// Sessions and machines, kept up to date from daemon messages.
#[derive(Debug, Default)]
pub struct Model {
    pub sessions: HashMap<(String, String), Session>,
    pub machines: Vec<MachineInfo>,
    /// The local machine's session list has arrived.
    pub synced: bool,
}

impl Model {
    /// Applies a daemon message; returns the session event it carried, if any.
    pub fn apply(&mut self, msg: &Message) -> Option<(String, String, AgentEvent)> {
        match msg {
            Message::MachineList(l) => self.machines = l.clone(),
            Message::SessionList(l) => self.apply_list(&l.machine, l.sessions.clone()),
            Message::Routed { machine, session, inner } => {
                if let Message::Event(ev) = inner.as_ref() {
                    self.apply_event(machine, session, ev);
                    return Some((machine.clone(), session.clone(), ev.clone()));
                }
                if let Message::Exit(_) = inner.as_ref() {
                    if let Some(s) = self.sessions.get_mut(&(machine.clone(), session.clone())) {
                        s.activity = Activity::Exited;
                        s.meta.status = SessionStatus::Exited;
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn apply_list(&mut self, machine: &str, list: Vec<SessionMeta>) {
        let keep: Vec<_> = list.iter().map(|m| (machine.to_owned(), m.id.clone())).collect();
        self.sessions.retain(|k, _| k.0 != machine || keep.contains(k));
        for meta in list {
            let key = (machine.to_owned(), meta.id.clone());
            let exited = meta.status == SessionStatus::Exited;
            let entry = self.sessions.entry(key).or_insert_with(|| Session {
                machine: machine.to_owned(),
                meta: meta.clone(),
                // Codex fires SessionStart only with its first turn, so it starts out idle.
                activity: if meta.agent == "codex" { Activity::Idle } else { Activity::Starting },
                prompt: None,
                turns: VecDeque::new(),
            });
            entry.meta = meta;
            if exited {
                entry.activity = Activity::Exited;
                entry.prompt = None;
            }
        }
        if machine == cq_proto::LOCAL_MACHINE {
            self.synced = true;
        }
    }

    fn apply_event(&mut self, machine: &str, session: &str, ev: &AgentEvent) {
        let Some(s) = self.sessions.get_mut(&(machine.to_owned(), session.to_owned())) else { return };
        let text = |k: &str| ev.payload.get(k).and_then(Value::as_str).map(str::to_owned);
        match (ev.source.as_str(), ev.payload.get("hook_event_name").and_then(Value::as_str)) {
            ("hook", Some("SessionStart")) => s.activity = Activity::Idle,
            ("hook", Some("UserPromptSubmit")) => {
                s.activity = Activity::Working;
                s.turns.push_back(Turn { prompt: text("prompt"), reply: None, at_ms: ev.received_at_ms });
                while s.turns.len() > MAX_TURNS {
                    s.turns.pop_front();
                }
            }
            ("hook", Some("Stop")) => {
                s.activity = Activity::Idle;
                let reply = text("last_assistant_message");
                match s.turns.back_mut() {
                    Some(t) if t.reply.is_none() => t.reply = reply,
                    _ => s.turns.push_back(Turn { prompt: None, reply, at_ms: ev.received_at_ms }),
                }
            }
            ("cq", _) => match ev.payload.get("type").and_then(Value::as_str) {
                Some("permission_prompt") => {
                    s.prompt = ev.payload.get("prompt").cloned();
                    s.activity = Activity::Waiting;
                }
                Some("permission_resolved") => {
                    s.prompt = None;
                    if s.activity == Activity::Waiting {
                        let idle = ev.payload.get("idle").and_then(Value::as_bool) == Some(true);
                        s.activity = if idle { Activity::Idle } else { Activity::Working };
                    }
                }
                Some("meta") => s.meta.name = text("name"),
                _ => {}
            },
            _ => {}
        }
        if s.meta.status == SessionStatus::Exited {
            s.activity = Activity::Exited;
        }
    }
}

/// Locates the `cq` binary: `CQ_BIN`, next to this executable, or on PATH.
pub fn cq_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("CQ_BIN") {
        return p.into();
    }
    let name = if cfg!(windows) { "cq.exe" } else { "cq" };
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) {
        let p = dir.join(name);
        if p.is_file() {
            return p;
        }
    }
    name.into()
}

/// Keeps this process's standard handles out of children it spawns. Windows children inherit
/// every inheritable handle of the parent, so a detached process would otherwise hold e.g. the
/// caller's stdout pipe open and a shell reading from it would never finish. Rust still hands
/// children their own standard handles explicitly, so normal spawns are unaffected.
#[cfg(windows)]
pub fn disinherit_std_handles() {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        unsafe {
            let h = GetStdHandle(which);
            if !h.is_null() {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Log file `~/.codequilt/logs/<name>.log`, with its directory created.
pub fn log_file(name: &str) -> Result<PathBuf> {
    let dir = local::state_root()?.join("logs");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{name}.log")))
}

/// Starts a process detached from this console and process tree, with stderr going to `log`.
pub fn spawn_detached(cmd: &mut std::process::Command, log: &std::path::Path) -> Result<()> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(log).with_context(|| format!("create {}", log.display()))?);
    #[cfg(windows)]
    {
        disinherit_std_handles();
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        // Break away from the terminal's job so closing it does not kill the process; fall back
        // if the job forbids breakaway.
        cmd.creation_flags(base | CREATE_BREAKAWAY_FROM_JOB);
        if cmd.spawn().is_ok() {
            return Ok(());
        }
        cmd.creation_flags(base);
        cmd.spawn().context("spawn detached process")?;
        Ok(())
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        // Reap the child when it exits; a long-lived caller such as the daemon would otherwise keep
        // zombies.
        let mut child = cmd.spawn().context("spawn detached process")?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// Connects to the local daemon, starting `cq daemon` if it is not running.
pub async fn connect_daemon() -> Result<BoxStream> {
    let endpoint = local::daemon_endpoint()?;
    if let Ok(s) = local::connect(&endpoint, Duration::ZERO).await {
        return Ok(s);
    }
    spawn_detached(std::process::Command::new(cq_binary()).arg("daemon"), &log_file("daemon")?)?;
    local::connect(&endpoint, Duration::from_secs(5)).await
}

/// A hub connection to the local daemon.
pub struct DaemonClient {
    pub msgs: tokio::sync::mpsc::UnboundedReceiver<Result<Message>>,
    wr: tokio::io::WriteHalf<BoxStream>,
    federate: bool,
}

impl DaemonClient {
    /// Connects (starting the daemon if needed) and says hello. With `federate` the daemon also
    /// relays every registered remote machine.
    pub async fn open(federate: bool) -> Result<Self> {
        let (rd, wr) = tokio::io::split(connect_daemon().await?);
        let mut client = Self { msgs: cq_proto::spawn_reader(rd), wr, federate };
        let hello = cq_proto::Hello {
            protocol_version: cq_proto::DAEMON_PROTOCOL_VERSION,
            kind: cq_proto::ClientKind::Hub,
            cols: 0,
            rows: 0,
            federate,
        };
        client.send(&Message::Hello(hello)).await?;
        Ok(client)
    }

    pub async fn send(&mut self, msg: &Message) -> Result<()> {
        cq_proto::write_message(&mut self.wr, msg).await
    }

    /// Launches a session through the daemon and returns its id.
    pub async fn launch(&mut self, launch: cq_proto::Launch) -> Result<String> {
        let id = launch.id;
        self.send(&Message::Launch(launch)).await?;
        while let Some(msg) = self.msgs.recv().await {
            match msg? {
                Message::Launched(l) if l.id == id => return Ok(l.session),
                Message::RequestFailed(f) if f.id == id => anyhow::bail!(f.error),
                _ => {}
            }
        }
        anyhow::bail!("daemon connection closed")
    }

    /// Connects again after the daemon went away (e.g. `cq daemon restart`), starting it if
    /// needed and retrying every second until it succeeds.
    pub async fn reconnect(&mut self) {
        loop {
            match Self::open(self.federate).await {
                Ok(c) => return *self = c,
                Err(e) => eprintln!("daemon unreachable: {e:#}"),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

/// Which machines' sessions an orchestrator may see and drive (`machine:ID` entries; none
/// means every machine).
#[derive(Debug, Clone, Default)]
pub struct Scope {
    machines: Vec<String>,
}

impl Scope {
    pub fn parse(entries: &[String]) -> Result<Self> {
        let machines = entries
            .iter()
            .map(|e| e.strip_prefix("machine:").map(str::to_owned).with_context(|| format!("invalid scope '{e}' (use machine:ID)")))
            .collect::<Result<_>>()?;
        Ok(Self { machines })
    }

    pub fn admits(&self, machine: &str) -> bool {
        self.machines.is_empty() || self.machines.iter().any(|m| m == machine)
    }
}
