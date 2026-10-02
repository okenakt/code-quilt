//! Connections from the local daemon to registered remote machines (`ssh <host> cq bridge`).
//!
//! The remote daemon speaks the same protocol and reports its own sessions under
//! [`LOCAL_MACHINE`]; this module rewrites them to the machine's id so clients of the local daemon
//! see every machine through one connection.

use crate::machine::MachineEntry;
use anyhow::{Context, Result, bail};
use cq_proto::local::BoxStream;
use cq_proto::{
    ClientKind, DAEMON_PROTOCOL_VERSION, Hello, LOCAL_MACHINE, MachineInfo, Message, SessionList,
    SessionRef, Subscribe, spawn_reader, write_message,
};
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Delay before reconnecting to an unreachable machine.
const RETRY: Duration = Duration::from_secs(5);

/// Child process stdio presented as one stream; the child is killed when dropped.
struct ChildStream {
    io: tokio::io::Join<tokio::process::ChildStdout, tokio::process::ChildStdin>,
    _child: tokio::process::Child,
}

impl AsyncRead for ChildStream {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for ChildStream {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// Runs `ssh <host> cq bridge`. No port is opened on the remote side and the user's ssh key
/// authentication applies; `BatchMode` keeps ssh from ever prompting.
fn open_ssh(host: &str, cq_path: &str) -> Result<BoxStream> {
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args(["-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15", host, cq_path, "bridge"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // The daemon has no console; keep ssh from opening one.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().context("spawn ssh")?;
    let stdout = child.stdout.take().context("ssh stdout")?;
    let stdin = child.stdin.take().context("ssh stdin")?;
    Ok(Box::new(ChildStream { io: tokio::io::join(stdout, stdin), _child: child }))
}

/// Rewrites a message from a remote daemon so it carries the machine's id.
fn tag_incoming(id: &str, msg: Message) -> Message {
    match msg {
        Message::SessionList(l) => Message::SessionList(SessionList { machine: id.to_owned(), sessions: l.sessions }),
        Message::Routed { session, inner, .. } => Message::Routed { machine: id.to_owned(), session, inner },
        Message::Launched(mut l) => {
            l.machine = id.to_owned();
            Message::Launched(l)
        }
        other => other,
    }
}

/// Rewrites a client request for the remote daemon, where the machine is its own [`LOCAL_MACHINE`].
fn untag_outgoing(msg: Message) -> Message {
    match msg {
        Message::Subscribe(mut s) => {
            s.machine = LOCAL_MACHINE.to_owned();
            Message::Subscribe(s)
        }
        Message::Unsubscribe(r) => Message::Unsubscribe(SessionRef { machine: LOCAL_MACHINE.to_owned(), session: r.session }),
        Message::Routed { session, inner, .. } => Message::Routed { machine: LOCAL_MACHINE.to_owned(), session, inner },
        Message::Launch(mut l) => {
            l.machine = LOCAL_MACHINE.to_owned();
            Message::Launch(l)
        }
        Message::ListDir(mut l) => {
            l.machine = LOCAL_MACHINE.to_owned();
            Message::ListDir(l)
        }
        other => other,
    }
}

/// Keeps one client's view of a remote machine: connects, relays, and reconnects with backoff.
/// Subscriptions are tracked so they survive reconnects.
pub async fn run(
    entry: MachineEntry,
    mut rx: UnboundedReceiver<Message>,
    out: UnboundedSender<Message>,
    status: UnboundedSender<MachineInfo>,
) {
    let mut subs: HashMap<String, Subscribe> = HashMap::new();
    let report = |online: bool, error: Option<String>| {
        let _ = status.send(MachineInfo { id: entry.id.clone(), label: entry.label.clone(), online, error });
    };
    loop {
        let host = entry.ssh.clone().unwrap_or_default();
        let cq = entry.cq_path.clone().unwrap_or_else(|| "cq".into());
        let err = match open_ssh(&host, &cq) {
            Ok(stream) => match relay(&entry.id, stream, &mut subs, &mut rx, &out, &report).await {
                Ok(()) => "disconnected".to_owned(),
                Err(e) => format!("{e:#}"),
            },
            Err(e) => format!("{e:#}"),
        };
        report(false, Some(err));
        let retry = tokio::time::sleep(RETRY);
        tokio::pin!(retry);
        loop {
            tokio::select! {
                _ = &mut retry => break,
                m = rx.recv() => match m {
                    // Remember subscription changes made while offline.
                    Some(Message::Subscribe(s)) => { subs.insert(s.session.clone(), s); }
                    Some(Message::Unsubscribe(r)) => { subs.remove(&r.session); }
                    Some(Message::Launch(l)) => fail(&out, l.id, "machine is offline"),
                    Some(Message::ListDir(l)) => fail(&out, l.id, "machine is offline"),
                    Some(_) => {}
                    None => return,
                },
            }
        }
    }
}

fn fail(out: &UnboundedSender<Message>, id: u64, error: &str) {
    let _ = out.send(Message::RequestFailed(cq_proto::RequestFailed { id, error: error.into() }));
}

async fn relay(
    id: &str,
    stream: BoxStream,
    subs: &mut HashMap<String, Subscribe>,
    rx: &mut UnboundedReceiver<Message>,
    out: &UnboundedSender<Message>,
    report: &impl Fn(bool, Option<String>),
) -> Result<()> {
    let (rd, mut wr) = tokio::io::split(stream);
    let mut msgs = spawn_reader(rd);
    let hello = Hello { protocol_version: DAEMON_PROTOCOL_VERSION, kind: ClientKind::Hub, cols: 0, rows: 0, federate: false };
    write_message(&mut wr, &Message::Hello(hello)).await?;
    write_message(&mut wr, &Message::ListSessions).await?;
    for s in subs.values() {
        write_message(&mut wr, &untag_outgoing(Message::Subscribe(s.clone()))).await?;
    }
    let mut online = false;
    loop {
        tokio::select! {
            msg = msgs.recv() => {
                let Some(msg) = msg else { return Ok(()) };
                let msg = msg?;
                if let Message::Error(e) = &msg {
                    bail!("remote daemon: {e}");
                }
                // The first reply proves the bridge works (ssh may fail only after spawning).
                if !online {
                    online = true;
                    report(true, None);
                }
                let _ = out.send(tag_incoming(id, msg));
            }
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { return Ok(()) };
                match &cmd {
                    Message::Subscribe(s) => { subs.insert(s.session.clone(), s.clone()); }
                    Message::Unsubscribe(r) => { subs.remove(&r.session); }
                    _ => {}
                }
                write_message(&mut wr, &untag_outgoing(cmd)).await?;
            }
        }
    }
}
