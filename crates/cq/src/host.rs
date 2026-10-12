//! Session host (`cq host`): owns the agent's PTY, keeps a screen grid, and fans output out to
//! every attached client. Runs detached so the session survives its terminal and the hub.

use crate::agent::{self, PermissionPrompt};
use crate::{ipc, paths, session, usage};
use anyhow::{Context, Result, bail};
use cq_protocol::{
    AgentEvent, ClientKind, Exit, Hello, Inject, InputOwner, Message, PROTOCOL_VERSION, SessionMeta, Rename,
    SessionStatus, Size, Terminate, Welcome, read_message, write_message,
};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
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
    pub name: Option<String>,
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

/// Internal state of whatever started the daemon, which must not reach the agents: an enclosing
/// Claude Code session (the agent would take itself for a child session and, among other
/// things, disable transcripts), or a Volta-managed tool (with `_VOLTA_TOOL_RECURSION` set,
/// Volta's shims skip their own lookup and the agents' hooks fail to find `node`).
const LAUNCHER_VARS: &[&str] = &[
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
    "_VOLTA_TOOL_RECURSION",
];

/// Finds the executable for a bare command name. On Windows, PATH may hold an extensionless
/// shell shim next to the real `.exe` / `.cmd` (npm, Volta), so `PATHEXT` entries are preferred,
/// and batch files run through `cmd.exe /c`. Returns the program and arguments to put before the
/// caller's.
fn resolve_program(prog: &str) -> (String, Vec<String>) {
    if !cfg!(windows) || prog.contains(['/', '\\']) {
        return (prog.to_owned(), Vec::new());
    }
    match paths::find_program(prog, paths::path_dirs()) {
        Some(p) => with_batch_wrapper(&p),
        None => (prog.to_owned(), Vec::new()),
    }
}

/// Runs `.cmd` / `.bat` files through `cmd.exe /c`; returns the program and leading arguments.
pub fn with_batch_wrapper(p: &std::path::Path) -> (String, Vec<String>) {
    let path = p.to_string_lossy().into_owned();
    let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
    if matches!(ext.as_deref(), Some("cmd" | "bat")) {
        ("cmd.exe".to_owned(), vec!["/c".to_owned(), path])
    } else {
        (path, Vec::new())
    }
}

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
    codex_rollout: usage::CodexTail,
    /// Set when output arrived since the last permission prompt scan.
    dirty: bool,
    prompt: Option<PermissionPrompt>,
    /// The agent finished its turn (Stop / SessionStart) and has not received a prompt since.
    agent_idle: bool,
    /// Last time a client typed into the session.
    last_input: std::time::Instant,
    /// Text waiting for the session to become idle.
    injections: VecDeque<Inject>,
    /// Injected text was submitted (first and latest Enter) and no prompt has been submitted since.
    pending_submit: Option<(std::time::Instant, std::time::Instant)>,
    /// Codex: an answered prompt that may still need Enter to confirm the selection.
    pending_confirm: Option<(String, std::time::Instant)>,
    /// Kills the agent process; used when a graceful exit does not finish in time.
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// A terminate request is in progress.
    terminating: bool,
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
            // Whether the agent is between turns, so clients know what the session went back to.
            None => serde_json::json!({ "type": "permission_resolved", "idle": self.agent_idle }),
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
            "claude" | "codex" => agent::find_choice_prompt(&screen_lines(self.parser.screen())),
            _ => None,
        };
        if found != self.prompt {
            self.prompt = found;
            let ev = self.prompt_event();
            self.publish(ev);
        }
    }

    fn approve(&mut self, option: u8, prompt_id: Option<&str>) -> std::result::Result<(), String> {
        // Answer only a prompt that is visible right now, so a stale dialog cannot approve a
        // different request.
        self.dirty = true;
        self.scan_prompt();
        let Some(p) = &self.prompt else { return Err("no permission prompt on screen".into()) };
        if prompt_id.is_some_and(|id| id != p.id) {
            return Err("the prompt on screen has changed".into());
        }
        if !p.options.iter().any(|o| o.number == option) {
            return Err(format!("option {option} not offered"));
        }
        let id = p.id.clone();
        if let Some(selected) = p.selected {
            let key: &[u8] = match (option > selected, self.parser.screen().application_cursor()) {
                (true, true) => b"\x1bOB",
                (true, false) => b"\x1b[B",
                (false, true) => b"\x1bOA",
                (false, false) => b"\x1b[A",
            };
            self.pty_tx.send(key.repeat(usize::from(option.abs_diff(selected)))).map_err(|e| e.to_string())?;
            // Enter separately, so the keys are not taken for a paste.
            self.send_later(b"\r", 150);
            return Ok(());
        }
        self.pty_tx.send(vec![b'0' + option]).map_err(|e| e.to_string())?;
        if self.meta.agent == "codex" {
            // Some Codex lists confirm on the digit, others only move the selection.
            self.pending_confirm = Some((id, std::time::Instant::now()));
        }
        Ok(())
    }

    /// Codex: presses Enter for an answered prompt that is still on screen.
    fn confirm_pending(&mut self) {
        let Some((id, at)) = &self.pending_confirm else { return };
        if at.elapsed() < Duration::from_millis(400) {
            return;
        }
        if self.prompt.as_ref().is_some_and(|p| &p.id == id) {
            let _ = self.pty_tx.send(b"\r".to_vec());
        }
        self.pending_confirm = None;
    }

    /// Sends bytes to the agent after a delay.
    fn send_later(&self, bytes: &'static [u8], ms: u64) {
        let tx = self.pty_tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let _ = tx.send(bytes.to_vec());
        });
    }

    /// Writes input to the agent. Codex treats a burst of input as a paste, so the Enter that
    /// ends a pasted message is sent a moment later to submit it.
    fn write_input(&mut self, mut bytes: Vec<u8>) {
        let tail = b"\x1b[201~\r";
        if self.meta.agent == "codex" && bytes.ends_with(tail) {
            bytes.pop();
            let _ = self.pty_tx.send(bytes);
            self.send_later(b"\r", 500);
            return;
        }
        let _ = self.pty_tx.send(bytes);
    }

    fn rename(&mut self, r: Rename) {
        let name = r.name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        if let Some(taken) = name.as_deref().and_then(|n| session::name_taken(n, &self.meta.id)) {
            let reason = format!("session {taken} is already named '{}'", name.unwrap_or_default());
            self.publish(cq_event(serde_json::json!({ "type": "rejected", "reason": reason })));
            return;
        }
        self.meta.name = name;
        let _ = session::save(&self.meta);
        self.publish(cq_event(serde_json::json!({ "type": "meta", "name": self.meta.name })));
    }

    /// Types the next queued injection when the agent is idle, no prompt or typed text is on
    /// screen, and nobody has typed for a while.
    fn try_inject(&mut self) {
        let Some(next) = self.injections.front() else { return };
        if !self.agent_idle || self.prompt.is_some() {
            return;
        }
        if self.last_input.elapsed() < Duration::from_millis(next.quiet_ms) {
            return;
        }
        if self.input_empty() != Some(true) {
            return;
        }
        let inj = self.injections.pop_front().expect("checked above");
        let mut bytes = format!("\x1b[200~{}\x1b[201~", inj.text).into_bytes();
        if inj.submit {
            bytes.push(b'\r');
            self.agent_idle = false;
            self.pending_submit = Some((std::time::Instant::now(), std::time::Instant::now()));
        }
        self.write_input(bytes);
        let ok = true;
        self.last_input = std::time::Instant::now();
        let payload = serde_json::json!({ "type": "inject_result", "id": inj.id, "ok": ok });
        self.publish(AgentEvent { source: "cq".into(), received_at_ms: session::now_ms(), payload });
    }

    fn input_empty(&self) -> Option<bool> {
        match self.meta.agent.as_str() {
            "claude" => agent::claude_input_empty(self.parser.screen()),
            "codex" => agent::codex_input_empty(self.parser.screen()),
            _ => Some(true),
        }
    }

    /// Presses Enter again while injected text still sits in the input box without a prompt
    /// having been submitted. Codex on Windows reads a long paste as fast typing and may take
    /// the Enter after it as a line break.
    fn confirm_submit(&mut self) {
        let Some((first, latest)) = self.pending_submit else { return };
        // Give up after a while, or once someone types (the text may be theirs now).
        if first.elapsed() > Duration::from_secs(10) || self.last_input.duration_since(first) > Duration::from_millis(100) {
            self.pending_submit = None;
        } else if latest.elapsed() > Duration::from_millis(1500) && self.prompt.is_none() && self.input_empty() == Some(false) {
            let _ = self.pty_tx.send(b"\r".to_vec());
            self.pending_submit = Some((first, std::time::Instant::now()));
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
    let (program, prefix) = resolve_program(prog);
    let mut cmd = CommandBuilder::new(&program);
    cmd.args(&prefix);
    cmd.args(rest);
    cmd.cwd(&args.cwd);
    cmd.env("CQ_SESSION_ID", &args.id);
    // A daemon started over ssh has no TERM; agents would then draw without colors.
    if cfg!(unix) && std::env::var_os("TERM").is_none_or(|t| t == "dumb") {
        cmd.env("TERM", "xterm-256color");
        if std::env::var_os("COLORTERM").is_none() {
            cmd.env("COLORTERM", "truecolor");
        }
    }
    for var in LAUNCHER_VARS {
        cmd.env_remove(var);
    }
    let mut child = pair.slave.spawn_command(cmd).with_context(|| format!("spawn {prog}"))?;
    drop(pair.slave);
    let killer = child.clone_killer();

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    // A resumed session keeps what its earlier run learned about the agent.
    let earlier: Option<SessionMeta> = std::fs::read(paths::session_file(&args.id)?).ok().and_then(|d| serde_json::from_slice(&d).ok());
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
        ended_at_ms: None,
        agent_session_id: earlier.as_ref().and_then(|m| m.agent_session_id.clone()),
        name: args.name.clone(),
        account: earlier.as_ref().and_then(|m| m.account.clone()),
        tokens_by_hour: earlier.map(|m| m.tokens_by_hour).unwrap_or_default(),
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
        codex_rollout: usage::CodexTail::default(),
        dirty: false,
        prompt: None,
        // Codex fires SessionStart only with the first turn, so it starts out idle.
        agent_idle: args.agent == "codex",
        last_input: std::time::Instant::now(),
        injections: VecDeque::new(),
        pending_submit: None,
        pending_confirm: None,
        killer,
        terminating: false,
    }));

    {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(400)).await;
                let mut st = state.lock().unwrap();
                st.scan_prompt();
                st.try_inject();
                st.confirm_pending();
                st.confirm_submit();
            }
        });
    }
    if matches!(args.agent.as_str(), "claude" | "codex") {
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
                        // vt100 can panic on some output (a wide character in the last column).
                        // Panicking here would poison the lock and wedge the session, so start a
                        // fresh screen instead; the agent's next redraw fills it again.
                        let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| st.parser.process(&buf[..n])));
                        if parsed.is_err() {
                            st.parser = vt100::Parser::new_with_callbacks(st.size.rows, st.size.cols, 0, ExtraModes::default());
                        }
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
        st.meta.ended_at_ms = Some(session::now_ms());
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
                // Monitors (e.g. an orchestrator through the daemon) may type, but never take the input
                // right or the PTY size from an attached terminal.
                Message::Input(bytes) if monitor => {
                    st.last_input = std::time::Instant::now();
                    st.write_input(bytes);
                }
                Message::Inject(inj) => {
                    let payload = serde_json::json!({ "type": "inject_queued", "id": inj.id });
                    st.injections.push_back(inj);
                    st.publish(AgentEvent { source: "cq".into(), received_at_ms: session::now_ms(), payload });
                }
                Message::Rename(r) => st.rename(r),
                Message::Terminate(t) => {
                    if !std::mem::replace(&mut st.terminating, true) {
                        let payload = serde_json::json!({ "type": "terminating", "force": t.force });
                        st.publish(cq_event(payload));
                        tokio::spawn(terminate(state.clone(), t));
                    }
                }
                Message::Resize(_) if monitor => {}
                Message::Approve(a) => {
                    let payload = match st.approve(a.option, a.prompt_id.as_deref()) {
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
                    st.last_input = std::time::Instant::now();
                    st.write_input(bytes);
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

/// Ends the agent the way a user would, so it can clean up (e.g. archive its Remote Control
/// session), and kills it if it is still running when the grace period ends.
///
/// Steps: Esc closes a dialog or interrupts a turn; Ctrl+C clears typed text and, pressed again,
/// quits both Claude Code and Codex; if that did not work, the exit command is typed.
async fn terminate(state: Shared, t: Terminate) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(t.grace_ms);
    let agent = state.lock().unwrap().meta.agent.clone();
    let exit_command: Option<&[u8]> = match agent.as_str() {
        "claude" => Some(b"/exit"),
        "codex" => Some(b"/quit"),
        _ => None,
    };
    if !t.force {
        if let Some(command) = exit_command {
            let send = |bytes: &[u8]| {
                let _ = state.lock().unwrap().pty_tx.send(bytes.to_vec());
            };
            let pause = |ms| tokio::time::sleep_until((tokio::time::Instant::now() + Duration::from_millis(ms)).min(deadline));
            send(b"\x1b");
            pause(800).await;
            for _ in 0..3 {
                send(b"\x03");
                pause(300).await;
            }
            pause(2000).await;
            // Still running: Ctrl+C may have only interrupted a turn. Type the exit command.
            send(b"\x03");
            pause(300).await;
            send(command);
            pause(400).await;
            send(b"\r");
        }
        tokio::time::sleep_until(deadline).await;
    }
    // Reaching this point means the agent did not exit; the host would have ended otherwise.
    let mut st = state.lock().unwrap();
    st.publish(cq_event(serde_json::json!({ "type": "terminate_forced" })));
    if let Err(e) = st.killer.kill() {
        eprintln!("kill failed: {e}");
    }
}

/// Publishes account changes and token totals (and, for Codex, its status) every two seconds,
/// and keeps them in the session's metadata (saved at most once a minute, and at exit).
async fn poll_usage(state: Shared) {
    let codex = state.lock().unwrap().meta.agent == "codex";
    let mut stamp = None;
    let mut saved = std::time::Instant::now();
    let mut unsaved = false;
    loop {
        let recheck = std::mem::take(&mut state.lock().unwrap().account_recheck);
        let now_stamp = if codex { usage::codex_account_stamp() } else { usage::account_stamp() };
        if recheck || now_stamp != stamp {
            stamp = now_stamp;
            let read = if codex { usage::read_codex_account } else { usage::read_claude_account };
            let account = tokio::task::spawn_blocking(read).await.ok().flatten();
            let mut st = state.lock().unwrap();
            if account != st.account {
                st.account = account.clone();
                st.meta.account = account.as_ref().map(|a| a.label.clone());
                let _ = session::save(&st.meta);
                st.publish(cq_event(serde_json::json!({ "type": "account", "account": account })));
            }
        }
        {
            let mut st = state.lock().unwrap();
            let tokens = if codex {
                let u = st.codex_rollout.poll();
                if let Some(status) = u.as_ref().and_then(|u| u.status.clone()) {
                    // Codex has no status line; publish the same shape from its rollout.
                    st.publish(AgentEvent { source: "statusline".into(), received_at_ms: session::now_ms(), payload: status });
                }
                u.and_then(|u| u.tokens)
            } else {
                st.transcript.poll()
            };
            if let Some(totals) = tokens {
                st.publish(cq_event(serde_json::json!({ "type": "tokens", "tokens": totals })));
                unsaved |= st.meta.tokens_by_hour != totals.by_hour;
                st.meta.tokens_by_hour = totals.by_hour;
            }
            if unsaved && saved.elapsed() >= Duration::from_secs(60) {
                (saved, unsaved) = (std::time::Instant::now(), false);
                let _ = session::save(&st.meta);
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
        if st.meta.agent == "codex" {
            st.codex_rollout.set_path(path.into());
        } else {
            st.transcript.set_path(path.into());
        }
    }
    let hook = if ev.source == "hook" { ev.payload.get("hook_event_name").and_then(|v| v.as_str()) } else { None };
    match hook {
        Some("SessionStart" | "Stop") => st.agent_idle = true,
        Some("UserPromptSubmit") => {
            st.agent_idle = false;
            st.pending_submit = None;
        }
        _ => {}
    }
    if hook == Some("SessionStart") {
        st.account_recheck = true;
        if let Some(sid) = ev.payload.get("session_id").and_then(|v| v.as_str()) {
            st.meta.agent_session_id = Some(sid.to_owned());
            let _ = session::save(&st.meta);
        }
    }
    st.publish(ev);
}

/// The visible screen as text, one line per row. Unlike `Screen::contents`, rows filled up to the
/// last column are not joined with the next one (Codex pads highlighted rows to full width).
pub fn screen_lines(screen: &vt100::Screen) -> String {
    let (_, cols) = screen.size();
    screen.rows(0, cols).map(|r| r.trim_end().to_owned()).collect::<Vec<_>>().join("\n")
}
