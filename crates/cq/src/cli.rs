//! CLI operations on sessions of any machine, performed through the local daemon.

use crate::daemon;
use anyhow::{Context, Result, bail};
use cq_proto::{
    ClientKind, DAEMON_PROTOCOL_VERSION, Hello, LOCAL_MACHINE, Message, SessionStatus, spawn_reader,
    write_message,
};
use std::time::Duration;

/// Splits `machine:id` (or a bare local id) into its parts.
pub fn parse_target(target: &str) -> (String, String) {
    match target.split_once(':') {
        Some((m, id)) if !m.is_empty() && !m.contains(['\\', '/']) => (m.to_owned(), id.to_owned()),
        _ => (LOCAL_MACHINE.to_owned(), target.to_owned()),
    }
}

/// Sends a control message to a running session and waits until the host confirms it with a
/// `cq` event of type `confirm`.
pub async fn session_op(target: &str, inner: Message, confirm: &str) -> Result<String> {
    let (machine, prefix) = parse_target(target);
    let stream = daemon::connect().await?;
    let (rd, mut wr) = tokio::io::split(stream);
    let mut msgs = spawn_reader(rd);
    let hello = Hello {
        protocol_version: DAEMON_PROTOCOL_VERSION,
        kind: ClientKind::Hub,
        cols: 0,
        rows: 0,
        federate: machine != LOCAL_MACHINE,
    };
    write_message(&mut wr, &Message::Hello(hello)).await?;
    write_message(&mut wr, &Message::ListSessions).await?;

    let run = async {
        let mut session: Option<String> = None;
        let mut inner = Some(inner);
        let mut sent_ms = 0u64;
        while let Some(msg) = msgs.recv().await {
            match msg? {
                Message::SessionList(l) if l.machine == machine && session.is_none() => {
                    let hits: Vec<_> = l
                        .sessions
                        .iter()
                        .filter(|s| s.status == SessionStatus::Running && s.id.starts_with(&prefix))
                        .collect();
                    let id = match hits.as_slice() {
                        [one] => one.id.clone(),
                        [] => bail!("no running session matches '{target}'"),
                        _ => bail!("'{target}' is ambiguous ({} sessions)", hits.len()),
                    };
                    let msg = Message::Routed {
                        machine: machine.clone(),
                        session: id.clone(),
                        inner: Box::new(inner.take().expect("sent once")),
                    };
                    sent_ms = crate::session::now_ms();
                    write_message(&mut wr, &msg).await?;
                    session = Some(id);
                }
                Message::MachineList(list) => {
                    if let Some(m) = list.iter().find(|m| m.id == machine && !m.online && m.error.is_some()) {
                        bail!("machine {} is offline: {}", m.id, m.error.as_deref().unwrap_or_default());
                    }
                    if list.iter().all(|m| m.id != machine) {
                        bail!("unknown machine '{machine}'");
                    }
                }
                Message::Routed { machine: m, session: s, inner } if m == machine && Some(&s) == session.as_ref() => {
                    if let Message::Event(ev) = *inner {
                        let ty = ev.payload.get("type").and_then(|t| t.as_str());
                        // Replayed history may hold older confirmations; allow small clock skew.
                        let fresh = ev.received_at_ms + 2000 >= sent_ms;
                        if ev.source == "cq" && ty == Some(confirm) && fresh {
                            return Ok(s);
                        }
                    }
                }
                Message::Error(e) => bail!("daemon: {e}"),
                _ => {}
            }
        }
        bail!("daemon connection closed")
    };
    tokio::time::timeout(Duration::from_secs(20), run).await.context("timed out waiting for the session")?
}
