//! `cq-queen hook`: the Queen session's UserPromptSubmit hook. A prompt of the form `@name text`
//! is delivered straight to that session and blocked, so addressing a session costs no LLM turn.
//! Anything else (or an unknown name) passes through to the Queen.

use anyhow::Result;
use cq_client::{Activity, Model, Session};
use cq_proto::{
    AgentEvent, ClientKind, Hello, Message, PROTOCOL_VERSION,
    write_message,
};
use serde_json::{Value, json};
use std::io::Read;
use std::time::Duration;

/// Splits `@target text` into its parts.
pub fn parse_mention(prompt: &str) -> Option<(&str, &str)> {
    let rest = prompt.trim_start().strip_prefix('@')?;
    let end = rest.find(char::is_whitespace)?;
    let (target, body) = rest.split_at(end);
    let body = body.trim();
    (!target.is_empty() && !body.is_empty()).then_some((target, body))
}

/// Running sessions other than `own` matching `target` (`name`, `machine:name`, or id prefix).
fn matches<'a>(model: &'a Model, target: &str, own: Option<&str>) -> Vec<&'a Session> {
    let (machine, rest) = match target.split_once(':') {
        Some((m, r)) if model.machines.iter().any(|x| x.id == m) => (Some(m), r),
        _ => (None, target),
    };
    let live: Vec<&Session> = model
        .sessions
        .values()
        .filter(|s| s.activity != Activity::Exited && Some(s.meta.id.as_str()) != own && machine.is_none_or(|m| s.machine == m))
        .collect();
    let by_name: Vec<_> = live.iter().copied().filter(|s| s.meta.name.as_deref() == Some(rest)).collect();
    if !by_name.is_empty() {
        return by_name;
    }
    live.into_iter().filter(|s| s.meta.id.starts_with(rest)).collect()
}

fn block(reason: &str) {
    println!("{}", json!({ "decision": "block", "reason": reason }));
}

/// Tells this session's host that the prompt was blocked, so it counts as idle again.
async fn report_blocked(prompt: &str) -> Result<()> {
    let Ok(id) = std::env::var("CQ_SESSION_ID") else { return Ok(()) };
    let mut s = cq_proto::local::connect(&cq_proto::local::session_endpoint(&id)?, Duration::from_secs(1)).await?;
    let hello = Hello { protocol_version: PROTOCOL_VERSION, kind: ClientKind::Event, cols: 0, rows: 0, federate: false };
    write_message(&mut s, &Message::Hello(hello)).await?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let ev = AgentEvent { source: "cq".into(), received_at_ms: now, payload: json!({ "type": "prompt_blocked", "prompt": prompt }) };
    write_message(&mut s, &Message::Event(ev)).await?;
    Ok(())
}

/// Loads every machine's session list, waiting for reachable remote machines up to a limit.
async fn load_model() -> Result<(Model, cq_client::DaemonClient)> {
    let mut conn = cq_client::DaemonClient::open(true).await?;
    conn.send(&Message::ListSessions).await?;
    let mut model = Model::default();
    let mut listed: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let done = !model.machines.is_empty()
            && model.machines.iter().all(|m| listed.contains(&m.id) || (!m.online && m.error.is_some()));
        if done {
            break;
        }
        match tokio::time::timeout_at(deadline, conn.msgs.recv()).await {
            Ok(Some(Ok(msg))) => {
                if let Message::SessionList(l) = &msg {
                    listed.push(l.machine.clone());
                }
                model.apply(&msg);
            }
            _ => break,
        }
    }
    Ok((model, conn))
}

/// Runs the hook: reads the hook payload from stdin and prints a block decision when it sent
/// the prompt directly.
pub async fn run() -> Result<()> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let payload: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let prompt = payload.get("prompt").and_then(Value::as_str).unwrap_or_default();
    let Some((target, body)) = parse_mention(prompt) else { return Ok(()) };
    let own = std::env::var("CQ_SESSION_ID").ok();
    let (model, mut conn) = load_model().await?;
    let hits = matches(&model, target, own.as_deref());
    let s = match hits.as_slice() {
        [] => return Ok(()), // Not a session name: let the Queen read it.
        [one] => *one,
        many => {
            let names: Vec<String> = many.iter().map(|s| format!("{} ({})", s.label(), s.key())).collect();
            block(&format!("@{target} matches several sessions: {}. Use @machine:name.", names.join(", ")));
            report_blocked(prompt).await.ok();
            return Ok(());
        }
    };
    let mut bytes = format!("\x1b[200~{body}\x1b[201~").into_bytes();
    bytes.push(b'\r');
    let msg = Message::Routed { machine: s.machine.clone(), session: s.meta.id.clone(), inner: Box::new(Message::Input(bytes)) };
    conn.send(&msg).await?;
    // Let CodeQuilt's own UserPromptSubmit hook (running in parallel) report first, so the
    // "blocked" notice is the last word on this prompt.
    tokio::time::sleep(Duration::from_millis(600)).await;
    report_blocked(prompt).await.ok();
    block(&format!("Sent directly to @{} ({}). No reply from the Queen for this message.", s.label(), s.key()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mentions() {
        assert_eq!(parse_mention("@api run the tests"), Some(("api", "run the tests")));
        assert_eq!(parse_mention("  @devbox:web fix\nthe build"), Some(("devbox:web", "fix\nthe build")));
        assert_eq!(parse_mention("@api"), None);
        assert_eq!(parse_mention("hello @api"), None);
    }
}
