//! Session host (`cq host`): owns the agent's PTY, keeps a screen grid, and fans output out to
//! every attached client. Runs detached so the session survives its terminal and the hub.

use crate::{ipc, paths, session};
use anyhow::{Context, Result, bail};
use cq_proto::{
    AgentEvent, ClientKind, Exit, Hello, InputOwner, Message, PROTOCOL_VERSION, SessionMeta,
    SessionStatus, Size, Welcome, read_message, write_message,
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// Parameters for starting a host.
pub struct HostArgs {
    pub id: String,
    pub agent: String,
    pub cwd: String,
    pub size: Size,
    pub command: Vec<String>,
}

/// Terminal modes that vt100 does not track but a newly attached terminal must replay.
#[derive(Default)]
struct ExtraModes {
    /// Focus event reporting (DECSET 1004).
    focus_events: bool,
    /// Kitty keyboard protocol flag stack (`CSI > flags u` / `CSI < n u`).
    kitty_stack: Vec<u16>,
    /// Replies to cursor position requests, answered by the host as the terminal of record.
    replies: Vec<u8>,
}

impl ExtraModes {
    fn formatted(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.focus_events {
            out.extend_from_slice(b"\x1b[?1004h");
        }
        for flags in &self.kitty_stack {
            out.extend_from_slice(format!("\x1b[>{flags}u").as_bytes());
        }
        out
    }
}

impl vt100::Callbacks for ExtraModes {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied();
        match (i1, c) {
            (None, 'n') if first == Some(6) => {
                // ConPTY blocks on this request at startup (PSEUDOCONSOLE_INHERIT_CURSOR), and a
                // detached host may have no terminal attached to answer it.
                let (row, col) = screen.cursor_position();
                self.replies.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            (Some(b'?'), 'h') if params.iter().any(|p| p.first() == Some(&1004)) => {
                self.focus_events = true
            }
            (Some(b'?'), 'l') if params.iter().any(|p| p.first() == Some(&1004)) => {
                self.focus_events = false
            }
            (Some(b'>'), 'u') => self.kitty_stack.push(first.unwrap_or(0)),
            (Some(b'<'), 'u') => {
                let n = first.unwrap_or(1).max(1) as usize;
                let keep = self.kitty_stack.len().saturating_sub(n);
                self.kitty_stack.truncate(keep);
            }
            (Some(b'='), 'u') => {
                let flags = first.unwrap_or(0);
                let mode = params.get(1).and_then(|p| p.first()).copied().unwrap_or(1);
                let cur = self.kitty_stack.last().copied().unwrap_or(0);
                let new = match mode {
                    2 => cur | flags,
                    3 => cur & !flags,
                    _ => flags,
                };
                match self.kitty_stack.last_mut() {
                    Some(top) => *top = new,
                    None => self.kitty_stack.push(new),
                }
            }
            _ => {}
        }
    }
}

struct Client {
    tx: mpsc::UnboundedSender<Message>,
    size: Size,
}

struct State {
    parser: vt100::Parser<ExtraModes>,
    master: Box<dyn MasterPty + Send>,
    clients: HashMap<u64, Client>,
    owner: Option<u64>,
    next_client_id: u64,
    size: Size,
    meta: SessionMeta,
    pty_tx: std::sync::mpsc::Sender<Vec<u8>>,
}

impl State {
    fn broadcast(&self, msg: &Message) {
        for c in self.clients.values() {
            let _ = c.tx.send(msg.clone());
        }
    }

    fn apply_size(&mut self, size: Size) {
        if size == self.size || size.cols == 0 || size.rows == 0 {
            return;
        }
        let res = self.master.resize(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Err(e) = res {
            eprintln!("resize failed: {e}");
            return;
        }
        self.parser.screen_mut().set_size(size.rows, size.cols);
        self.size = size;
    }

    fn set_owner(&mut self, id: Option<u64>) {
        if self.owner == id {
            return;
        }
        self.owner = id;
        if let Some(size) = id.and_then(|i| self.clients.get(&i)).map(|c| c.size) {
            self.apply_size(size);
        }
        self.broadcast(&Message::InputOwner(InputOwner { owner: id }));
    }

    fn snapshot(&self) -> Vec<u8> {
        let mut out = self.parser.screen().state_formatted();
        out.extend(self.parser.callbacks().formatted());
        out
    }
}

type Shared = Arc<Mutex<State>>;

/// Runs the host until the agent process exits.
pub async fn run(args: HostArgs) -> Result<()> {
    paths::ensure_dirs()?;
    let endpoint = paths::session_endpoint(&args.id)?;
    let mut listener = ipc::Listener::bind(&endpoint)?;

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: args.size.rows, cols: args.size.cols, pixel_width: 0, pixel_height: 0 })
        .context("openpty")?;
    let Some((prog, rest)) = args.command.split_first() else { bail!("empty command") };
    let mut cmd = CommandBuilder::new(prog);
    cmd.args(rest);
    cmd.cwd(&args.cwd);
    cmd.env("CQ_SESSION_ID", &args.id);
    let mut child = pair.slave.spawn_command(cmd).with_context(|| format!("spawn {prog}"))?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    let meta = SessionMeta {
        id: args.id.clone(),
        agent: args.agent.clone(),
        cwd: args.cwd.clone(),
        pipe: endpoint.clone(),
        host_pid: std::process::id(),
        child_pid: child.process_id(),
        started_at_ms: session::now_ms(),
        status: SessionStatus::Running,
        exit_code: None,
        agent_session_id: None,
    };
    session::save(&meta)?;

    let (pty_tx, pty_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        for buf in pty_rx {
            if writer.write_all(&buf).and_then(|_| writer.flush()).is_err() {
                break;
            }
        }
    });

    let state: Shared = Arc::new(Mutex::new(State {
        parser: vt100::Parser::new_with_callbacks(
            args.size.rows,
            args.size.cols,
            0,
            ExtraModes::default(),
        ),
        master: pair.master,
        clients: HashMap::new(),
        owner: None,
        next_client_id: 1,
        size: args.size,
        meta,
        pty_tx,
    }));

    {
        let state = state.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut st = state.lock().unwrap();
                        st.parser.process(&buf[..n]);
                        let replies = std::mem::take(&mut st.parser.callbacks_mut().replies);
                        if !replies.is_empty() {
                            let _ = st.pty_tx.send(replies);
                        }
                        st.broadcast(&Message::Output(strip_cpr_requests(&buf[..n])));
                    }
                }
            }
        });
    }

    let exit_code = {
        let accept_state = state.clone();
        let accept = async move {
            loop {
                match listener.accept().await {
                    Ok(stream) => {
                        let st = accept_state.clone();
                        tokio::spawn(async move {
                            if let Err(e) = serve_client(st, stream).await {
                                eprintln!("client error: {e:#}");
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("accept failed: {e:#}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        };
        let wait = tokio::task::spawn_blocking(move || child.wait());
        tokio::select! {
            _ = accept => unreachable!(),
            r = wait => r?.ok().map(|s| s.exit_code()),
        }
    };

    // Give the reader thread a moment to forward the agent's final output.
    tokio::time::sleep(Duration::from_millis(200)).await;
    {
        let mut st = state.lock().unwrap();
        st.meta.status = SessionStatus::Exited;
        st.meta.exit_code = exit_code;
        let _ = session::save(&st.meta);
        st.broadcast(&Message::Exit(Exit { code: exit_code }));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    ipc::cleanup(&endpoint);
    Ok(())
}

async fn serve_client(state: Shared, stream: ipc::BoxStream) -> Result<()> {
    let (mut rd, mut wr) = tokio::io::split(stream);
    let hello: Hello =
        match tokio::time::timeout(Duration::from_secs(5), read_message(&mut rd)).await?? {
            Some(Message::Hello(h)) => h,
            _ => bail!("expected hello"),
        };
    if hello.protocol_version != PROTOCOL_VERSION {
        let msg = format!(
            "protocol mismatch: host {PROTOCOL_VERSION}, client {}",
            hello.protocol_version
        );
        write_message(&mut wr, &Message::Error(msg.clone())).await?;
        bail!(msg);
    }

    if hello.kind == ClientKind::Event {
        while let Some(msg) = read_message(&mut rd).await? {
            if let Message::Event(ev) = msg {
                handle_event(&state, ev);
            }
        }
        return Ok(());
    }

    let (tx, mut rx) = mpsc::unbounded_channel();
    let id = {
        let mut st = state.lock().unwrap();
        let id = st.next_client_id;
        st.next_client_id += 1;
        let size = Size { cols: hello.cols, rows: hello.rows };
        let _ = tx.send(Message::Welcome(Welcome {
            protocol_version: PROTOCOL_VERSION,
            client_id: id,
            session: st.meta.clone(),
        }));
        // Snapshot and subscription happen under one lock so no output is lost or duplicated.
        let _ = tx.send(Message::Snapshot(st.snapshot()));
        let _ = tx.send(Message::InputOwner(InputOwner { owner: st.owner }));
        st.clients.insert(id, Client { tx, size });
        if st.owner.is_none() {
            st.set_owner(Some(id));
        }
        id
    };

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write_message(&mut wr, &msg).await.is_err() {
                break;
            }
        }
    });

    let result = async {
        while let Some(msg) = read_message(&mut rd).await? {
            let mut st = state.lock().unwrap();
            match msg {
                Message::Input(bytes) => {
                    // Last writer wins the input right, and the PTY follows its size.
                    st.set_owner(Some(id));
                    let _ = st.pty_tx.send(bytes);
                }
                Message::Resize(size) => {
                    if let Some(c) = st.clients.get_mut(&id) {
                        c.size = size;
                    }
                    if st.owner == Some(id) {
                        st.apply_size(size);
                    }
                }
                _ => {}
            }
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;

    {
        let mut st = state.lock().unwrap();
        st.clients.remove(&id);
        if st.owner == Some(id) {
            st.set_owner(None);
        }
    }
    writer.abort();
    result
}

/// Removes cursor position requests so attached terminals do not send duplicate replies.
fn strip_cpr_requests(buf: &[u8]) -> Vec<u8> {
    const REQ: &[u8] = b"\x1b[6n";
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        if buf[i..].starts_with(REQ) {
            i += REQ.len();
        } else {
            out.push(buf[i]);
            i += 1;
        }
    }
    out
}

fn handle_event(state: &Shared, ev: AgentEvent) {
    let mut st = state.lock().unwrap();
    if ev.source == "hook"
        && ev.payload.get("hook_event_name").and_then(|v| v.as_str()) == Some("SessionStart")
    {
        if let Some(sid) = ev.payload.get("session_id").and_then(|v| v.as_str()) {
            st.meta.agent_session_id = Some(sid.to_owned());
            let _ = session::save(&st.meta);
        }
    }
    st.broadcast(&Message::Event(ev));
}
