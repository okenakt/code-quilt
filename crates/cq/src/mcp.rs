//! `cq mcp`: an MCP server (stdio, JSON-RPC 2.0) that lets an agent see and drive other
//! sessions through the local daemon. Agent-agnostic: Claude Code and Codex both speak MCP.

use anyhow::{Result, anyhow, bail};
use cq_client::{Activity, MAX_TURNS, Model, Session};
use cq_protocol::{LOCAL_MACHINE, Message};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

type Approvals = Arc<Mutex<HashMap<(String, String), oneshot::Sender<Value>>>>;

struct Server {
    model: Arc<Mutex<Model>>,
    approvals: Approvals,
    tx: mpsc::UnboundedSender<Message>,
    scope: cq_client::Scope,
    /// Declare the Claude Code channel capability.
    channel: bool,
    /// Serve a worker session (only [`WORKER_TOOLS`]) rather than an orchestrator.
    worker: bool,
    /// The session this server runs inside (the orchestrator itself), if any.
    own: Option<String>,
}

fn snippet(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

impl Server {
    fn visible(&self, s: &Session) -> bool {
        self.scope.admits(&s.machine) && self.own.as_deref() != Some(&s.meta.id)
    }

    /// Resolves `machine:id`, a session name, or an id prefix to one visible session.
    fn resolve(&self, target: &str) -> Result<Session> {
        let m = self.model.lock().unwrap();
        let live: Vec<&Session> = m.sessions.values().filter(|s| self.visible(s) && s.activity != Activity::Exited).collect();
        let (machine, rest) = match target.split_once(':') {
            Some((mm, r)) if m.machines.iter().any(|x| x.id == mm) || mm == LOCAL_MACHINE => (Some(mm), r),
            _ => (None, target),
        };
        let pick = |pred: &dyn Fn(&Session) -> bool| -> Vec<Session> {
            live.iter().filter(|s| machine.is_none_or(|mm| s.machine == mm) && pred(s)).map(|s| (*s).clone()).collect()
        };
        for hits in [
            pick(&|s| s.meta.name.as_deref() == Some(rest)),
            pick(&|s| s.meta.id == rest),
            pick(&|s| s.meta.id.starts_with(rest)),
        ] {
            match hits.len() {
                0 => continue,
                1 => return Ok(hits.into_iter().next().unwrap()),
                n => bail!("'{target}' matches {n} sessions; use machine:id"),
            }
        }
        bail!("no running session in scope matches '{target}'")
    }

    fn describe(s: &Session) -> Value {
        let last = s.turns.back();
        json!({
            "session": s.key(),
            "name": s.meta.name,
            "machine": s.machine,
            "agent": s.meta.agent,
            "cwd": s.meta.cwd,
            "state": s.activity.as_str(),
            "permission_prompt": s.prompt,
            "last_prompt": last.and_then(|t| t.prompt.as_deref()).map(|p| snippet(p, 200)),
            "last_reply": last.and_then(|t| t.reply.as_deref()).map(|r| snippet(r, 400)),
        })
    }

    fn list_sessions(&self, args: &Value) -> Result<Value> {
        let include_exited = args.get("include_exited").and_then(Value::as_bool).unwrap_or(false);
        let m = self.model.lock().unwrap();
        let mut out: Vec<_> = m
            .sessions
            .values()
            .filter(|s| self.visible(s) && (include_exited || s.activity != Activity::Exited))
            .collect();
        out.sort_by_key(|s| (s.machine.clone(), s.meta.started_at_ms));
        let machines: Vec<_> = m.machines.iter().map(|x| json!({ "id": x.id, "label": x.label, "online": x.online })).collect();
        Ok(json!({ "machines": machines, "sessions": out.into_iter().map(Self::describe).collect::<Vec<_>>() }))
    }

    fn get_turns(&self, args: &Value) -> Result<Value> {
        let s = self.resolve(str_arg(args, "session")?)?;
        let n = args.get("turns").and_then(Value::as_u64).unwrap_or(1).clamp(1, MAX_TURNS as u64) as usize;
        let turns: Vec<_> = s
            .turns
            .iter()
            .rev()
            .take(n)
            .rev()
            .map(|t| json!({ "prompt": t.prompt, "reply": t.reply, "at_ms": t.at_ms }))
            .collect();
        Ok(json!({ "session": s.key(), "name": s.meta.name, "state": s.activity.as_str(), "permission_prompt": s.prompt, "turns": turns }))
    }

    /// Queues a message from this session for another one: it is typed in once that session is
    /// idle and nobody is typing there, marked with who sent it.
    fn message(&self, args: &Value) -> Result<Value> {
        let s = self.resolve(str_arg(args, "session")?)?;
        let text = str_arg(args, "text")?;
        let from = {
            let m = self.model.lock().unwrap();
            let own = self.own.as_deref().and_then(|id| m.sessions.get(&(LOCAL_MACHINE.to_owned(), id.to_owned())));
            own.map_or_else(|| "outside CodeQuilt".to_owned(), |o| format!("{} ({})", o.label(), o.key()))
        };
        let inject = cq_protocol::Inject { id: crate::session::now_ms(), text: format!("[from {from}] {text}"), submit: true, quiet_ms: 3000 };
        self.tx
            .send(Message::Routed { machine: s.machine.clone(), session: s.meta.id.clone(), inner: Box::new(Message::Inject(inject)) })
            .map_err(|_| anyhow!("daemon connection closed"))?;
        Ok(json!({ "queued_for": s.key(), "name": s.meta.name }))
    }

    fn send(&self, args: &Value) -> Result<Value> {
        let s = self.resolve(str_arg(args, "session")?)?;
        let text = str_arg(args, "text")?;
        let submit = args.get("submit").and_then(Value::as_bool).unwrap_or(true);
        let mut bytes = format!("\x1b[200~{text}\x1b[201~").into_bytes();
        if submit {
            bytes.push(b'\r');
        }
        self.tx
            .send(Message::Routed { machine: s.machine.clone(), session: s.meta.id.clone(), inner: Box::new(Message::Input(bytes)) })
            .map_err(|_| anyhow!("daemon connection closed"))?;
        Ok(json!({ "sent_to": s.key(), "name": s.meta.name, "submitted": submit }))
    }

    /// Starts a session and queues the first prompt, which is typed once the session is idle
    /// (after any startup dialogs).
    async fn launch(&self, args: &Value) -> Result<Value> {
        let cwd = str_arg(args, "cwd")?.to_owned();
        let agent = args.get("agent").and_then(Value::as_str).unwrap_or("claude");
        if !matches!(agent, "claude" | "codex") {
            bail!("unknown agent '{agent}' (claude or codex)");
        }
        let machine = args.get("machine").and_then(Value::as_str).unwrap_or(LOCAL_MACHINE).to_owned();
        if !self.scope.admits(&machine) {
            bail!("machine '{machine}' is outside this server's scope");
        }
        let name = args.get("name").and_then(Value::as_str).map(str::to_owned);
        let mut conn = cq_client::DaemonClient::open(machine != LOCAL_MACHINE).await?;
        let launch = cq_protocol::Launch {
            id: 1,
            machine: machine.clone(),
            agent: agent.to_owned(),
            cwd,
            args: Vec::new(),
            name: name.clone(),
            no_mcp: false,
            cols: 120,
            rows: 36,
        };
        let session = tokio::time::timeout(Duration::from_secs(30), conn.launch(launch))
            .await
            .map_err(|_| anyhow!("the session did not start within 30s"))??;
        let mut out = json!({ "session": format!("{machine}:{session}"), "name": name });
        if let Some(prompt) = args.get("prompt").and_then(Value::as_str) {
            let inject = cq_protocol::Inject { id: crate::session::now_ms(), text: prompt.to_owned(), submit: true, quiet_ms: 3000 };
            conn.send(&Message::Routed { machine: machine.clone(), session: session.clone(), inner: Box::new(Message::Inject(inject)) }).await?;
            let queued = async {
                while let Some(msg) = conn.msgs.recv().await {
                    if let Message::Routed { session: s, inner, .. } = msg?
                        && s == session
                        && matches!(&*inner, Message::Event(ev) if ev.payload.get("type").and_then(Value::as_str) == Some("inject_queued"))
                    {
                        return Ok(());
                    }
                }
                bail!("daemon connection closed")
            };
            tokio::time::timeout(Duration::from_secs(20), queued).await.map_err(|_| anyhow!("the session started but did not accept the prompt"))??;
            out["prompt"] = json!("queued; it is typed once the session is idle");
        }
        Ok(out)
    }

    async fn approve(&self, args: &Value) -> Result<Value> {
        let s = self.resolve(str_arg(args, "session")?)?;
        let option = args.get("option").and_then(Value::as_u64).ok_or_else(|| anyhow!("missing 'option'"))? as u8;
        let prompt_id = str_arg(args, "prompt_id")?.to_owned();
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().unwrap().insert((s.machine.clone(), s.meta.id.clone()), tx);
        let approve = cq_protocol::Approve { option, prompt_id: Some(prompt_id) };
        self.tx
            .send(Message::Routed { machine: s.machine.clone(), session: s.meta.id.clone(), inner: Box::new(Message::Approve(approve)) })
            .map_err(|_| anyhow!("daemon connection closed"))?;
        let result = tokio::time::timeout(Duration::from_secs(15), rx).await.map_err(|_| anyhow!("no answer from the session"))??;
        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            bail!("approval refused: {}", result.get("reason").and_then(Value::as_str).unwrap_or("unknown"));
        }
        Ok(json!({ "session": s.key(), "option": option, "ok": true }))
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| anyhow!("missing '{key}'"))
}

/// The tools a worker session gets: it can see the other sessions and leave them messages, but not
/// type for the user, answer their prompts or start sessions.
pub const WORKER_TOOLS: [&str; 3] = ["list_sessions", "get_turns", "message"];

/// The tools of this server; a worker gets only [`WORKER_TOOLS`].
fn tools(worker: bool) -> Value {
    let session = json!({ "type": "string", "description": "Session name, machine:id, or id prefix" });
    let all = json!([
        {
            "name": "message",
            "description": "Leave a message for another coding agent session. It is typed into that session as a prompt once the session is idle, starting with [from <your name> (<machine:id>)] so it can tell the message came from you and not from the user. Write it so it can be acted on without this conversation. When you receive such a message, reply with this tool only if a reply is needed.",
            "inputSchema": { "type": "object", "properties": { "session": session, "text": { "type": "string" } }, "required": ["session", "text"] }
        },
        {
            "name": "list_sessions",
            "description": "List coding agent sessions in scope: machine, name, directory, state (working / idle / approval / exited), any pending permission prompt, and the start of the last prompt and reply.",
            "inputSchema": { "type": "object", "properties": { "include_exited": { "type": "boolean" } } }
        },
        {
            "name": "get_turns",
            "description": "Get the latest turns (user prompt and agent reply, full text) of a session and its pending permission prompt.",
            "inputSchema": { "type": "object", "properties": { "session": session, "turns": { "type": "integer", "minimum": 1, "maximum": MAX_TURNS } }, "required": ["session"] }
        },
        {
            "name": "send",
            "description": "Type a message into a session as if the user typed it. Presses Enter unless submit is false.",
            "inputSchema": { "type": "object", "properties": { "session": session, "text": { "type": "string" }, "submit": { "type": "boolean" } }, "required": ["session", "text"] }
        },
        {
            "name": "launch",
            "description": "Start a new coding agent session in a directory. The directory must be inside the allowed roots of ~/.codequilt/config.toml or trusted in the agent. Give it a name to address it later. The optional prompt is typed once the session is idle, after any startup dialogs.",
            "inputSchema": { "type": "object", "properties": {
                "cwd": { "type": "string", "description": "Working directory on the target machine" },
                "agent": { "type": "string", "enum": ["claude", "codex"] },
                "machine": { "type": "string", "description": "Machine id; the local machine by default" },
                "name": { "type": "string" },
                "prompt": { "type": "string", "description": "First instruction for the new session" }
            }, "required": ["cwd"] }
        },
        {
            "name": "approve",
            "description": "Answer a session's permission prompt with one of its numbered options. Only on the user's explicit instruction. prompt_id must come from list_sessions or get_turns; the call fails if the prompt changed.",
            "inputSchema": { "type": "object", "properties": { "session": session, "prompt_id": { "type": "string" }, "option": { "type": "integer", "minimum": 1 } }, "required": ["session", "prompt_id", "option"] }
        }
    ]);
    let keep = |t: &&Value| !worker || t["name"].as_str().is_some_and(|n| WORKER_TOOLS.contains(&n));
    Value::Array(all.as_array().expect("array").iter().filter(keep).cloned().collect())
}

/// Name of the desktop command that delivers channel messages to the session `id`.
pub fn channel_command(id: &str) -> String {
    format!("channel:{id}")
}

/// Keeps the model in sync with the daemon and forwards outgoing messages. With `channel`, also
/// registers the session's channel command and turns its invocations into channel notifications.
async fn daemon_loop(
    model: Arc<Mutex<Model>>,
    approvals: Approvals,
    mut rx: mpsc::UnboundedReceiver<Message>,
    channel: Option<(String, mpsc::UnboundedSender<Value>)>,
) {
    loop {
        let run = async {
            let mut conn = cq_client::DaemonClient::open(true).await?;
            conn.send(&Message::ListSessions).await?;
            if let Some((name, _)) = &channel {
                let cmd = cq_protocol::CommandInfo { name: name.clone(), title: "Deliver a channel message".into(), ..Default::default() };
                conn.send(&Message::RegisterCommand(cmd)).await?;
            }
            loop {
                tokio::select! {
                    msg = conn.msgs.recv() => {
                        let Some(msg) = msg else { return Ok::<_, anyhow::Error>(()) };
                        let msg = msg?;
                        if let Some((machine, session, ev)) = model.lock().unwrap().apply(&msg) {
                            if ev.source == "cq" && ev.payload.get("type").and_then(Value::as_str) == Some("approval_result") {
                                if let Some(w) = approvals.lock().unwrap().remove(&(machine, session)) {
                                    let _ = w.send(ev.payload.clone());
                                }
                            }
                        }
                        match msg {
                            Message::InvokeCommand(inv) => {
                                if let Some((name, out)) = &channel {
                                    if &inv.name == name {
                                        let content = inv.args.get("content").and_then(Value::as_str).unwrap_or_default();
                                        let meta = inv.args.get("meta").cloned().unwrap_or(json!({}));
                                        let _ = out.send(json!({
                                            "jsonrpc": "2.0",
                                            "method": "notifications/claude/channel",
                                            "params": { "content": content, "meta": meta },
                                        }));
                                    }
                                }
                            }
                            Message::Error(e) => bail!("daemon: {e}"),
                            _ => {}
                        }
                    }
                    out = rx.recv() => match out {
                        Some(o) => conn.send(&o).await?,
                        None => return Ok(()),
                    },
                }
            }
        };
        if let Err(e) = run.await {
            eprintln!("cq mcp: {e:#}");
        }
        // Reconnect; the hosts replay their history to the new connection.
        model.lock().unwrap().sessions.clear();
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Runs the MCP server on stdin/stdout. With `channel`, the server is also a Claude Code channel:
/// invoking the desktop command [`channel_command`] for this session pushes a message into it.
pub async fn run(scope: cq_client::Scope, channel: bool, worker: bool) -> Result<()> {
    let own = std::env::var("CQ_SESSION_ID").ok();
    if channel && own.is_none() {
        bail!("--channel needs to run inside a CodeQuilt session (CQ_SESSION_ID is not set)");
    }
    // One writer owns stdout so replies and notifications never interleave.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(v) = out_rx.recv().await {
            if stdout.write_all(format!("{v}\n").as_bytes()).await.is_err() || stdout.flush().await.is_err() {
                break;
            }
        }
    });

    let model = Arc::new(Mutex::new(Model::default()));
    let (tx, rx) = mpsc::unbounded_channel();
    let channel_cfg = if channel { own.as_deref().map(|id| (channel_command(id), out_tx.clone())) } else { None };
    let approvals: Approvals = Arc::default();
    tokio::spawn(daemon_loop(model.clone(), approvals.clone(), rx, channel_cfg));
    let server = Arc::new(Server { model, approvals, tx, scope, channel, worker, own });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let server = server.clone();
        let out = out_tx.clone();
        tokio::spawn(async move {
            let Some(id) = req.get("id").cloned() else { return }; // notification
            let reply = match handle(&server, &req).await {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32603, "message": format!("{e:#}") } }),
            };
            let _ = out.send(reply);
        });
    }
    Ok(())
}

async fn handle(server: &Server, req: &Value) -> Result<Value> {
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    match req.get("method").and_then(Value::as_str).unwrap_or_default() {
        "initialize" => {
            let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18");
            let mut capabilities = json!({ "tools": {} });
            if server.channel {
                capabilities["experimental"] = json!({ "claude/channel": {} });
            }
            Ok(json!({
                "protocolVersion": version,
                "capabilities": capabilities,
                "serverInfo": { "name": "codequilt", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Tools to see and drive the other coding agent sessions managed by CodeQuilt."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools(server.worker) })),
        "tools/call" => {
            // Give the first session lists a moment to arrive after startup.
            for _ in 0..30 {
                if server.model.lock().unwrap().synced {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let result = match name {
                other if server.worker && !WORKER_TOOLS.contains(&other) => Err(anyhow!("tool '{other}' is not available to worker sessions")),
                "list_sessions" => server.list_sessions(&args),
                "get_turns" => server.get_turns(&args),
                "send" => server.send(&args),
                "message" => server.message(&args),
                "approve" => server.approve(&args).await,
                "launch" => server.launch(&args).await,
                other => Err(anyhow!("unknown tool '{other}'")),
            };
            Ok(match result {
                Ok(v) => json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v)? }] }),
                Err(e) => json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true }),
            })
        }
        other => bail!("method not found: {other}"),
    }
}
