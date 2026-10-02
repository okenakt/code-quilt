//! Machine daemon (`cq daemon`) and its stdio bridge (`cq bridge`).
//!
//! The daemon discovers session hosts from `~/.codequilt/sessions` and relays them to clients over
//! a single connection. For clients that ask for it (`federate`), it also connects to every
//! registered remote machine and relays those too, so clients only ever talk to the local daemon.
//! It keeps no state of its own: event history lives in each host, so a restarted daemon (or
//! client) simply reconnects and receives the replay.

use crate::machine::{self, MachineEntry};
use crate::{ipc, paths, remote, session};
use anyhow::{Context, Result, bail};
use cq_proto::{
    ClientKind, DirList, Hello, DAEMON_PROTOCOL_VERSION, LOCAL_MACHINE, Launched, MachineInfo, Message, PROTOCOL_VERSION,
    RequestFailed, SessionList, SessionMeta, SessionStatus, Subscribe, read_message, spawn_reader,
    write_message,
};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

/// Runs the daemon. Returns immediately if another daemon already owns the endpoint.
pub async fn run() -> Result<()> {
    paths::ensure_dirs()?;
    let endpoint = paths::daemon_endpoint()?;
    let mut listener = match ipc::Listener::bind(&endpoint) {
        Ok(l) => l,
        Err(e) => {
            if ipc::connect_now(&endpoint).await.is_ok() {
                return Ok(());
            }
            return Err(e);
        }
    };
    loop {
        let stream = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = serve(stream).await {
                eprintln!("daemon client error: {e:#}");
            }
        });
    }
}

/// Connects to the local daemon, starting it if needed.
pub async fn connect() -> Result<ipc::BoxStream> {
    let endpoint = paths::daemon_endpoint()?;
    if let Ok(s) = ipc::connect_now(&endpoint).await {
        return Ok(s);
    }
    let exe = std::env::current_exe()?;
    let log = paths::root()?.join("logs").join("daemon.log");
    crate::launch::spawn_detached(std::process::Command::new(exe).arg("daemon"), &log)?;
    ipc::connect(&endpoint, Duration::from_secs(5)).await.context("daemon did not start")
}

/// `cq bridge`: relays stdin/stdout to the local daemon (used as `ssh host cq bridge`).
pub async fn bridge() -> Result<()> {
    let stream = connect().await?;
    let (mut rd, mut wr) = tokio::io::split(stream);
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    tokio::select! {
        r = tokio::io::copy(&mut stdin, &mut wr) => { r?; }
        r = tokio::io::copy(&mut rd, &mut stdout) => { r?; }
    }
    Ok(())
}

struct Conn {
    tx: UnboundedSender<Message>,
    task: JoinHandle<()>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn routed(session: &str, inner: Message) -> Message {
    Message::Routed { machine: LOCAL_MACHINE.to_owned(), session: session.to_owned(), inner: Box::new(inner) }
}

/// Opens a host connection of the given kind and relays it as `Routed` messages.
fn open_host(meta: &SessionMeta, kind: ClientKind, cols: u16, rows: u16, out: UnboundedSender<Message>) -> Conn {
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let session = meta.id.clone();
    let pipe = meta.pipe.clone();
    let task = tokio::spawn(async move {
        let run = async {
            let stream = ipc::connect(&pipe, Duration::from_secs(1)).await?;
            let (rd, mut wr) = tokio::io::split(stream);
            let mut msgs = spawn_reader(rd);
            let hello = Hello { protocol_version: PROTOCOL_VERSION, kind, cols, rows, federate: false };
            write_message(&mut wr, &Message::Hello(hello)).await?;
            loop {
                tokio::select! {
                    msg = msgs.recv() => {
                        let Some(msg) = msg else { break };
                        let msg = msg?;
                        // Events arrive via the monitor connection; drop duplicates elsewhere.
                        let forward = match (&msg, kind) {
                            (Message::Event(_), ClientKind::Monitor) => true,
                            (Message::Event(_), _) => false,
                            (Message::Output(_) | Message::Snapshot(_), ClientKind::Monitor) => false,
                            _ => true,
                        };
                        if forward {
                            let _ = out.send(routed(&session, msg));
                        }
                    }
                    msg = rx.recv() => match msg {
                        Some(m) => write_message(&mut wr, &m).await?,
                        None => break,
                    },
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        if let Err(e) = run.await {
            let _ = out.send(routed(&session, Message::Error(format!("{e:#}"))));
        }
    });
    Conn { tx, task }
}

/// One client's relay to the sessions of this machine.
struct LocalRelay {
    out: UnboundedSender<Message>,
    monitors: HashMap<String, Conn>,
    /// Hosts whose monitor connection ended recently (e.g. incompatible version); retried later.
    backoff: HashMap<String, tokio::time::Instant>,
    subs: HashMap<String, Conn>,
    sessions: Vec<SessionMeta>,
    last_key: String,
}

impl LocalRelay {
    fn new(out: UnboundedSender<Message>) -> Self {
        Self {
            out,
            monitors: HashMap::new(),
            backoff: HashMap::new(),
            subs: HashMap::new(),
            sessions: Vec::new(),
            last_key: String::new(),
        }
    }

    fn send_list(&self) {
        let list = SessionList { machine: LOCAL_MACHINE.to_owned(), sessions: self.sessions.clone() };
        let _ = self.out.send(Message::SessionList(list));
    }

    /// Rescans sessions, reports list changes, and keeps one monitor per running host.
    fn refresh(&mut self) {
        self.sessions = session::list().unwrap_or_default();
        let key = serde_json::to_string(&self.sessions).unwrap_or_default();
        if key != self.last_key {
            self.last_key = key;
            self.send_list();
        }
        let now = tokio::time::Instant::now();
        let (sessions, backoff) = (&self.sessions, &mut self.backoff);
        self.monitors.retain(|id, c| {
            let keep = !c.task.is_finished() && sessions.iter().any(|m| &m.id == id);
            if !keep {
                backoff.insert(id.clone(), now + Duration::from_secs(30));
            }
            keep
        });
        self.backoff.retain(|_, until| *until > now);
        for m in self.sessions.iter().filter(|m| m.status == SessionStatus::Running) {
            if !self.monitors.contains_key(&m.id) && !self.backoff.contains_key(&m.id) {
                self.monitors.insert(m.id.clone(), open_host(m, ClientKind::Monitor, 0, 0, self.out.clone()));
            }
        }
        self.subs.retain(|_, c| !c.task.is_finished());
    }

    /// Connection that should carry a session message. Control messages go through the monitor
    /// so they work without a subscription; so does input for sessions without an open tile.
    fn route(&self, session: &str, inner: &Message) -> Option<&Conn> {
        match inner {
            Message::Approve(_) | Message::SetMeta(_) | Message::Inject(_) => self.monitors.get(session),
            Message::Input(_) => self.subs.get(session).or_else(|| self.monitors.get(session)),
            _ => self.subs.get(session),
        }
    }

    fn handle(&mut self, msg: Message) {
        match msg {
            Message::ListSessions => {
                self.sessions = session::list().unwrap_or_default();
                self.send_list();
            }
            Message::Subscribe(Subscribe { session, kind, cols, rows, .. }) => {
                if self.sessions.iter().all(|m| m.id != session) {
                    self.sessions = session::list().unwrap_or_default();
                }
                match self.sessions.iter().find(|m| m.id == session) {
                    Some(meta) => {
                        let kind = if kind == ClientKind::Event { ClientKind::Hub } else { kind };
                        self.subs.insert(session.clone(), open_host(meta, kind, cols, rows, self.out.clone()));
                    }
                    None => {
                        let _ = self.out.send(routed(&session, Message::Error("unknown session".into())));
                    }
                }
            }
            Message::Unsubscribe(r) => {
                self.subs.remove(&r.session);
            }
            Message::Launch(req) => {
                let reply = match crate::launch::launch_detached(&req) {
                    Ok(session) => Message::Launched(Launched { id: req.id, machine: LOCAL_MACHINE.to_owned(), session }),
                    Err(e) => Message::RequestFailed(RequestFailed { id: req.id, error: format!("launch failed: {e:#}") }),
                };
                let _ = self.out.send(reply);
            }
            Message::ListDir(req) => {
                let reply = match crate::config::list_dir(&req.path) {
                    Ok(l) => Message::DirList(DirList { id: req.id, ..l }),
                    Err(e) => Message::RequestFailed(RequestFailed { id: req.id, error: format!("{e:#}") }),
                };
                let _ = self.out.send(reply);
            }
            Message::Routed { session, inner, .. } => {
                if self.route(&session, &inner).is_none() {
                    // The host may have started after the last scan.
                    self.refresh();
                }
                if let Some(c) = self.route(&session, &inner) {
                    let _ = c.tx.send(*inner);
                }
            }
            _ => {}
        }
    }
}

/// Machine id a client request is addressed to.
fn target_machine(msg: &Message) -> Option<&str> {
    match msg {
        Message::Subscribe(s) => Some(&s.machine),
        Message::Unsubscribe(r) => Some(&r.machine),
        Message::Routed { machine, .. } => Some(machine),
        Message::Launch(l) => Some(&l.machine),
        Message::ListDir(l) => Some(&l.machine),
        _ => None,
    }
}

/// State shared by all clients of this daemon: the desktop's focus and registered commands.
#[derive(Default)]
struct Desktop {
    next_client: u64,
    clients: HashMap<u64, UnboundedSender<Message>>,
    focus: Option<cq_proto::SessionRef>,
    commands: HashMap<String, (u64, cq_proto::CommandInfo)>,
}

impl Desktop {
    fn broadcast(&self, msg: &Message) {
        for tx in self.clients.values() {
            let _ = tx.send(msg.clone());
        }
    }

    fn command_list(&self) -> Message {
        let mut list: Vec<_> = self.commands.values().map(|(_, c)| c.clone()).collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        Message::CommandList(list)
    }
}

fn desktop() -> &'static std::sync::Mutex<Desktop> {
    static DESKTOP: std::sync::OnceLock<std::sync::Mutex<Desktop>> = std::sync::OnceLock::new();
    DESKTOP.get_or_init(Default::default)
}

/// Handles desktop-level messages; returns the message back if it is not one.
fn handle_desktop(client: u64, msg: Message) -> Option<Message> {
    let mut d = desktop().lock().unwrap();
    match msg {
        Message::SetFocus(f) => {
            if d.focus != f {
                d.focus = f.clone();
                d.broadcast(&Message::Focus(f));
            }
        }
        Message::RegisterCommand(c) => {
            d.commands.insert(c.name.clone(), (client, c));
            let list = d.command_list();
            d.broadcast(&list);
        }
        Message::InvokeCommand(inv) => {
            if let Some((owner, _)) = d.commands.get(&inv.name) {
                if let Some(tx) = d.clients.get(owner) {
                    let _ = tx.send(Message::InvokeCommand(inv));
                }
            }
        }
        other => return Some(other),
    }
    None
}

fn local_info() -> MachineInfo {
    let label = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| LOCAL_MACHINE.to_owned());
    MachineInfo { id: LOCAL_MACHINE.to_owned(), label, online: true, error: None }
}

async fn serve(stream: ipc::BoxStream) -> Result<()> {
    let (mut rd, mut wr) = tokio::io::split(stream);
    let hello = match tokio::time::timeout(Duration::from_secs(5), read_message(&mut rd)).await?? {
        Some(Message::Hello(h)) => h,
        _ => bail!("expected hello"),
    };
    if hello.protocol_version != DAEMON_PROTOCOL_VERSION {
        let msg = format!("protocol mismatch: daemon {DAEMON_PROTOCOL_VERSION}, client {}", hello.protocol_version);
        write_message(&mut wr, &Message::Error(msg.clone())).await?;
        bail!(msg);
    }

    let (out, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if write_message(&mut wr, &m).await.is_err() {
                break;
            }
        }
    });

    // Remote machines, one relay task per machine for this client.
    let entries: Vec<MachineEntry> = if hello.federate {
        machine::load().unwrap_or_default().into_iter().filter(|m| m.ssh.is_some()).collect()
    } else {
        Vec::new()
    };
    let (status_tx, mut status_rx) = mpsc::unbounded_channel::<MachineInfo>();
    let mut machines: Vec<MachineInfo> = vec![local_info()];
    let mut remotes: HashMap<String, Conn> = HashMap::new();
    for e in entries {
        machines.push(MachineInfo { id: e.id.clone(), label: e.label.clone(), online: false, error: None });
        let (tx, rx) = mpsc::unbounded_channel();
        let id = e.id.clone();
        let task = tokio::spawn(remote::run(e, rx, out.clone(), status_tx.clone()));
        remotes.insert(id, Conn { tx, task });
    }
    if hello.federate {
        let _ = out.send(Message::MachineList(machines.clone()));
    }

    let client_id = {
        let mut d = desktop().lock().unwrap();
        d.next_client += 1;
        let id = d.next_client;
        d.clients.insert(id, out.clone());
        let _ = out.send(Message::Focus(d.focus.clone()));
        let _ = out.send(d.command_list());
        id
    };

    let mut local = LocalRelay::new(out.clone());
    let mut msgs = spawn_reader(rd);
    let mut tick = tokio::time::interval(Duration::from_secs(1));

    let result = loop {
        tokio::select! {
            _ = tick.tick() => local.refresh(),
            info = status_rx.recv() => {
                let Some(info) = info else { continue };
                if let Some(m) = machines.iter_mut().find(|m| m.id == info.id) {
                    if *m != info {
                        *m = info;
                        let _ = out.send(Message::MachineList(machines.clone()));
                    }
                }
            }
            msg = msgs.recv() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    None => break Ok(()),
                    Some(Err(e)) => break Err(e),
                };
                let Some(msg) = handle_desktop(client_id, msg) else { continue };
                match target_machine(&msg) {
                    None | Some(LOCAL_MACHINE) => {
                        if matches!(msg, Message::ListSessions) {
                            for r in remotes.values() {
                                let _ = r.tx.send(Message::ListSessions);
                            }
                        }
                        local.handle(msg);
                    }
                    Some(id) => match remotes.get(id) {
                        Some(r) => {
                            let _ = r.tx.send(msg);
                        }
                        None => {
                            let error = format!("unknown machine '{id}'");
                            let reply = match msg {
                                Message::Launch(l) => Message::RequestFailed(RequestFailed { id: l.id, error }),
                                Message::ListDir(l) => Message::RequestFailed(RequestFailed { id: l.id, error }),
                                _ => continue,
                            };
                            let _ = out.send(reply);
                        }
                    },
                }
            }
        }
    };
    {
        let mut d = desktop().lock().unwrap();
        d.clients.remove(&client_id);
        let before = d.commands.len();
        d.commands.retain(|_, (owner, _)| *owner != client_id);
        if d.commands.len() != before {
            let list = d.command_list();
            d.broadcast(&list);
        }
    }
    drop(local);
    drop(remotes);
    writer.abort();
    result
}
