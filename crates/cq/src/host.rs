//! Session host (`cq host`): owns the agent's PTY, keeps a screen grid, and fans output out to
//! every attached client. Runs detached so the session survives its terminal and the hub.

use crate::agent::{self, PermissionPrompt};
use crate::{ipc, paths, session, usage};
use anyhow::{Context, Result, bail};
use cq_proto::{
    AgentEvent, ClientKind, Exit, Hello, InputOwner, Message, PROTOCOL_VERSION, SessionMeta,
    SessionStatus, Size, Welcome, read_message, write_message,
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::{HashMap, VecDeque};
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

/// Environment variables Claude Code sets for its own child processes.
const NESTED_SESSION_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

/// Number of recent agent events replayed to newly connected hubs and monitors.
const EVENT_HISTORY: usize = 200;

struct Client {
    tx: mpsc::UnboundedSender<Message>,
    size: Size,
    /// Monitors receive events but no terminal output, and never own input.
    monitor: bool,
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
    events: VecDeque<AgentEvent>,
    /// Latest value of each state event kind, replayed after the history.
    latest: HashMap<String, AgentEvent>,
    account: Option<usage::Account>,
    /// Set to force an account re-read (e.g. after SessionStart, which follows /login).
    account_recheck: bool,
    transcript: usage::TranscriptTail,
    /// Set when output arrived since the last permission prompt scan.
    dirty: bool,
    prompt: Option<PermissionPrompt>,
}

impl State {
    fn broadcast(&self, msg: &Message) {
        for c in self.clients.values() {
            let _ = c.tx.send(msg.clone());
        }
    }

    fn broadcast_output(&self, msg: &Message) {
        for c in self.clients.values().filter(|c| !c.monitor) {
            let _ = c.tx.send(msg.clone());
        }
    }

    /// Records an event and sends it to every client. Frequently repeated state events (status
    /// line, account, tokens) keep only their latest value so they never evict history.
    fn publish(&mut self, ev: AgentEvent) {
        let slot = match ev.source.as_str() {
            "statusline" => Some("statusline".to_owned()),
            "cq" => match ev.payload.get("type").and_then(|t| t.as_str()) {
                Some(t @ ("account" | "tokens")) => Some(t.to_owned()),
                _ => None,
            },
            _ => None,
        };
        if let Some(slot) = slot {
            self.latest.insert(slot, ev.clone());
        } else {
            if self.events.len() == EVENT_HISTORY {
                self.events.pop_front();
            }
            self.events.push_back(ev.clone());
        }
        self.broadcast(&Message::Event(ev));
    }

    fn prompt_event(&self) -> AgentEvent {
        let payload = match &self.prompt {
            Some(p) => serde_json::json!({ "type": "permission_prompt", "prompt": p }),
            None => serde_json::json!({ "type": "permission_resolved" }),
        };
        AgentEvent { source: "cq".into(), received_at_ms: session::now_ms(), payload }
    }

    /// Re-scans the screen and publishes prompt appearance or resolution.
    fn scan_prompt(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let found = match self.meta.agent.as_str() {
            "claude" => agent::find_claude_permission_prompt(&self.parser.screen().contents()),
            _ => None,
        };
        if found != self.prompt {
            self.prompt = found;
            let ev = self.prompt_event();
            self.publish(ev);
        }
    }

    fn approve(&mut self, option: u8) -> std::result::Result<(), String> {
        // Answer only a prompt that is visible right now, so a stale dialog cannot approve a
        // different request.
        self.dirty = true;
        self.scan_prompt();
        let Some(p) = &self.prompt else { return Err("no permission prompt on screen".into()) };
        if !p.options.iter().any(|o| o.number == option) {
            return Err(format!("option {option} not offered"));
        }
        self.pty_tx.send(vec![b'0' + option]).map_err(|e| e.to_string())
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
    // Markers of an enclosing Claude Code session (e.g. when cqd was started from one) would
    // make the agent believe it is a child session and, among other things, disable transcripts.
    for var in NESTED_SESSION_VARS {
        cmd.env_remove(var);
    }
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
        events: VecDeque::new(),
        latest: HashMap::new(),
        account: None,
        account_recheck: true,
        transcript: usage::TranscriptTail::default(),
        dirty: false,
        prompt: None,
    }));

    {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(400)).await;
                state.lock().unwrap().scan_prompt();
            }
        });
    }
    if args.agent == "claude" {
        tokio::spawn(poll_usage(state.clone()));
    }

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
                        st.dirty = true;
                        st.broadcast_output(&Message::Output(strip_cpr_requests(&buf[..n])));
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

    let monitor = hello.kind == ClientKind::Monitor;
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
        if !monitor {
            let _ = tx.send(Message::Snapshot(st.snapshot()));
        }
        let _ = tx.send(Message::InputOwner(InputOwner { owner: st.owner }));
        if hello.kind != ClientKind::Terminal {
            for ev in st.events.iter().chain(st.latest.values()) {
                let _ = tx.send(Message::Event(ev.clone()));
            }
            if st.prompt.is_some() {
                let _ = tx.send(Message::Event(st.prompt_event()));
            }
        }
        st.clients.insert(id, Client { tx, size, monitor });
        if st.owner.is_none() && !monitor {
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
                Message::Input(_) | Message::Resize(_) if monitor => {}
                Message::Approve(a) => {
                    let payload = match st.approve(a.option) {
                        Ok(()) => serde_json::json!({
                            "type": "approval_result", "ok": true, "option": a.option,
                        }),
                        Err(e) => serde_json::json!({
                            "type": "approval_result", "ok": false, "option": a.option, "reason": e,
                        }),
                    };
                    let ev =
                        AgentEvent { source: "cq".into(), received_at_ms: session::now_ms(), payload };
                    if let Some(c) = st.clients.get(&id) {
                        let _ = c.tx.send(Message::Event(ev));
                    }
                }
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

fn cq_event(payload: serde_json::Value) -> AgentEvent {
    AgentEvent { source: "cq".into(), received_at_ms: session::now_ms(), payload }
}

/// Publishes account changes and transcript token totals every two seconds.
async fn poll_usage(state: Shared) {
    let mut stamp = None;
    loop {
        let recheck = std::mem::take(&mut state.lock().unwrap().account_recheck);
        let now_stamp = usage::account_stamp();
        if recheck || now_stamp != stamp {
            stamp = now_stamp;
            let account = tokio::task::spawn_blocking(usage::read_claude_account).await.ok().flatten();
            let mut st = state.lock().unwrap();
            if account != st.account {
                st.account = account.clone();
                st.publish(cq_event(serde_json::json!({ "type": "account", "account": account })));
            }
        }
        {
            let mut st = state.lock().unwrap();
            if let Some(totals) = st.transcript.poll() {
                st.publish(cq_event(serde_json::json!({ "type": "tokens", "tokens": totals })));
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn handle_event(state: &Shared, ev: AgentEvent) {
    let mut st = state.lock().unwrap();
    // Agent events are a good moment to re-check the screen (e.g. Notification on a prompt).
    st.dirty = true;
    if let Some(path) = ev.payload.get("transcript_path").and_then(|v| v.as_str()) {
        st.transcript.set_path(path.into());
    }
    if ev.source == "hook"
        && ev.payload.get("hook_event_name").and_then(|v| v.as_str()) == Some("SessionStart")
    {
        st.account_recheck = true;
        if let Some(sid) = ev.payload.get("session_id").and_then(|v| v.as_str()) {
            st.meta.agent_session_id = Some(sid.to_owned());
            let _ = session::save(&st.meta);
        }
    }
    st.publish(ev);
}
