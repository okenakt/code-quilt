//! Machine daemon (`cq daemon`) and its stdio bridge (`cq bridge`).
//!
//! The daemon discovers session hosts from `~/.codequilt/sessions` and relays them to clients over
//! a single connection. For clients that ask for it (`federate`), it also connects to every
//! registered remote machine and relays those too, so clients only ever talk to the local daemon.
//! It keeps no state of its own: event history lives in each host, so a restarted daemon (or
//! client) simply reconnects and receives the replay.

use crate::machine::{self, MachineEntry};
use crate::{ipc, paths, plugin, remote, session};
use anyhow::{Result, bail};
use cq_proto::{
    ClientKind, CommandInfo, CommandResult, DirList, Hello, InvokeCommand, DAEMON_PROTOCOL_VERSION, LOCAL_MACHINE, Launched, MachineInfo, Message, PROTOCOL_VERSION,
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
    tokio::spawn(maintain_desktop());
    loop {
        let stream = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = serve(stream).await {
                eprintln!("daemon client error: {e:#}");
            }
        });
    }
}

/// `cq bridge`: relays stdin/stdout to the local daemon (used as `ssh host cq bridge`).
pub async fn bridge() -> Result<()> {
    let stream = cq_client::connect_daemon().await?;
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
            Message::Approve(_) | Message::SetMeta(_) | Message::Inject(_) | Message::Terminate(_) => {
                self.monitors.get(session)
            }
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

/// How long an `activate` command waits for its plugin to register a handler.
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an `exec` command may run before the caller is told it timed out.
const EXEC_TIMEOUT: Duration = Duration::from_secs(120);

/// An invocation waiting for its plugin to start and register.
struct Pending {
    client: u64,
    inv: InvokeCommand,
    since: std::time::Instant,
}

/// State shared by all clients of this daemon: the desktop's focus, the commands registered by
/// running clients, and the commands declared by installed plugins.
#[derive(Default)]
struct Desktop {
    next_client: u64,
    clients: HashMap<u64, UnboundedSender<Message>>,
    focus: Option<cq_proto::SessionRef>,
    commands: HashMap<String, (u64, CommandInfo)>,
    plugins: Vec<(String, plugin::Installed)>,
    pending: Vec<Pending>,
}

impl Desktop {
    fn broadcast(&self, msg: &Message) {
        for tx in self.clients.values() {
            let _ = tx.send(msg.clone());
        }
    }

    /// Manifest commands first (they carry titles and arguments), then commands that exist only
    /// because a running client registered them.
    fn command_list(&self) -> Message {
        let mut list: Vec<CommandInfo> = Vec::new();
        for (name, p) in &self.plugins {
            list.extend(p.manifest.commands.iter().map(|c| c.info(name)));
        }
        for (_, c) in self.commands.values() {
            if !list.iter().any(|l| l.name == c.name) {
                list.push(c.clone());
            }
        }
        list.sort_by(|a, b| a.name.cmp(&b.name));
        Message::CommandList(list)
    }

    fn manifest_command(&self, name: &str) -> Option<(&plugin::Installed, &plugin::ManifestCommand)> {
        self.plugins.iter().find_map(|(_, p)| p.manifest.commands.iter().find(|c| c.name == name).map(|c| (p, c)))
    }

    fn reply(&self, client: u64, id: Option<u64>, ok: bool, message: impl Into<String>) {
        if let (Some(id), Some(tx)) = (id, self.clients.get(&client)) {
            let _ = tx.send(Message::CommandResult(CommandResult { id, ok, message: message.into() }));
        }
    }

    /// Hands an invocation to the client that registered the command.
    fn deliver(&self, client: u64, owner: u64, inv: InvokeCommand) {
        let id = inv.id;
        match self.clients.get(&owner) {
            Some(tx) => {
                let _ = tx.send(Message::InvokeCommand(inv));
                self.reply(client, id, true, "delivered");
            }
            None => self.reply(client, id, false, "the handler is gone"),
        }
    }

    fn invoke(&mut self, client: u64, mut inv: InvokeCommand) {
        if inv.context.is_none() {
            inv.context = self.focus.clone();
        }
        if let Some((owner, _)) = self.commands.get(&inv.name) {
            let owner = *owner;
            return self.deliver(client, owner, inv);
        }
        let Some((p, cmd)) = self.manifest_command(&inv.name) else {
            return self.reply(client, inv.id, false, format!("unknown command '{}'", inv.name));
        };
        let (path, run, declared) = (p.path.clone(), cmd.run.clone(), cmd.args.clone());
        let template = match &run {
            plugin::Run::Exec(t) | plugin::Run::Activate(t) => t,
        };
        let args = match plugin::expand_args(template, &declared, &inv.args, inv.context.as_ref()) {
            Ok(a) => a,
            Err(e) => return self.reply(client, inv.id, false, format!("{e:#}")),
        };
        match run {
            plugin::Run::Exec(_) => {
                let Some(tx) = self.clients.get(&client).cloned() else { return };
                tokio::spawn(exec_command(path, args, inv.id, tx));
            }
            plugin::Run::Activate(_) => {
                let starting = self.pending.iter().any(|p| p.inv.name == inv.name);
                if !starting && let Err(e) = start_plugin(&path, &args) {
                    return self.reply(client, inv.id, false, format!("{e:#}"));
                }
                self.pending.push(Pending { client, inv, since: std::time::Instant::now() });
            }
        }
    }

    /// Delivers invocations that were waiting for `name` to be registered by `owner`.
    fn flush_pending(&mut self, name: &str, owner: u64) {
        let (ready, waiting): (Vec<_>, Vec<_>) = self.pending.drain(..).partition(|p| p.inv.name == name);
        self.pending = waiting;
        for p in ready {
            self.deliver(p.client, owner, p.inv);
        }
    }

    /// Gives up on activations whose plugin did not register in time.
    fn expire_pending(&mut self) {
        let (expired, waiting): (Vec<_>, Vec<_>) =
            self.pending.drain(..).partition(|p| p.since.elapsed() > ACTIVATE_TIMEOUT);
        self.pending = waiting;
        for p in expired {
            self.reply(p.client, p.inv.id, false, format!("{} did not start in time", p.inv.name));
        }
    }

}

/// Runs an `exec` command and reports its outcome to the caller.
async fn exec_command(path: std::path::PathBuf, args: Vec<String>, id: Option<u64>, tx: UnboundedSender<Message>) {
    let (program, prefix) = crate::host::with_batch_wrapper(&path);
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(prefix).args(&args).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        // The daemon has no console; without this every command would open a console window.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let outcome = match tokio::time::timeout(EXEC_TIMEOUT, cmd.output()).await {
        Err(_) => (false, format!("still running after {}s", EXEC_TIMEOUT.as_secs())),
        Ok(Err(e)) => (false, format!("run {}: {e}", path.display())),
        Ok(Ok(out)) => {
            let text = if out.status.success() { &out.stdout } else { &out.stderr };
            let text = String::from_utf8_lossy(text).trim().to_owned();
            let tail: String = text.chars().rev().take(500).collect::<Vec<_>>().into_iter().rev().collect();
            (out.status.success(), tail)
        }
    };
    if let Some(id) = id {
        let _ = tx.send(Message::CommandResult(CommandResult { id, ok: outcome.0, message: outcome.1 }));
    }
}

/// Starts a plugin process for an `activate` command, detached like a session host.
fn start_plugin(path: &std::path::Path, args: &[String]) -> Result<()> {
    let (program, prefix) = crate::host::with_batch_wrapper(path);
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    cq_client::spawn_detached(std::process::Command::new(program).args(prefix).args(args), &cq_client::log_file(&name)?)
}

fn desktop() -> &'static std::sync::Mutex<Desktop> {
    static DESKTOP: std::sync::OnceLock<std::sync::Mutex<Desktop>> = std::sync::OnceLock::new();
    DESKTOP.get_or_init(Default::default)
}

/// Keeps the plugin commands current and expires activations that never registered.
async fn maintain_desktop() {
    let _ = tokio::task::spawn_blocking(plugin::sync_bundled).await;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut n = 0u32;
    loop {
        tick.tick().await;
        let plugins = if n.is_multiple_of(3) { Some(tokio::task::spawn_blocking(plugin::installed).await.unwrap_or_default()) } else { None };
        n = n.wrapping_add(1);
        let mut d = desktop().lock().unwrap();
        if let Some(plugins) = plugins {
            let sig = |ps: &[(String, plugin::Installed)]| serde_json::to_string(ps).unwrap_or_default();
            if sig(&plugins) != sig(&d.plugins) {
                d.plugins = plugins;
                let list = d.command_list();
                d.broadcast(&list);
            }
        }
        d.expire_pending();
    }
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
            let name = c.name.clone();
            d.commands.insert(name.clone(), (client, c));
            let list = d.command_list();
            d.broadcast(&list);
            d.flush_pending(&name, client);
        }
        Message::InvokeCommand(inv) => d.invoke(client, inv),
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
        machine::load().into_iter().filter(|m| m.ssh.is_some()).collect()
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
