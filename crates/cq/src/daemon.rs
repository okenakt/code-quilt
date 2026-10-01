//! Machine daemon (`cq daemon`) and its stdio bridge (`cq bridge`).
//!
//! The daemon discovers session hosts from `~/.codequilt/sessions` and relays them to clients over
//! a single connection. It keeps no state of its own: event history lives in each host, so a
//! restarted daemon (or client) simply reconnects and receives the replay.

use crate::{ipc, paths, session};
use anyhow::{Context, Result, bail};
use cq_proto::{
    ClientKind, Hello, Message, PROTOCOL_VERSION, SessionMeta, SessionStatus, Subscribe, spawn_reader,
    read_message, write_message,
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

/// Opens a host connection of the given kind and relays it as `Routed` messages.
fn open_host(
    meta: &SessionMeta,
    kind: ClientKind,
    cols: u16,
    rows: u16,
    out: UnboundedSender<Message>,
) -> Conn {
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let session = meta.id.clone();
    let pipe = meta.pipe.clone();
    let task = tokio::spawn(async move {
        let run = async {
            let stream = ipc::connect(&pipe, Duration::from_secs(1)).await?;
            let (rd, mut wr) = tokio::io::split(stream);
            let mut msgs = spawn_reader(rd);
            let hello = Hello { protocol_version: PROTOCOL_VERSION, kind, cols, rows };
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
                            let _ = out.send(Message::Routed { session: session.clone(), inner: Box::new(msg) });
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
            let _ = out.send(Message::Routed {
                session: session.clone(),
                inner: Box::new(Message::Error(format!("{e:#}"))),
            });
        }
    });
    Conn { tx, task }
}

fn list_key(list: &[SessionMeta]) -> String {
    serde_json::to_string(list).unwrap_or_default()
}

async fn serve(stream: ipc::BoxStream) -> Result<()> {
    let (mut rd, mut wr) = tokio::io::split(stream);
    let hello = match tokio::time::timeout(Duration::from_secs(5), read_message(&mut rd)).await?? {
        Some(Message::Hello(h)) => h,
        _ => bail!("expected hello"),
    };
    if hello.protocol_version != PROTOCOL_VERSION {
        let msg = format!(
            "protocol mismatch: daemon {PROTOCOL_VERSION}, client {}",
            hello.protocol_version
        );
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

    let mut msgs = spawn_reader(rd);
    let mut monitors: HashMap<String, Conn> = HashMap::new();
    // Hosts whose monitor connection ended recently (e.g. incompatible version); retried later.
    let mut backoff: HashMap<String, tokio::time::Instant> = HashMap::new();
    let mut subs: HashMap<String, Conn> = HashMap::new();
    let mut sessions: Vec<SessionMeta> = Vec::new();
    let mut last_key = String::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));

    let result = loop {
        tokio::select! {
            _ = tick.tick() => {
                sessions = session::list().unwrap_or_default();
                let key = list_key(&sessions);
                if key != last_key {
                    last_key = key;
                    let _ = out.send(Message::SessionList(sessions.clone()));
                }
                let now = tokio::time::Instant::now();
                monitors.retain(|id, c| {
                    let keep = !c.task.is_finished() && sessions.iter().any(|m| &m.id == id);
                    if !keep {
                        backoff.insert(id.clone(), now + Duration::from_secs(30));
                    }
                    keep
                });
                backoff.retain(|_, until| *until > now);
                for m in sessions.iter().filter(|m| m.status == SessionStatus::Running) {
                    if !monitors.contains_key(&m.id) && !backoff.contains_key(&m.id) {
                        monitors.insert(m.id.clone(), open_host(m, ClientKind::Monitor, 0, 0, out.clone()));
                    }
                }
                subs.retain(|_, c| !c.task.is_finished());
            }
            msg = msgs.recv() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    None => break Ok(()),
                    Some(Err(e)) => break Err(e),
                };
                match msg {
                    Message::ListSessions => {
                        sessions = session::list().unwrap_or_default();
                        let _ = out.send(Message::SessionList(sessions.clone()));
                    }
                    Message::Subscribe(Subscribe { session, kind, cols, rows }) => {
                        if sessions.iter().all(|m| m.id != session) {
                            sessions = session::list().unwrap_or_default();
                        }
                        match sessions.iter().find(|m| m.id == session) {
                            Some(meta) => {
                                let kind = if kind == ClientKind::Event { ClientKind::Hub } else { kind };
                                subs.insert(session.clone(), open_host(meta, kind, cols, rows, out.clone()));
                            }
                            None => {
                                let _ = out.send(Message::Routed {
                                    session,
                                    inner: Box::new(Message::Error("unknown session".into())),
                                });
                            }
                        }
                    }
                    Message::Unsubscribe(session) => {
                        subs.remove(&session);
                    }
                    Message::Launch(req) => {
                        let reply = match crate::launch::launch_detached(&req) {
                            Ok(id) => Message::Launched(id),
                            Err(e) => Message::Error(format!("launch failed: {e:#}")),
                        };
                        let _ = out.send(reply);
                    }
                    Message::ListDir(path) => {
                        let reply = match crate::config::list_dir(&path) {
                            Ok(l) => Message::DirList(l),
                            Err(e) => Message::Error(format!("{e:#}")),
                        };
                        let _ = out.send(reply);
                    }
                    Message::Routed { session, inner } => {
                        // Approvals go through the monitor so they work without a subscription;
                        // so does input for sessions that have no open tile (Queen).
                        let target = match *inner {
                            Message::Approve(_) => monitors.get(&session),
                            Message::Input(_) => subs.get(&session).or_else(|| monitors.get(&session)),
                            _ => subs.get(&session),
                        };
                        if let Some(c) = target {
                            let _ = c.tx.send(*inner);
                        }
                    }
                    _ => {}
                }
            }
        }
    };
    drop(monitors);
    drop(subs);
    writer.abort();
    result
}
