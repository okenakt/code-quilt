//! Connection from the dashboard to the local daemon, which relays every machine. Messages are
//! forwarded to the frontend as events; a small cache lets a reloaded frontend rebuild its state.

use anyhow::{Context, Result, bail};
use base64::Engine;
use cq_proto::local::BoxStream;
use cq_proto::{
    AgentEvent, ClientKind, CommandInfo, DAEMON_PROTOCOL_VERSION, InvokeCommand, Hello, MachineInfo, Message, SessionMeta, SessionRef,
    Subscribe, spawn_reader, write_message,
};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

/// Events kept per session for frontend reloads.
const CACHE_EVENTS: usize = 300;

#[derive(Clone, Serialize)]
struct SessionsPayload<'a> {
    machine: &'a str,
    sessions: &'a [SessionMeta],
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
    event: &'a AgentEvent,
}

/// What the backend has received so far, so a reloaded frontend can rebuild its state.
#[derive(Default)]
struct Cache {
    machines: Vec<MachineInfo>,
    sessions: HashMap<String, Vec<SessionMeta>>,
    events: HashMap<(String, String), VecDeque<AgentEvent>>,
    commands: Vec<CommandInfo>,
}

/// Kind of state event that only matters in its latest form.
fn latest_slot(ev: &AgentEvent) -> Option<&str> {
    match ev.source.as_str() {
        "statusline" => Some("statusline"),
        "cq" => ev.payload.get("type").and_then(|t| t.as_str()).filter(|t| matches!(*t, "account" | "tokens")),
        _ => None,
    }
}

#[derive(Serialize)]
pub struct ReplaySessions {
    machine: String,
    sessions: Vec<SessionMeta>,
}

#[derive(Serialize)]
pub struct ReplayEvent {
    machine: String,
    session: String,
    event: AgentEvent,
}

/// Cached sessions and events for a frontend that (re)starts after the backend connected.
#[derive(Serialize)]
pub struct Replay {
    sessions: Vec<ReplaySessions>,
    events: Vec<ReplayEvent>,
}

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Message, String>>>>;

/// The dashboard's single connection to the local daemon.
pub struct Hub {
    app: AppHandle,
    tx: UnboundedSender<Message>,
    online: Mutex<(bool, Option<String>)>,
    /// Active subscriptions with their sizes, re-established after reconnects.
    subs: Mutex<HashMap<SessionRef, (u16, u16)>>,
    pending: Pending,
    next_id: AtomicU64,
    cache: Mutex<Cache>,
}

impl Hub {
    /// Creates the hub and keeps it connected to the local daemon.
    pub fn start(app: AppHandle) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let hub = Arc::new(Self {
            app,
            tx,
            online: Mutex::new((false, None)),
            subs: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            cache: Mutex::new(Cache::default()),
        });
        let h = hub.clone();
        tauri::async_runtime::spawn(async move { h.connection_loop(rx).await });
        hub
    }

    /// Returns all machines and their reachability. While the daemon itself is unreachable this
    /// reports a single offline local machine.
    pub fn machines(&self) -> Vec<MachineInfo> {
        let (online, error) = self.online.lock().unwrap().clone();
        let cached = self.cache.lock().unwrap().machines.clone();
        if online && !cached.is_empty() {
            return cached;
        }
        vec![MachineInfo { id: cq_proto::LOCAL_MACHINE.into(), label: "local".into(), online: false, error }]
    }

    fn send(&self, msg: Message) -> Result<()> {
        self.tx.send(msg).ok().context("daemon connection closed")
    }

    fn routed(&self, machine: &str, session: &str, inner: Message) -> Result<()> {
        self.send(Message::Routed { machine: machine.into(), session: session.into(), inner: Box::new(inner) })
    }

    /// Asks the daemon to resend all session lists.
    pub fn list_sessions(&self) -> Result<()> {
        self.send(Message::ListSessions)
    }

    /// Opens a terminal relay for a session.
    pub fn subscribe(&self, machine: &str, session: &str, cols: u16, rows: u16) -> Result<()> {
        let r = SessionRef { machine: machine.into(), session: session.into() };
        self.subs.lock().unwrap().insert(r, (cols, rows));
        self.send(Message::Subscribe(Subscribe {
            machine: machine.into(),
            session: session.into(),
            kind: ClientKind::Hub,
            cols,
            rows,
        }))
    }

    /// Closes a terminal relay.
    pub fn unsubscribe(&self, machine: &str, session: &str) -> Result<()> {
        let r = SessionRef { machine: machine.into(), session: session.into() };
        self.subs.lock().unwrap().remove(&r);
        self.send(Message::Unsubscribe(r))
    }

    /// Sends terminal input.
    pub fn input(&self, machine: &str, session: &str, data: Vec<u8>) -> Result<()> {
        self.routed(machine, session, Message::Input(data))
    }

    /// Reports a tile resize.
    pub fn resize(&self, machine: &str, session: &str, cols: u16, rows: u16) -> Result<()> {
        let r = SessionRef { machine: machine.into(), session: session.into() };
        if let Some(s) = self.subs.lock().unwrap().get_mut(&r) {
            *s = (cols, rows);
        }
        self.routed(machine, session, Message::Resize(cq_proto::Size { cols, rows }))
    }

    /// Answers a specific permission prompt.
    pub fn approve(&self, machine: &str, session: &str, option: u8, prompt_id: Option<String>) -> Result<()> {
        self.routed(machine, session, Message::Approve(cq_proto::Approve { option, prompt_id }))
    }

    /// Stops a machine's daemon; the connection that notices starts a new one.
    pub fn restart_daemon(&self, machine: String) -> Result<()> {
        self.send(Message::Shutdown(cq_proto::Shutdown { machine }))
    }

    /// Commands known to the daemon (plugin manifests and running clients).
    pub fn commands(&self) -> Vec<CommandInfo> {
        self.cache.lock().unwrap().commands.clone()
    }

    /// Runs a command through the daemon. Plain invocations wait for the outcome (plugins may
    /// run for up to two minutes); press and release of hold commands are sent without waiting.
    pub async fn invoke_command(&self, name: String, args: serde_json::Value, phase: String) -> Result<String> {
        let wait = phase == "invoke";
        let id = wait.then(|| self.next_id());
        let inv = InvokeCommand { name, phase, args, id, context: None };
        let Some(id) = id else {
            self.send(Message::InvokeCommand(inv))?;
            return Ok(String::new());
        };
        match self.call_within(id, Message::InvokeCommand(inv), Duration::from_secs(130)).await? {
            Message::CommandResult(r) => Ok(r.message),
            _ => Ok(String::new()),
        }
    }

    /// Asks a session to end gracefully; the host kills it after the grace period.
    pub fn terminate(&self, machine: &str, session: &str) -> Result<()> {
        let t = cq_proto::Terminate { grace_ms: 10_000, force: false };
        self.routed(machine, session, Message::Terminate(t))
    }

    /// Tells the daemon which session the user is typing into.
    pub fn set_focus(&self, focus: Option<SessionRef>) -> Result<()> {
        self.send(Message::SetFocus(focus))
    }

    /// Allocates a request id.
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Sends a request carrying `id` and waits for its reply.
    pub async fn call(&self, id: u64, msg: Message) -> Result<Message> {
        self.call_within(id, msg, Duration::from_secs(20)).await
    }

    async fn call_within(&self, id: u64, msg: Message, limit: Duration) -> Result<Message> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(e) = self.send(msg) {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        let r = tokio::time::timeout(limit, rx).await;
        self.pending.lock().unwrap().remove(&id);
        match r {
            Ok(Ok(Ok(m))) => Ok(m),
            Ok(Ok(Err(e))) => bail!(e),
            Ok(Err(_)) => bail!("connection lost"),
            Err(_) => bail!("timed out"),
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

    fn set_online(&self, online: bool, error: Option<String>) {
        *self.online.lock().unwrap() = (online, error);
        let _ = self.app.emit("machines", self.machines());
    }

    async fn connection_loop(self: Arc<Self>, mut rx: UnboundedReceiver<Message>) {
        loop {
            let err = match cq_client::connect_daemon().await {
                Ok(stream) => {
                    // Hosts replay their history to the new connection; drop the stale copy.
                    self.cache.lock().unwrap().events.clear();
                    match self.run_connection(stream, &mut rx).await {
                        Ok(()) => "disconnected".to_owned(),
                        Err(e) => format!("{e:#}"),
                    }
                }
                Err(e) => format!("{e:#}"),
            };
            for (_, w) in self.pending.lock().unwrap().drain() {
                let _ = w.send(Err("connection lost".into()));
            }
            self.set_online(false, Some(err));
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

    async fn run_connection(&self, stream: BoxStream, rx: &mut UnboundedReceiver<Message>) -> Result<()> {
        let (rd, mut wr) = tokio::io::split(stream);
        let hello = Hello { protocol_version: DAEMON_PROTOCOL_VERSION, kind: ClientKind::Hub, cols: 0, rows: 0, federate: true };
        write_message(&mut wr, &Message::Hello(hello)).await?;
        write_message(&mut wr, &Message::ListSessions).await?;
        let resubscribe: Vec<_> = self.subs.lock().unwrap().iter().map(|(k, v)| (k.clone(), *v)).collect();
        for (r, (cols, rows)) in resubscribe {
            let sub = Subscribe { machine: r.machine, session: r.session, kind: ClientKind::Hub, cols, rows };
            write_message(&mut wr, &Message::Subscribe(sub)).await?;
        }
        let mut msgs = spawn_reader(rd);
        loop {
            tokio::select! {
                msg = msgs.recv() => {
                    let Some(msg) = msg else { return Ok(()) };
                    self.dispatch(msg?)?;
                }
                cmd = rx.recv() => match cmd {
                    Some(m) => write_message(&mut wr, &m).await?,
                    None => return Ok(()),
                },
            }
        }
    }

    fn reply(&self, id: u64, r: Result<Message, String>) {
        if let Some(w) = self.pending.lock().unwrap().remove(&id) {
            let _ = w.send(r);
        }
    }

    fn dispatch(&self, msg: Message) -> Result<()> {
        match msg {
            Message::MachineList(list) => {
                self.cache.lock().unwrap().machines = list;
                self.set_online(true, None);
            }
            Message::SessionList(l) => {
                self.cache.lock().unwrap().sessions.insert(l.machine.clone(), l.sessions.clone());
                let _ = self.app.emit("sessions", SessionsPayload { machine: &l.machine, sessions: &l.sessions });
            }
            Message::Routed { machine, session, inner } => self.dispatch_session(&machine, &session, *inner),
            Message::Launched(l) => self.reply(l.id, Ok(Message::Launched(l))),
            Message::DirList(l) => self.reply(l.id, Ok(Message::DirList(l))),
            Message::RequestFailed(f) => self.reply(f.id, Err(f.error)),
            Message::Focus(f) => {
                let _ = self.app.emit("focus", f);
            }
            Message::CommandList(list) => {
                self.cache.lock().unwrap().commands = list.clone();
                let _ = self.app.emit("commands", list);
            }
            Message::CommandResult(r) => {
                let id = r.id;
                self.reply(id, if r.ok { Ok(Message::CommandResult(r)) } else { Err(r.message) });
            }
            // An unsolicited error (e.g. protocol mismatch) ends the connection.
            Message::Error(e) => bail!("daemon error: {e}"),
            _ => {}
        }
        Ok(())
    }

    fn cache_event(&self, machine: &str, session: &str, ev: &AgentEvent) {
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

