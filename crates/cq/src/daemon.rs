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
use cq_protocol::{
    ClientKind, CommandInfo, CommandResult, DirList, Hello, InvokeCommand, DAEMON_PROTOCOL_VERSION, Launched, MachineInfo, Message, PROTOCOL_VERSION,
    RequestFailed, SessionList, SessionMeta, SessionStatus, Size, Subscribe, read_message, spawn_reader,
    write_message,
};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Runs the daemon. Returns immediately if another daemon already owns the endpoint.
pub async fn run() -> Result<()> {
    paths::ensure_dirs()?;
    let endpoint = paths::daemon_endpoint()?;
    // Binding a unix socket replaces the file, so check for a live daemon first.
    if ipc::connect_now(&endpoint).await.is_ok() {
        return Ok(());
    }
    // Load plugin commands before listening, so a client's first invocation finds them and no
    // client waits on an endpoint that is not accepting yet.
    let plugins = tokio::task::spawn_blocking(|| {
        plugin::sync_bundled();
        plugin::installed()
    })
    .await?;
    desktop().lock().unwrap().plugins = plugins;
    let mut listener = ipc::Listener::bind(&endpoint)?;
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

/// A task relaying one connection, fed through `tx`; dropping it ends the task.
pub struct Conn {
    pub tx: UnboundedSender<Message>,
    pub task: JoinHandle<()>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn routed(machine: &str, session: &str, inner: Message) -> Message {
    Message::Routed { machine: machine.to_owned(), session: session.to_owned(), inner: Box::new(inner) }
}

/// Opens a host connection of the given kind and relays it as `Routed` messages.
fn open_host(machine: &str, meta: &SessionMeta, kind: ClientKind, cols: u16, rows: u16, out: UnboundedSender<Message>) -> Conn {
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let machine = machine.to_owned();
    let session = meta.id.clone();
    let pipe = meta.pipe.clone();
    let task = tokio::spawn(async move {
        let run = async {
            let stream = ipc::connect(&pipe, Duration::from_secs(1)).await?;
            let (rd, mut wr) = tokio::io::split(stream);
            let mut msgs = spawn_reader(rd);
            let hello = Hello { cols, rows, ..Hello::new(PROTOCOL_VERSION, kind) };
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
                            let _ = out.send(routed(&machine, &session, msg));
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
            let _ = out.send(routed(&machine, &session, Message::Error(format!("{e:#}"))));
        }
    });
    Conn { tx, task }
}

/// One client's relay to the sessions of this machine, tagged with this machine's id.
struct LocalRelay {
    machine: String,
    out: UnboundedSender<Message>,
    monitors: HashMap<String, Conn>,
    /// Hosts whose monitor connection ended recently (e.g. incompatible version); retried later.
    backoff: HashMap<String, tokio::time::Instant>,
    subs: HashMap<String, Conn>,
    sessions: Vec<SessionMeta>,
    last_key: String,
}

impl LocalRelay {
    fn new(machine: String, out: UnboundedSender<Message>) -> Self {
        Self {
            machine,
            out,
            monitors: HashMap::new(),
            backoff: HashMap::new(),
            subs: HashMap::new(),
            sessions: Vec::new(),
            last_key: String::new(),
        }
    }

    fn send_list(&self) {
        let list = SessionList { machine: self.machine.clone(), sessions: self.sessions.clone() };
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
                self.monitors.insert(m.id.clone(), open_host(&self.machine, m, ClientKind::Monitor, 0, 0, self.out.clone()));
            }
        }
        self.subs.retain(|_, c| !c.task.is_finished());
    }

    /// Connection that should carry a session message. Control messages go through the monitor
    /// so they work without a subscription; so does input for sessions without an open tile.
    fn route(&self, session: &str, inner: &Message) -> Option<&Conn> {
        match inner {
            Message::Approve(_) | Message::Rename(_) | Message::Inject(_) | Message::Terminate(_) | Message::ShareFile(_) => {
                self.monitors.get(session)
            }
            Message::Input(_) => self.subs.get(session).or_else(|| self.monitors.get(session)),
            _ => self.subs.get(session),
        }
    }

    /// Starts a session in the background and answers the request `id` with its outcome.
    fn start_session(&self, id: u64, what: &'static str, start: impl FnOnce() -> Result<String> + Send + 'static) {
        let (machine, out) = (self.machine.clone(), self.out.clone());
        tokio::spawn(async move {
            let reply = match start_registered(start).await {
                Ok(session) => Message::Launched(Launched { id, machine, session }),
                Err(e) => Message::RequestFailed(RequestFailed { id, error: format!("{what} failed: {e:#}") }),
            };
            let _ = out.send(reply);
        });
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
                    Some(meta) if meta.status != SessionStatus::Running => {
                        let _ = self.out.send(routed(&self.machine, &session, Message::Error("session exited".into())));
                    }
                    Some(meta) => {
                        let kind = if kind == ClientKind::Event { ClientKind::Hub } else { kind };
                        self.subs.insert(session.clone(), open_host(&self.machine, meta, kind, cols, rows, self.out.clone()));
                    }
                    None => {
                        let _ = self.out.send(routed(&self.machine, &session, Message::Error("unknown session".into())));
                    }
                }
            }
            Message::Unsubscribe(r) => {
                self.subs.remove(&r.session);
            }
            Message::Launch(req) => self.start_session(req.id, "launch", move || crate::launch::launch_detached(&req)),
            Message::Resume(req) => {
                let size = Size { cols: req.cols.max(20), rows: req.rows.max(5) };
                self.start_session(req.id, "resume", move || crate::launch::resume_detached(&req.session, size))
            }
            Message::ListDir(req) => {
                let reply = match crate::config::list_dir(&req.path) {
                    Ok(l) => Message::DirList(DirList { id: req.id, ..l }),
                    Err(e) => Message::RequestFailed(RequestFailed { id: req.id, error: format!("{e:#}") }),
                };
                let _ = self.out.send(reply);
            }
            Message::ReadFile(req) => {
                let reply = match crate::session::read_shared(&req.session, &req.path) {
                    Ok(data) => Message::FileContent { id: req.id, data },
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
        Message::Resume(r) => Some(&r.machine),
        Message::ListDir(l) => Some(&l.machine),
        Message::ReadFile(r) => Some(&r.machine),
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
    focus: Option<cq_protocol::SessionRef>,
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

/// A connection from a daemon that reached this one over ssh (an upstream): its client id and
/// where to send it messages.
type Link = (u64, UnboundedSender<Message>);

/// Upstreams connected to this daemon (newest last), and where the answers they give for each of
/// this daemon's federated clients go.
#[derive(Default)]
struct Links {
    upstreams: Vec<Link>,
    inboxes: HashMap<u64, UnboundedSender<Message>>,
}

fn links() -> &'static std::sync::Mutex<Links> {
    static LINKS: std::sync::OnceLock<std::sync::Mutex<Links>> = std::sync::OnceLock::new();
    LINKS.get_or_init(Default::default)
}

/// The upstream federated clients use: the newest one.
fn current_upstream() -> &'static watch::Sender<Option<Link>> {
    static CURRENT: std::sync::OnceLock<watch::Sender<Option<Link>>> = std::sync::OnceLock::new();
    CURRENT.get_or_init(|| watch::channel(None).0)
}

fn set_upstreams(f: impl FnOnce(&mut Vec<Link>)) {
    let mut l = links().lock().unwrap();
    f(&mut l.upstreams);
    current_upstream().send_replace(l.upstreams.last().cloned());
}

fn machine_list(own: &[MachineInfo], upstream: &[MachineInfo]) -> Message {
    Message::MachineList(own.iter().chain(upstream).cloned().collect())
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
        machine::load().into_iter().filter(|e| !hello.exclude.contains(&e.id)).collect()
    } else {
        Vec::new()
    };
    let (status_tx, mut status_rx) = mpsc::unbounded_channel::<MachineInfo>();
    let me = machine::local()?;
    let mut machines: Vec<MachineInfo> = vec![me.clone()];
    let mut remotes: HashMap<String, Conn> = HashMap::new();
    for e in entries {
        machines.push(MachineInfo { id: e.id.clone(), name: e.name.clone(), online: false, error: None });
        let (tx, rx) = mpsc::unbounded_channel();
        let id = e.id.clone();
        let task = tokio::spawn(remote::run(e, rx, out.clone(), status_tx.clone()));
        remotes.insert(id, Conn { tx, task });
    }
    // Every connection learns first which machine it reached.
    let _ = out.send(Message::MachineList(machines.clone()));

    let client_id = {
        let mut d = desktop().lock().unwrap();
        d.next_client += 1;
        let id = d.next_client;
        d.clients.insert(id, out.clone());
        let _ = out.send(Message::Focus(d.focus.clone()));
        let _ = out.send(d.command_list());
        id
    };

    // A federated client also sees, through the upstream, the machines this one did not register.
    if hello.relay {
        set_upstreams(|u| u.push((client_id, out.clone())));
    }
    let (inbox_tx, mut inbox_rx) = mpsc::unbounded_channel::<Message>();
    let mut upstream_rx = current_upstream().subscribe();
    let mut upstream: Option<Link> = None;
    let mut upstream_machines: Vec<MachineInfo> = Vec::new();
    let mut upstream_subs: HashMap<String, Subscribe> = HashMap::new();
    if hello.federate {
        links().lock().unwrap().inboxes.insert(client_id, inbox_tx);
        upstream_rx.mark_changed();
    }

    let mut local = LocalRelay::new(me.id.clone(), out.clone());
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
                        let _ = out.send(machine_list(&machines, &upstream_machines));
                    }
                }
            }
            Ok(()) = upstream_rx.changed(), if hello.federate => {
                let next = upstream_rx.borrow_and_update().clone();
                if next.as_ref().map(|l| l.0) == upstream.as_ref().map(|l| l.0) {
                    continue;
                }
                if let Some((_, tx)) = &upstream {
                    let _ = tx.send(Message::UpstreamClose(client_id));
                }
                upstream = next;
                match &upstream {
                    Some((_, tx)) => {
                        let exclude = machines.iter().map(|m| m.id.clone()).collect();
                        let hello = Hello { federate: true, exclude, ..Hello::new(DAEMON_PROTOCOL_VERSION, ClientKind::Hub) };
                        let resubscribe = upstream_subs.values().cloned().map(Message::Subscribe);
                        for m in [Message::Hello(hello), Message::ListSessions].into_iter().chain(resubscribe) {
                            let _ = tx.send(Message::Upstream { conn: client_id, inner: Box::new(m) });
                        }
                    }
                    None => {
                        for m in &mut upstream_machines {
                            m.online = false;
                            m.error = Some("unreachable while no upstream is connected to this machine".into());
                        }
                        let _ = out.send(machine_list(&machines, &upstream_machines));
                    }
                }
            }
            Some(msg) = inbox_rx.recv(), if hello.federate => match msg {
                Message::MachineList(l) => {
                    upstream_machines = l.into_iter().filter(|m| machines.iter().all(|x| x.id != m.id)).collect();
                    let _ = out.send(machine_list(&machines, &upstream_machines));
                }
                msg => {
                    let _ = out.send(msg);
                }
            },
            msg = msgs.recv() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    None => break Ok(()),
                    Some(Err(e)) => break Err(e),
                };
                // An upstream hands back what it answered for one of this daemon's clients.
                if let Message::Upstream { conn, inner } = msg {
                    if let (true, Some(tx)) = (hello.relay, links().lock().unwrap().inboxes.get(&conn)) {
                        let _ = tx.send(*inner);
                    }
                    continue;
                }
                let Some(msg) = handle_desktop(client_id, msg) else { continue };
                match target_machine(&msg).filter(|id| *id != me.id) {
                    None => {
                        if matches!(msg, Message::Shutdown) {
                            ipc::cleanup(&paths::daemon_endpoint()?);
                            std::process::exit(0);
                        }
                        if matches!(msg, Message::ListSessions) {
                            for r in remotes.values() {
                                let _ = r.tx.send(Message::ListSessions);
                            }
                            if let Some((_, tx)) = &upstream {
                                let _ = tx.send(Message::Upstream { conn: client_id, inner: Box::new(Message::ListSessions) });
                            }
                        }
                        local.handle(msg);
                    }
                    Some(id) => match (remotes.get(id), &upstream) {
                        (Some(r), _) => {
                            let _ = r.tx.send(msg);
                        }
                        (None, Some((_, tx))) if upstream_machines.iter().any(|m| m.id == id) => {
                            match &msg {
                                Message::Subscribe(s) => {
                                    upstream_subs.insert(s.session.clone(), s.clone());
                                }
                                Message::Unsubscribe(r) => {
                                    upstream_subs.remove(&r.session);
                                }
                                _ => {}
                            }
                            let _ = tx.send(Message::Upstream { conn: client_id, inner: Box::new(msg) });
                        }
                        _ => {
                            if let Some(rid) = msg.request_id() {
                                let error = format!("unknown machine '{id}'");
                                let _ = out.send(Message::RequestFailed(RequestFailed { id: rid, error }));
                            }
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
    if hello.relay {
        set_upstreams(|u| u.retain(|(id, _)| *id != client_id));
    }
    links().lock().unwrap().inboxes.remove(&client_id);
    if let Some((_, tx)) = &upstream {
        let _ = tx.send(Message::UpstreamClose(client_id));
    }
    drop(local);
    drop(remotes);
    writer.abort();
    result
}

/// Starts a session (new or resumed) and returns once its host has recorded it as running, so
/// the session is in every list by the time the caller hears about it.
async fn start_registered(start: impl FnOnce() -> Result<String>) -> Result<String> {
    let id = start()?;
    let file = paths::session_file(&id)?;
    for _ in 0..100 {
        let meta = std::fs::read(&file).ok().and_then(|d| serde_json::from_slice::<SessionMeta>(&d).ok());
        if meta.is_some_and(|m| m.status == SessionStatus::Running) {
            return Ok(id);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("the session host did not start; see ~/.codequilt/logs/{id}.log")
}
