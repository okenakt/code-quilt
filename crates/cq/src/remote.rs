//! Connections from the local daemon to registered remote machines (`ssh <host> cq bridge`).
//!
//! The remote daemon speaks the same protocol and tags its sessions with its own machine id, so
//! its messages pass through unchanged and clients of the local daemon see every machine through
//! one connection.

use crate::machine::{MachineEntry, Os};
use anyhow::{Context, Result, bail};
use cq_protocol::local::BoxStream;
use cq_protocol::{ClientKind, DAEMON_PROTOCOL_VERSION, Hello, MachineInfo, Message, Subscribe, spawn_reader, write_message};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

/// Delay before reconnecting to an unreachable machine.
const RETRY: Duration = Duration::from_secs(5);

/// Prefix of errors the remote daemon reported itself, which explain a failure better than
/// whatever ssh prints once it is killed.
const REMOTE_ERROR: &str = "remote daemon: ";

/// `ssh` to the machine. Authentication is left to ssh (keys, agent, `~/.ssh/config`), and
/// `BatchMode` keeps it from ever prompting: the daemon has no terminal to ask on.
fn ssh(e: &MachineEntry) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=15"]);
    if let Some(p) = e.port {
        cmd.args(["-p", &p.to_string()]);
    }
    cmd.arg(&e.ssh)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // The daemon has no console; keep ssh from opening one.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Learns the OS of the machine. The command parses the same in sh, cmd and PowerShell: `uname`
/// fails on Windows, where `ver` answers instead.
pub async fn probe(e: &MachineEntry) -> Result<Os> {
    let out = ssh(e).arg("uname -s || ver").stdin(std::process::Stdio::null()).output().await.context("spawn ssh")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Some(system) = stdout.lines().map(str::trim).find(|l| !l.is_empty()) else {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("ssh {}: {}", e.ssh, err.trim().lines().last().unwrap_or("connection failed"));
    };
    Ok(if system.contains("Windows") { Os::Windows } else { Os::Unix })
}

/// `ssh <host> cq bridge` with its stdio as the stream to the remote daemon.
struct Bridge {
    child: tokio::process::Child,
    stream: Option<BoxStream>,
    /// The last line ssh (or the remote command) wrote to stderr.
    stderr: JoinHandle<Option<String>>,
}

impl Bridge {
    fn open(e: &MachineEntry) -> Result<Self> {
        let cq = e.cq();
        let mut cmd = ssh(e);
        // ssh joins the remote command into one string for the remote shell.
        match e.os {
            Os::Unix => cmd.arg(format!("sh -lc 'exec \"{cq}\" bridge'")),
            Os::Windows => cmd.arg(format!("\"{cq}\" bridge")),
        };
        let mut child = cmd.spawn().context("spawn ssh")?;
        let stdout = child.stdout.take().context("ssh stdout")?;
        let stdin = child.stdin.take().context("ssh stdin")?;
        let mut lines = tokio::io::BufReader::new(child.stderr.take().context("ssh stderr")?).lines();
        let stderr = tokio::spawn(async move {
            let mut last = None;
            while let Ok(Some(l)) = lines.next_line().await {
                if !l.trim().is_empty() {
                    last = Some(l.trim().to_owned());
                }
            }
            last
        });
        Ok(Self { child, stream: Some(Box::new(tokio::io::join(stdout, stdin))), stderr })
    }

    /// Ends the connection and says why it failed: the remote daemon's own error, else what ssh
    /// reported, else `err`.
    async fn close(mut self, err: anyhow::Error) -> String {
        let _ = self.child.start_kill();
        let err = format!("{err:#}");
        if err.starts_with(REMOTE_ERROR) {
            return err;
        }
        match tokio::time::timeout(Duration::from_secs(2), self.stderr).await {
            Ok(Ok(Some(line))) => line,
            _ => err,
        }
    }
}

/// Connects through `cq bridge` as the daemon does and returns how the remote daemon introduces
/// itself, which also proves it speaks this protocol version.
pub async fn check(e: &MachineEntry) -> Result<MachineInfo> {
    let mut b = Bridge::open(e)?;
    let (rd, mut wr) = tokio::io::split(b.stream.take().expect("fresh"));
    let mut msgs = spawn_reader(rd);
    let answered = async {
        write_message(&mut wr, &Message::Hello(hello())).await?;
        match tokio::time::timeout(Duration::from_secs(20), msgs.recv()).await {
            Ok(Some(Ok(msg))) => introduction(msg),
            Ok(Some(Err(err))) => Err(err),
            Ok(None) => bail!("`{} bridge` exited", e.cq()),
            Err(_) => bail!("no answer from `{} bridge`", e.cq()),
        }
    };
    match answered.await {
        Ok(me) => Ok(me),
        Err(err) => bail!(b.close(err).await),
    }
}

/// The machine a daemon's first message introduces.
fn introduction(msg: Message) -> Result<MachineInfo> {
    match msg {
        Message::MachineList(mut l) if !l.is_empty() => Ok(l.swap_remove(0)),
        Message::Error(err) => bail!("{REMOTE_ERROR}{err}"),
        _ => bail!("{REMOTE_ERROR}it did not introduce itself"),
    }
}

fn hello() -> Hello {
    Hello { protocol_version: DAEMON_PROTOCOL_VERSION, kind: ClientKind::Hub, cols: 0, rows: 0, federate: false }
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
        let _ = status.send(MachineInfo { id: entry.id.clone(), name: entry.name.clone(), online, error });
    };
    loop {
        let err = match Bridge::open(&entry) {
            Ok(mut b) => {
                let stream = b.stream.take().expect("fresh");
                let err = relay(&entry.id, stream, &mut subs, &mut rx, &out, &report).await.err();
                b.close(err.unwrap_or_else(|| anyhow::anyhow!("disconnected"))).await
            }
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
                    Some(m) => {
                        if let Some(id) = m.request_id() {
                            fail(&out, id, "machine is offline");
                        }
                    }
                    None => return,
                },
            }
        }
    }
}

fn fail(out: &UnboundedSender<Message>, id: u64, error: &str) {
    let _ = out.send(Message::RequestFailed(cq_protocol::RequestFailed { id, error: error.into() }));
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
    write_message(&mut wr, &Message::Hello(hello())).await?;
    // The machine at this address may have changed (reinstalled, or another host behind it).
    let me = match msgs.recv().await {
        Some(msg) => introduction(msg?)?,
        None => return Ok(()),
    };
    if me.id != id {
        bail!("this address now reaches another machine ('{}'); register it with `cq machine add`", me.name);
    }
    report(true, None);
    write_message(&mut wr, &Message::ListSessions).await?;
    for s in subs.values() {
        write_message(&mut wr, &Message::Subscribe(s.clone())).await?;
    }
    loop {
        tokio::select! {
            msg = msgs.recv() => {
                let Some(msg) = msg else { return Ok(()) };
                let msg = msg?;
                if let Message::Error(e) = &msg {
                    bail!("{REMOTE_ERROR}{e}");
                }
                // Focus and commands belong to the desktop the client sits at, not to the
                // remote machine; relaying them would overwrite the local daemon's.
                if matches!(msg, Message::MachineList(_) | Message::Focus(_) | Message::CommandList(_) | Message::InvokeCommand(_) | Message::CommandResult(_)) {
                    continue;
                }
                let _ = out.send(msg);
            }
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { return Ok(()) };
                match &cmd {
                    Message::Subscribe(s) => { subs.insert(s.session.clone(), s.clone()); }
                    Message::Unsubscribe(r) => { subs.remove(&r.session); }
                    _ => {}
                }
                write_message(&mut wr, &cmd).await?;
            }
        }
    }
}
