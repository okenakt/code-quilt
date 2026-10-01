//! Connections from the dashboard to each machine's daemon, relayed to the frontend as events.

use anyhow::{Context, Result, bail};
use base64::Engine;
use cq_proto::local::{self, BoxStream};
use cq_proto::{ClientKind, Hello, Message, PROTOCOL_VERSION, Subscribe, spawn_reader, write_message};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

/// Id of the machine the dashboard runs on.
pub const LOCAL: &str = "local";

/// A machine entry from `~/.codequilt/machines.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineConfig {
    pub id: String,
    pub label: String,
    /// Host alias passed to `ssh`; `None` for the local machine.
    #[serde(default)]
    pub ssh: Option<String>,
    /// Path of `cq` on the remote machine (default: `cq` on PATH).
    #[serde(default)]
    pub cq_path: Option<String>,
    /// Arbitrary command whose stdio reaches a `cq bridge` (e.g. `["wsl", "cq", "bridge"]`).
    /// Takes precedence over `ssh`.
    #[serde(default)]
    pub command: Option<Vec<String>>,
}

/// Machine state reported to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct MachineInfo {
    pub id: String,
    pub label: String,
    pub online: bool,
    pub error: Option<String>,
}

#[derive(Clone, Serialize)]
struct SessionsPayload<'a> {
    machine: &'a str,
    sessions: &'a [cq_proto::SessionMeta],
}

#[derive(Clone, Serialize)]
struct DataPayload<'a> {
    machine: &'a str,
    session: &'a str,
    kind: &'a str,
    /// Base64 terminal bytes for `snapshot` and `output`.
    data: Option<String>,
    /// JSON body for control messages.
    json: Option<serde_json::Value>,
}

#[derive(Clone, Serialize)]
struct EventPayload<'a> {
    machine: &'a str,
    session: &'a str,
    event: &'a cq_proto::AgentEvent,
}

type Pending = Arc<Mutex<VecDeque<oneshot::Sender<Result<Message, String>>>>>;

struct Machine {
    /// Waiters for daemon replies to Launch / ListDir, answered in order.
    pending: Pending,
    info: Arc<Mutex<MachineInfo>>,
    tx: UnboundedSender<Message>,
    /// Active subscriptions (session -> size), re-established after reconnects.
    subs: Arc<Mutex<HashMap<String, (u16, u16)>>>,
}

/// Events kept per session for frontend reloads.
const CACHE_EVENTS: usize = 300;

/// What the backend has received so far, so a reloaded frontend can rebuild its state.
#[derive(Default)]
struct Cache {
    sessions: HashMap<String, Vec<cq_proto::SessionMeta>>,
    events: HashMap<(String, String), VecDeque<cq_proto::AgentEvent>>,
}

/// Kind of state event that only matters in its latest form.
fn latest_slot(ev: &cq_proto::AgentEvent) -> Option<&str> {
    match ev.source.as_str() {
        "statusline" => Some("statusline"),
        "cq" => ev.payload.get("type").and_then(|t| t.as_str()).filter(|t| matches!(*t, "account" | "tokens")),
        _ => None,
    }
}

#[derive(Serialize)]
pub struct ReplaySessions {
    machine: String,
    sessions: Vec<cq_proto::SessionMeta>,
}

#[derive(Serialize)]
pub struct ReplayEvent {
    machine: String,
    session: String,
    event: cq_proto::AgentEvent,
}

/// Cached sessions and events for a frontend that (re)starts after the backend connected.
#[derive(Serialize)]
pub struct Replay {
    sessions: Vec<ReplaySessions>,
    events: Vec<ReplayEvent>,
}

/// Owns one connection task per machine.
pub struct Hub {
    app: AppHandle,
    machines: Mutex<HashMap<String, Machine>>,
    cache: Mutex<Cache>,
}

impl Hub {
    /// Creates the hub and connects to the local machine and every configured remote.
    pub fn start(app: AppHandle) -> Arc<Self> {
        let hub = Arc::new(Self {
            app,
            machines: Mutex::new(HashMap::new()),
            cache: Mutex::new(Cache::default()),
        });
        let mut configs = vec![MachineConfig {
            id: LOCAL.into(),
            label: "local".into(),
            ssh: None,
            cq_path: None,
            command: None,
        }];
        configs.extend(load_machine_configs());
        for cfg in configs {
            hub.add_machine(cfg);
        }
        hub
    }

    fn add_machine(self: &Arc<Self>, cfg: MachineConfig) {
        let (tx, rx) = mpsc::unbounded_channel();
        let info = Arc::new(Mutex::new(MachineInfo {
            id: cfg.id.clone(),
            label: cfg.label.clone(),
            online: false,
            error: None,
        }));
        let subs = Arc::new(Mutex::new(HashMap::new()));
        let pending: Pending = Arc::default();
        self.machines
            .lock()
            .unwrap()
            .insert(cfg.id.clone(), Machine { info: info.clone(), tx, subs: subs.clone(), pending: pending.clone() });
        let hub = self.clone();
        tauri::async_runtime::spawn(async move { hub.machine_loop(cfg, info, subs, pending, rx).await });
    }

    /// Returns all machines and their connection state.
    pub fn machines(&self) -> Vec<MachineInfo> {
        let mut v: Vec<_> =
            self.machines.lock().unwrap().values().map(|m| m.info.lock().unwrap().clone()).collect();
        v.sort_by(|a, b| (a.id != LOCAL, &a.label).cmp(&(b.id != LOCAL, &b.label)));
        v
    }

    fn send(&self, machine: &str, msg: Message) -> Result<()> {
        let machines = self.machines.lock().unwrap();
        let m = machines.get(machine).context("unknown machine")?;
        m.tx.send(msg).ok().context("machine connection closed")
    }

    fn routed(&self, machine: &str, session: &str, inner: Message) -> Result<()> {
        self.send(machine, Message::Routed { session: session.into(), inner: Box::new(inner) })
    }

    /// Opens a terminal relay for a session.
    pub fn subscribe(&self, machine: &str, session: &str, cols: u16, rows: u16) -> Result<()> {
        if let Some(m) = self.machines.lock().unwrap().get(machine) {
            m.subs.lock().unwrap().insert(session.into(), (cols, rows));
        }
        self.send(
            machine,
            Message::Subscribe(Subscribe { session: session.into(), kind: ClientKind::Hub, cols, rows }),
        )
    }

    /// Closes a terminal relay.
    pub fn unsubscribe(&self, machine: &str, session: &str) -> Result<()> {
        if let Some(m) = self.machines.lock().unwrap().get(machine) {
            m.subs.lock().unwrap().remove(session);
        }
        self.send(machine, Message::Unsubscribe(session.into()))
    }

    /// Sends terminal input.
    pub fn input(&self, machine: &str, session: &str, data: Vec<u8>) -> Result<()> {
        self.routed(machine, session, Message::Input(data))
    }

    /// Reports a tile resize.
    pub fn resize(&self, machine: &str, session: &str, cols: u16, rows: u16) -> Result<()> {
        if let Some(m) = self.machines.lock().unwrap().get(machine) {
            if let Some(s) = m.subs.lock().unwrap().get_mut(session) {
                *s = (cols, rows);
            }
        }
        self.routed(machine, session, Message::Resize(cq_proto::Size { cols, rows }))
    }

    /// Answers a permission prompt.
    pub fn approve(&self, machine: &str, session: &str, option: u8) -> Result<()> {
        self.routed(machine, session, Message::Approve(cq_proto::Approve { option }))
    }

    /// Sends an arbitrary daemon request.
    pub fn request(&self, machine: &str, msg: Message) -> Result<()> {
        self.send(machine, msg)
    }

    /// Sends a request that the daemon answers with exactly one reply (or an error).
    pub async fn call(&self, machine: &str, msg: Message) -> Result<Message> {
        let (tx, rx) = oneshot::channel();
        {
            let machines = self.machines.lock().unwrap();
            let m = machines.get(machine).context("unknown machine")?;
            m.pending.lock().unwrap().push_back(tx);
            m.tx.send(msg).ok().context("machine connection closed")?;
        }
        match tokio::time::timeout(Duration::from_secs(15), rx).await {
            Ok(Ok(Ok(m))) => Ok(m),
            Ok(Ok(Err(e))) => bail!(e),
            Ok(Err(_)) => bail!("connection lost"),
            Err(_) => bail!("timed out"),
        }
    }

    fn set_status(&self, info: &Mutex<MachineInfo>, online: bool, error: Option<String>) {
        {
            let mut i = info.lock().unwrap();
            if i.online == online && i.error == error {
                return;
            }
            i.online = online;
            i.error = error;
        }
        let _ = self.app.emit("machines", self.machines());
    }

    async fn machine_loop(
        self: Arc<Self>,
        cfg: MachineConfig,
        info: Arc<Mutex<MachineInfo>>,
        subs: Arc<Mutex<HashMap<String, (u16, u16)>>>,
        pending: Pending,
        mut rx: UnboundedReceiver<Message>,
    ) {
        loop {
            let err = match open_transport(&cfg).await {
                Ok(stream) => {
                    // Hosts replay their history to the new connection; drop the stale copy.
                    self.cache.lock().unwrap().events.retain(|(m, _), _| m != &cfg.id);
                    self.set_status(&info, true, None);
                    let r = self.run_connection(&cfg.id, stream, &subs, &pending, &mut rx).await;
                    // Requests in flight will never be answered on this connection.
                    pending.lock().unwrap().clear();
                    match r {
                        Ok(()) => "disconnected".to_owned(),
                        Err(e) => format!("{e:#}"),
                    }
                }
                Err(e) => format!("{e:#}"),
            };
            self.set_status(&info, false, Some(err));
            // Drop commands issued while offline; subscriptions are replayed on reconnect.
            let retry = tokio::time::sleep(Duration::from_secs(3));
            tokio::pin!(retry);
            loop {
                tokio::select! {
                    _ = &mut retry => break,
                    m = rx.recv() => if m.is_none() { return },
                }
            }
        }
    }

    async fn run_connection(
        &self,
        machine: &str,
        stream: BoxStream,
        subs: &Mutex<HashMap<String, (u16, u16)>>,
        pending: &Pending,
        rx: &mut UnboundedReceiver<Message>,
    ) -> Result<()> {
        let (rd, mut wr) = tokio::io::split(stream);
        let hello = Hello { protocol_version: PROTOCOL_VERSION, kind: ClientKind::Hub, cols: 0, rows: 0 };
        write_message(&mut wr, &Message::Hello(hello)).await?;
        write_message(&mut wr, &Message::ListSessions).await?;
        let resubscribe: Vec<_> = subs.lock().unwrap().iter().map(|(k, v)| (k.clone(), *v)).collect();
        for (session, (cols, rows)) in resubscribe {
            let sub = Subscribe { session, kind: ClientKind::Hub, cols, rows };
            write_message(&mut wr, &Message::Subscribe(sub)).await?;
        }
        let mut msgs = spawn_reader(rd);
        loop {
            tokio::select! {
                msg = msgs.recv() => {
                    let Some(msg) = msg else { return Ok(()) };
                    self.dispatch(machine, msg?, pending)?;
                }
                cmd = rx.recv() => match cmd {
                    Some(m) => write_message(&mut wr, &m).await?,
                    None => return Ok(()),
                },
            }
        }
    }

    /// Returns everything cached so far.
    pub fn replay(&self) -> Replay {
        let cache = self.cache.lock().unwrap();
        Replay {
            sessions: cache
                .sessions
                .iter()
                .map(|(m, s)| ReplaySessions { machine: m.clone(), sessions: s.clone() })
                .collect(),
            events: cache
                .events
                .iter()
                .flat_map(|((m, s), evs)| {
                    evs.iter().map(|e| ReplayEvent { machine: m.clone(), session: s.clone(), event: e.clone() })
                })
                .collect(),
        }
    }

    fn cache_event(&self, machine: &str, session: &str, ev: &cq_proto::AgentEvent) {
        let mut cache = self.cache.lock().unwrap();
        let q = cache.events.entry((machine.to_owned(), session.to_owned())).or_default();
        if let Some(slot) = latest_slot(ev) {
            q.retain(|e| latest_slot(e) != Some(slot));
        }
        if q.len() == CACHE_EVENTS {
            q.pop_front();
        }
        q.push_back(ev.clone());
    }

    fn dispatch(&self, machine: &str, msg: Message, pending: &Pending) -> Result<()> {
        match msg {
            Message::SessionList(sessions) => {
                self.cache.lock().unwrap().sessions.insert(machine.to_owned(), sessions.clone());
                let _ = self.app.emit("sessions", SessionsPayload { machine, sessions: &sessions });
            }
            Message::Routed { session, inner } => self.dispatch_session(machine, &session, *inner),
            Message::Launched(_) | Message::DirList(_) => {
                if let Some(w) = pending.lock().unwrap().pop_front() {
                    let _ = w.send(Ok(msg));
                }
            }
            Message::Error(e) => match pending.lock().unwrap().pop_front() {
                Some(w) => {
                    let _ = w.send(Err(e));
                }
                // An unsolicited error (e.g. protocol mismatch) ends the connection.
                None => bail!("daemon error: {e}"),
            },
            other => {
                let _ = self.app.emit("daemon-message", format!("{other:?}"));
            }
        }
        Ok(())
    }

    fn dispatch_session(&self, machine: &str, session: &str, msg: Message) {
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let data = |kind: &str, data: Option<String>, json: Option<serde_json::Value>| {
            let _ = self.app.emit("session-data", DataPayload { machine, session, kind, data, json });
        };
        match msg {
            Message::Snapshot(b) => data("snapshot", Some(b64(&b)), None),
            Message::Output(b) => data("output", Some(b64(&b)), None),
            Message::Event(ev) => {
                self.cache_event(machine, session, &ev);
                let _ = self.app.emit("session-event", EventPayload { machine, session, event: &ev });
            }
            Message::Welcome(w) => data("welcome", None, serde_json::to_value(w).ok()),
            Message::InputOwner(o) => data("owner", None, serde_json::to_value(o).ok()),
            Message::Exit(e) => data("exit", None, serde_json::to_value(e).ok()),
            Message::Error(e) => data("error", None, Some(serde_json::Value::String(e))),
            _ => {}
        }
    }
}

fn load_machine_configs() -> Vec<MachineConfig> {
    let Ok(root) = local::state_root() else { return Vec::new() };
    let Ok(data) = std::fs::read(root.join("machines.json")) else { return Vec::new() };
    serde_json::from_slice::<Vec<MachineConfig>>(&data)
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.id != LOCAL && (m.ssh.is_some() || m.command.is_some()))
        .collect()
}

/// Locates the `cq` binary: `CQ_BIN`, next to this executable, or on PATH.
pub fn cq_binary() -> std::path::PathBuf {
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

fn spawn_daemon() -> Result<()> {
    let mut cmd = std::process::Command::new(cq_binary());
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        cmd.creation_flags(base | CREATE_BREAKAWAY_FROM_JOB);
        if cmd.spawn().is_ok() {
            return Ok(());
        }
        cmd.creation_flags(base);
    }
    cmd.spawn().context("start cq daemon")?;
    Ok(())
}

async fn open_transport(cfg: &MachineConfig) -> Result<BoxStream> {
    if let Some(cmd) = &cfg.command {
        let (prog, args) = cmd.split_first().context("empty machine command")?;
        return open_command(prog, args.iter().map(String::as_str)).await;
    }
    match &cfg.ssh {
        None => {
            let endpoint = local::daemon_endpoint()?;
            if let Ok(s) = local::connect(&endpoint, Duration::ZERO).await {
                return Ok(s);
            }
            spawn_daemon()?;
            local::connect(&endpoint, Duration::from_secs(5)).await
        }
        Some(host) => {
            let cq = cfg.cq_path.as_deref().unwrap_or("cq");
            // No port is opened on the remote side; the user's ssh key authentication applies.
            let args = ["-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15", host, cq, "bridge"];
            open_command("ssh", args.into_iter()).await
        }
    }
}

/// Child process stdio presented as one stream; the child is killed when dropped.
struct ChildStream {
    io: tokio::io::Join<tokio::process::ChildStdout, tokio::process::ChildStdin>,
    _child: tokio::process::Child,
}

impl AsyncRead for ChildStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for ChildStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// Runs a command (`ssh <host> cq bridge`, `wsl cq bridge`, ...) and uses its stdio as the daemon
/// connection.
async fn open_command<'a>(prog: &str, args: impl Iterator<Item = &'a str>) -> Result<BoxStream> {
    let mut cmd = tokio::process::Command::new(prog);
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().with_context(|| format!("spawn {prog}"))?;
    let stdout = child.stdout.take().context("child stdout")?;
    let stdin = child.stdin.take().context("child stdin")?;
    Ok(Box::new(ChildStream { io: tokio::io::join(stdout, stdin), _child: child }))
}
