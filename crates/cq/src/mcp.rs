//! `cq mcp`: an MCP server (stdio, JSON-RPC 2.0) that lets an agent see and drive other
//! sessions through the local daemon. Agent-agnostic: Claude Code and Codex both speak MCP.

use anyhow::{Result, anyhow, bail};
use cq_client::{Activity, MAX_TURNS, Model, Session};
use cq_proto::{ClientKind, DAEMON_PROTOCOL_VERSION, Hello, LOCAL_MACHINE, Message, spawn_reader, write_message};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// Which sessions this server may see and drive.
#[derive(Clone, Default)]
pub struct Scope {
    tags: Vec<String>,
    machines: Vec<String>,
}

impl Scope {
    /// Parses `tag:NAME` / `machine:ID` entries; empty means every session.
    pub fn parse(entries: &[String]) -> Result<Self> {
        let mut s = Self::default();
        for e in entries {
            match e.split_once(':') {
                Some(("tag", t)) => s.tags.push(t.to_owned()),
                Some(("machine", m)) => s.machines.push(m.to_owned()),
                _ => bail!("invalid scope '{e}' (use tag:NAME or machine:ID)"),
            }
        }
        Ok(s)
    }

    fn admits(&self, s: &Session) -> bool {
        let tag_ok = self.tags.is_empty() || s.meta.tags.iter().any(|t| self.tags.contains(t));
        let machine_ok = self.machines.is_empty() || self.machines.contains(&s.machine);
        tag_ok && machine_ok
    }
}

type Approvals = Arc<Mutex<HashMap<(String, String), oneshot::Sender<Value>>>>;

struct Server {
    model: Arc<Mutex<Model>>,
    approvals: Approvals,
    tx: mpsc::UnboundedSender<Message>,
    scope: Scope,
    /// Declare the Claude Code channel capability.
    channel: bool,
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
        self.scope.admits(s) && self.own.as_deref() != Some(&s.meta.id)
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
            "tags": s.meta.tags,
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

    async fn approve(&self, args: &Value) -> Result<Value> {
        let s = self.resolve(str_arg(args, "session")?)?;
        let option = args.get("option").and_then(Value::as_u64).ok_or_else(|| anyhow!("missing 'option'"))? as u8;
        let prompt_id = str_arg(args, "prompt_id")?.to_owned();
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().unwrap().insert((s.machine.clone(), s.meta.id.clone()), tx);
        let approve = cq_proto::Approve { option, prompt_id: Some(prompt_id) };
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

fn tools() -> Value {
    let session = json!({ "type": "string", "description": "Session name, machine:id, or id prefix" });
    json!([
        {
            "name": "list_sessions",
            "description": "List coding agent sessions in scope: machine, name, tags, directory, state (working / idle / approval / exited), any pending permission prompt, and the start of the last prompt and reply.",
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
            "name": "approve",
            "description": "Answer a session's permission prompt with one of its numbered options. Only on the user's explicit instruction. prompt_id must come from list_sessions or get_turns; the call fails if the prompt changed.",
            "inputSchema": { "type": "object", "properties": { "session": session, "prompt_id": { "type": "string" }, "option": { "type": "integer", "minimum": 1 } }, "required": ["session", "prompt_id", "option"] }
        }
    ])
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
            let stream = cq_client::connect_daemon().await?;
            let (rd, mut wr) = tokio::io::split(stream);
            let hello = Hello { protocol_version: DAEMON_PROTOCOL_VERSION, kind: ClientKind::Hub, cols: 0, rows: 0, federate: true };
            write_message(&mut wr, &Message::Hello(hello)).await?;
            write_message(&mut wr, &Message::ListSessions).await?;
            if let Some((name, _)) = &channel {
                let cmd = cq_proto::CommandInfo { name: name.clone(), title: "Deliver a channel message".into(), ..Default::default() };
                write_message(&mut wr, &Message::RegisterCommand(cmd)).await?;
            }
            let mut msgs = spawn_reader(rd);
            loop {
                tokio::select! {
                    msg = msgs.recv() => {
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
                        Some(o) => write_message(&mut wr, &o).await?,
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
pub async fn run(scope: Scope, channel: bool) -> Result<()> {
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
    let server = Arc::new(Server { model, approvals, tx, scope, channel, own });

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
        "tools/list" => Ok(json!({ "tools": tools() })),
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
                "list_sessions" => server.list_sessions(&args),
                "get_turns" => server.get_turns(&args),
                "send" => server.send(&args),
                "approve" => server.approve(&args).await,
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
