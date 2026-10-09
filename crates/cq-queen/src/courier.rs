//! The courier (`cq-queen run`): watches the sessions in the Queen's scope, gathers what needs
//! the user's attention, and delivers it to the Queen session when the Queen is idle.

use crate::config::{Delivery, QueenConfig};
use anyhow::{Result, bail};
use cq_client::{Activity, Model, Session};
use cq_proto::{
    CommandInfo, InvokeCommand, LOCAL_MACHINE, Message,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Something that needs the user's attention.
#[derive(Debug, Clone)]
struct Item {
    /// `machine:id` of the session.
    key: String,
    label: String,
    kind: ItemKind,
    at_ms: u64,
}

#[derive(Debug, Clone)]
enum ItemKind {
    /// A permission prompt or confirmation dialog.
    Prompt { prompt: Value },
    /// The session finished a turn.
    Done { reply: String },
}

impl Item {
    fn same_slot(&self, other: &Item) -> bool {
        self.key == other.key && std::mem::discriminant(&self.kind) == std::mem::discriminant(&other.kind)
    }

    fn headline(&self) -> String {
        let what = match &self.kind {
            ItemKind::Prompt { prompt } if prompt.get("kind").and_then(Value::as_str) == Some("dialog") => "needs confirmation",
            ItemKind::Prompt { .. } => "needs permission",
            ItemKind::Done { .. } => "finished its turn",
        };
        format!("{} ({}) {what}", self.label, self.key)
    }
}

fn clip(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push_str(" …[truncated; use get_turns for the full text]");
    }
    out
}

/// Formats a batch of new items and the ones still open from earlier deliveries.
fn compose(new: &[Item], open: &[Item]) -> String {
    let mut out = format!("[CodeQuilt] {} update{}:\n", new.len(), if new.len() == 1 { "" } else { "s" });
    for (i, it) in new.iter().enumerate() {
        out.push_str(&format!("\n{}. {}\n", i + 1, it.headline()));
        match &it.kind {
            ItemKind::Prompt { prompt } => {
                if let Some(id) = prompt.get("id").and_then(Value::as_str) {
                    out.push_str(&format!("   prompt_id: {id}\n"));
                }
                if let Some(text) = prompt.get("text").and_then(Value::as_str) {
                    for line in clip(text, 1200).lines() {
                        out.push_str(&format!("   | {line}\n"));
                    }
                }
                let options: Vec<String> = prompt
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|o| {
                        o.iter()
                            .map(|o| format!("{}. {}", o.get("number").and_then(Value::as_u64).unwrap_or(0), o.get("label").and_then(Value::as_str).unwrap_or("")))
                            .collect()
                    })
                    .unwrap_or_default();
                out.push_str(&format!("   options: {}\n", options.join(" / ")));
            }
            ItemKind::Done { reply } => {
                for line in clip(reply, 1500).lines() {
                    out.push_str(&format!("   > {line}\n"));
                }
            }
        }
    }
    if !open.is_empty() {
        let now = now_ms();
        out.push_str("\nStill waiting for the user from earlier:\n");
        for it in open {
            out.push_str(&format!("- {} ({}m ago)\n", it.headline(), now.saturating_sub(it.at_ms) / 60_000));
        }
    }
    out
}

/// The Queen's own session: the local running session named after this Queen (names are unique).
fn find_queen<'a>(model: &'a Model, name: &str) -> Option<&'a Session> {
    model.sessions.values().find(|s| {
        s.machine == LOCAL_MACHINE && s.activity != Activity::Exited && s.meta.name.as_deref() == Some(name)
    })
}

/// Runs the courier until the Queen session exits.
pub async fn run(name: &str, cfg: QueenConfig) -> Result<()> {
    let scope = cq_client::Scope::parse(&cfg.scope)?;
    let mut started = now_ms();
    let mut conn = cq_client::DaemonClient::open(true).await?;
    conn.send(&Message::ListSessions).await?;

    let me = format!("queen:{name}");
    let mut model = Model::default();
    let mut commands: Vec<String> = Vec::new();
    let mut registered = false;
    // Whether the local session list arrived on this connection; until then a missing Queen
    // only means the list is not there yet.
    let mut listed = false;
    let mut queen_id: Option<String> = None;
    let mut pending: Vec<Item> = Vec::new();
    let mut open: Vec<Item> = Vec::new();
    let mut first_pending: Option<Instant> = None;
    let mut inject_id = 0u64;
    let wake = |w: &str| cfg.wake_on.iter().any(|x| x == w);
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let give_up = Instant::now() + Duration::from_secs(300);

    loop {
        tokio::select! {
            msg = conn.msgs.recv() => {
                let Some(Ok(msg)) = msg else {
                    // The daemon went away (e.g. restarted): connect again and rebuild the view;
                    // undelivered items are kept.
                    conn.reconnect().await;
                    let _ = conn.send(&Message::ListSessions).await;
                    (model, registered, listed, started) = (Model::default(), false, false, now_ms());
                    continue;
                };
                if matches!(&msg, Message::SessionList(l) if l.machine == LOCAL_MACHINE) {
                    listed = true;
                }
                if let Message::CommandList(list) = &msg {
                    commands = list.iter().map(|c| c.name.clone()).collect();
                    if !registered {
                        if commands.contains(&me) {
                            eprintln!("another courier is already serving queen '{name}'");
                            return Ok(());
                        }
                        let cmd = CommandInfo { name: me.clone(), title: format!("Courier of queen {name}"), ..Default::default() };
                        registered = conn.send(&Message::RegisterCommand(cmd)).await.is_ok();
                    }
                }
                let Some((machine, session, ev)) = model.apply(&msg) else { continue };
                // History replayed on connect predates this courier; only live events count.
                if ev.received_at_ms + 1000 < started {
                    continue;
                }
                let Some(s) = model.sessions.get(&(machine.clone(), session.clone())) else { continue };
                // Queens (this one and any other with a running courier) are not reported, or two
                // Queens would keep answering each other's updates.
                let is_queen = machine == LOCAL_MACHINE
                    && (Some(&session) == queen_id.as_ref() || s.meta.name.as_ref().is_some_and(|n| commands.contains(&format!("queen:{n}"))));
                if is_queen || !scope.admits(&s.machine) {
                    continue;
                }
                let item = |kind| Item { key: s.key(), label: s.label().to_owned(), kind, at_ms: ev.received_at_ms };
                let key = s.key();
                let hook = ev.payload.get("hook_event_name").and_then(Value::as_str);
                let cq_type = if ev.source == "cq" { ev.payload.get("type").and_then(Value::as_str) } else { None };
                let new = match (hook, cq_type) {
                    (_, Some("permission_prompt")) => {
                        let prompt = ev.payload.get("prompt").cloned().unwrap_or(Value::Null);
                        let is_dialog = prompt.get("kind").and_then(Value::as_str) == Some("dialog");
                        wake(if is_dialog { "question" } else { "permission" }).then(|| item(ItemKind::Prompt { prompt }))
                    }
                    (_, Some("permission_resolved")) => {
                        pending.retain(|i| !(i.key == key && matches!(i.kind, ItemKind::Prompt { .. })));
                        open.retain(|i| !(i.key == key && matches!(i.kind, ItemKind::Prompt { .. })));
                        None
                    }
                    (Some("Stop"), _) => {
                        let reply = ev.payload.get("last_assistant_message").and_then(Value::as_str).unwrap_or_default();
                        (wake("stop") && !reply.trim().is_empty()).then(|| item(ItemKind::Done { reply: reply.to_owned() }))
                    }
                    (Some("UserPromptSubmit"), _) => {
                        // The session got its answer (from the user directly or via the Queen).
                        pending.retain(|i| i.key != key);
                        open.retain(|i| i.key != key);
                        None
                    }
                    _ => None,
                };
                if let Some(it) = new {
                    pending.retain(|p| !p.same_slot(&it));
                    open.retain(|p| !p.same_slot(&it));
                    pending.push(it);
                    first_pending.get_or_insert_with(Instant::now);
                }
            }
            _ = tick.tick() => {
                // Follow the Queen session; stop when it is gone.
                if !listed {
                    continue;
                }
                match find_queen(&model, name).map(|q| q.meta.id.clone()) {
                    Some(id) => queen_id = Some(id),
                    None if queen_id.is_some() => return Ok(()),
                    None if Instant::now() > give_up => bail!("no running session named '{name}'"),
                    None => continue,
                }
                let Some(qid) = queen_id.clone() else { continue };
                let Some(queen) = model.sessions.get(&(LOCAL_MACHINE.to_owned(), qid.clone())) else { continue };
                let batch_ready = first_pending.is_some_and(|t| t.elapsed() >= Duration::from_secs_f64(cfg.batch_seconds));
                if pending.is_empty() || !batch_ready || queen.activity != Activity::Idle {
                    continue;
                }
                let channel = format!("channel:{qid}");
                let content = compose(&pending, &open);
                let msg = match cfg.delivery() {
                    Delivery::Channels => {
                        // The channel exists once the Queen's MCP server is up (after the user
                        // confirms the development channel dialog).
                        if !commands.contains(&channel) {
                            continue;
                        }
                        Message::InvokeCommand(InvokeCommand {
                            name: channel,
                            args: json!({ "content": content, "meta": { "kind": "update", "items": pending.len().to_string() } }),
                            id: None,
                            context: None,
                        })
                    }
                    Delivery::Pty => {
                        inject_id += 1;
                        let inj = cq_proto::Inject { id: inject_id, text: content, submit: true, quiet_ms: 3000 };
                        Message::Routed { machine: LOCAL_MACHINE.to_owned(), session: qid, inner: Box::new(Message::Inject(inj)) }
                    }
                };
                if conn.send(&msg).await.is_err() {
                    continue; // The receive side notices the lost connection and reconnects.
                }
                open.extend(pending.drain(..).filter(|i| matches!(i.kind, ItemKind::Prompt { .. })));
                first_pending = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_prompts_and_replies() {
        let p = Item {
            key: "devbox:ab12".into(),
            label: "api".into(),
            kind: ItemKind::Prompt {
                prompt: json!({ "id": "9e02", "kind": "permission", "text": "Bash command\n  rm -rf build", "options": [{ "number": 1, "label": "Yes" }, { "number": 2, "label": "No" }] }),
            },
            at_ms: now_ms(),
        };
        let d = Item { key: "local:cd34".into(), label: "web".into(), kind: ItemKind::Done { reply: "All tests pass.".into() }, at_ms: now_ms() };
        let text = compose(&[p.clone(), d], &[]);
        assert!(text.starts_with("[CodeQuilt] 2 updates:"));
        assert!(text.contains("api (devbox:ab12) needs permission"));
        assert!(text.contains("prompt_id: 9e02"));
        assert!(text.contains("options: 1. Yes / 2. No"));
        assert!(text.contains("> All tests pass."));
        let text = compose(&[], &[p]);
        assert!(text.contains("Still waiting for the user from earlier:"));
    }
}
