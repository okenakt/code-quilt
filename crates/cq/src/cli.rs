//! CLI operations on sessions of any machine, performed through the local daemon.

use anyhow::{Context, Result, bail};
use cq_protocol::{
    Message, SessionStatus,
};
use std::time::Duration;

#[derive(Default)]
struct UsageSession {
    machine: String,
    meta: Option<cq_protocol::SessionMeta>,
    status: Option<(u64, serde_json::Value)>,
    account: Option<serde_json::Value>,
}

fn fmt_until(resets_at: Option<u64>, now_s: u64) -> String {
    let Some(t) = resets_at else { return "-".into() };
    let m = t.saturating_sub(now_s) / 60;
    if m >= 1440 {
        format!("{}d{}h", m / 1440, (m % 1440) / 60)
    } else if m >= 60 {
        format!("{}h{}m", m / 60, m % 60)
    } else {
        format!("{m}m")
    }
}

/// `cq usage`: rate limits per account and the running sessions, collected through the daemon.
pub async fn usage(local_only: bool, as_json: bool) -> Result<()> {
    use serde_json::{Value, json};
    use std::collections::{BTreeMap, HashMap};

    let mut conn = cq_client::DaemonClient::open(!local_only).await?;
    conn.send(&Message::ListSessions).await?;

    let mut sessions: HashMap<(String, String), UsageSession> = HashMap::new();
    let mut machines: Vec<cq_protocol::MachineInfo> = Vec::new();
    let mut listed: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    // Hosts replay their latest status on connect; stop once every reachable machine has
    // reported and nothing new arrived for a moment.
    loop {
        // The daemon lists itself (and, when federated, every remote machine) first.
        let all_listed = !machines.is_empty()
            && machines.iter().all(|m| listed.contains(&m.id) || (!m.online && m.error.is_some()));
        let wait = if all_listed { Duration::from_millis(700) } else { Duration::from_millis(1500) };
        let next = match tokio::time::timeout_at(deadline.min(tokio::time::Instant::now() + wait), conn.msgs.recv()).await {
            Ok(Some(m)) => m?,
            Ok(None) => break,
            Err(_) if all_listed || tokio::time::Instant::now() >= deadline => break,
            Err(_) => continue,
        };
        match next {
            Message::MachineList(l) => machines = l,
            Message::SessionList(l) => {
                if !listed.contains(&l.machine) {
                    listed.push(l.machine.clone());
                }
                for meta in l.sessions {
                    let e = sessions.entry((l.machine.clone(), meta.id.clone())).or_default();
                    e.machine = l.machine.clone();
                    e.meta = Some(meta);
                }
            }
            Message::Routed { machine, session, inner } => {
                let Message::Event(ev) = *inner else { continue };
                let e = sessions.entry((machine.clone(), session)).or_default();
                e.machine = machine;
                match (ev.source.as_str(), ev.payload.get("type").and_then(Value::as_str)) {
                    ("statusline", _) => e.status = Some((ev.received_at_ms, ev.payload)),
                    ("cq", Some("account")) => e.account = ev.payload.get("account").cloned(),
                    _ => {}
                }
            }
            Message::Error(e) => bail!("daemon: {e}"),
            _ => {}
        }
    }

    for s in sessions.values_mut() {
        s.machine = cq_client::machine_name(&machines, &s.machine).to_owned();
    }

    // Latest rate limit reading per account.
    let mut accounts: BTreeMap<String, (String, u64, Value, Vec<String>)> = BTreeMap::new();
    for s in sessions.values() {
        let Some((at, status)) = &s.status else { continue };
        let Some(limits) = status.get("rate_limits") else { continue };
        let id = s.account.as_ref().and_then(|a| a.get("id")).and_then(Value::as_str).unwrap_or("unknown").to_owned();
        let label = s.account.as_ref().and_then(|a| a.get("label")).and_then(Value::as_str).unwrap_or("unknown account").to_owned();
        let entry = accounts.entry(format!("{}:{id}", s.meta.as_ref().map_or("claude", |m| &m.agent))).or_insert((label, 0, Value::Null, vec![]));
        if !entry.3.contains(&s.machine) {
            entry.3.push(s.machine.clone());
        }
        if *at > entry.1 {
            entry.1 = *at;
            entry.2 = limits.clone();
        }
    }
    let mut running: Vec<&UsageSession> = sessions
        .values()
        .filter(|s| s.meta.as_ref().is_some_and(|m| m.status == SessionStatus::Running))
        .collect();
    running.sort_by_key(|s| (s.machine.clone(), s.meta.as_ref().map_or(0, |m| m.started_at_ms)));

    if as_json {
        let accounts: Vec<_> = accounts
            .iter()
            .map(|(k, (label, at, limits, ms))| json!({ "account": k, "label": label, "updated_ms": at, "rate_limits": limits, "machines": ms }))
            .collect();
        let sessions: Vec<_> = running
            .iter()
            .map(|s| {
                let m = s.meta.as_ref().expect("filtered");
                let st = s.status.as_ref().map(|(_, v)| v);
                json!({
                    "session": format!("{}:{}", s.machine, m.id),
                    "name": m.name,
                    "model": st.and_then(|v| v.pointer("/model/display_name")),
                    "context_percent": st.and_then(|v| v.pointer("/context_window/used_percentage")),
                    "cost_usd": st.and_then(|v| v.pointer("/cost/total_cost_usd")),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json!({ "accounts": accounts, "sessions": sessions }))?);
        return Ok(());
    }

    let now_ms = crate::session::now_ms();
    let now_s = now_ms / 1000;
    println!("{:<32}  {:>5}  {:>7}  {:>5}  {:>7}  {:>9}  MACHINES", "ACCOUNT", "5H", "RESETS", "WEEK", "RESETS", "UPDATED");
    if accounts.is_empty() {
        println!("(no rate limit data yet; it arrives with the next status line update)");
    }
    for (label, at, limits, ms) in accounts.values() {
        let pct = |p: &str| limits.pointer(p).and_then(Value::as_f64).map_or("?".to_owned(), |v| format!("{v:.0}%"));
        let reset = |p: &str| fmt_until(limits.pointer(p).and_then(Value::as_u64), now_s);
        let ago = format!("{}m ago", now_ms.saturating_sub(*at) / 60_000);
        println!(
            "{:<32}  {:>5}  {:>7}  {:>5}  {:>7}  {:>9}  {}",
            label,
            pct("/five_hour/used_percentage"),
            reset("/five_hour/resets_at"),
            pct("/seven_day/used_percentage"),
            reset("/seven_day/resets_at"),
            ago,
            ms.join(", ")
        );
    }
    println!();
    println!("{:<24}  {:<16}  {:<12}  {:>4}", "SESSION", "NAME", "MODEL", "CTX");
    for s in running {
        let m = s.meta.as_ref().expect("filtered");
        let st = s.status.as_ref().map(|(_, v)| v);
        println!(
            "{:<24}  {:<16}  {:<12}  {:>4}",
            format!("{}:{}", s.machine, m.id),
            m.name.as_deref().unwrap_or("-"),
            st.and_then(|v| v.pointer("/model/display_name")).and_then(Value::as_str).unwrap_or("-"),
            st.and_then(|v| v.pointer("/context_window/used_percentage")).and_then(Value::as_f64).map_or("-".into(), |v| format!("{v:.0}%")),
        );
    }
    for m in machines.iter().filter(|m| !m.online) {
        println!("({} offline: {})", m.name, m.error.as_deref().unwrap_or("not connected"));
    }
    Ok(())
}

/// Splits `machine:session` into the machine's name, when given, and the session.
pub fn parse_target(target: &str) -> (Option<String>, String) {
    match target.split_once(':') {
        Some((m, id)) if !m.is_empty() && !m.contains(['\\', '/']) => (Some(m.to_owned()), id.to_owned()),
        _ => (None, target.to_owned()),
    }
}

/// Waits for the session to exit instead of a confirmation event.
pub const EXITED: &str = "<exit>";

/// Finds the running session `target` names, on its machine or else on any machine. Returns the
/// machine id and the session id.
async fn find_session(conn: &mut cq_client::DaemonClient, target: &str) -> Result<(String, String)> {
    let (name, key) = parse_target(target);
    let machine = name.as_deref().map(crate::machine::id_of).transpose()?;
    conn.send(&Message::ListSessions).await?;
    let mut machines: Vec<cq_protocol::MachineInfo> = Vec::new();
    let mut lists: Vec<(String, Vec<cq_protocol::SessionMeta>)> = Vec::new();
    let wanted = |m: &cq_protocol::MachineInfo| machine.as_ref().is_none_or(|id| *id == m.id);
    loop {
        let settled = !machines.is_empty()
            && machines.iter().filter(|m| wanted(m)).all(|m| lists.iter().any(|(id, _)| *id == m.id) || (!m.online && m.error.is_some()));
        if settled {
            break;
        }
        match conn.msgs.recv().await.context("daemon connection closed")?? {
            Message::MachineList(l) => machines = l,
            Message::SessionList(l) => lists.push((l.machine, l.sessions)),
            Message::Error(e) => bail!("daemon: {e}"),
            _ => {}
        }
    }
    if let (Some(name), Some(m)) = (&name, machines.iter().find(|m| wanted(m) && !m.online)) {
        bail!("machine {name} is offline: {}", m.error.as_deref().unwrap_or_default());
    }
    let candidates: Vec<_> = lists.iter().filter(|(id, _)| machines.iter().any(|m| m.id == *id && wanted(m))).flat_map(|(_, s)| s.clone()).collect();
    let id = crate::session::pick(&candidates, &key)?.id.clone();
    let machine = lists.iter().find(|(_, s)| s.iter().any(|m| m.id == id)).map(|(m, _)| m.clone()).expect("picked from these lists");
    Ok((machine, id))
}

/// Sends a control message to a running session and waits until the host confirms it with a
/// `cq` event of type `confirm`, or until the session exits when `confirm` is [`EXITED`].
pub async fn session_op(target: &str, inner: Message, confirm: &str) -> Result<String> {
    let limit = Duration::from_secs(20)
        + match &inner {
            Message::Terminate(t) => Duration::from_millis(t.grace_ms),
            _ => Duration::ZERO,
        };
    // Other machines are reached only through a federated connection.
    let here = crate::machine::local()?.name;
    let only_here = parse_target(target).0.is_some_and(|n| n == here);
    let mut conn = cq_client::DaemonClient::open(!only_here).await?;

    let run = async {
        let (machine, session) = find_session(&mut conn, target).await?;
        let sent_ms = crate::session::now_ms();
        conn.send(&Message::Routed { machine: machine.clone(), session: session.clone(), inner: Box::new(inner) }).await?;
        while let Some(msg) = conn.msgs.recv().await {
            match msg? {
                Message::Routed { machine: m, session: s, inner } if m == machine && s == session => {
                    if confirm == EXITED && matches!(*inner, Message::Exit(_)) {
                        return Ok(s);
                    }
                    if let Message::Event(ev) = *inner {
                        let ty = ev.payload.get("type").and_then(|t| t.as_str());
                        // Replayed history may hold older confirmations; allow small clock skew.
                        let fresh = ev.received_at_ms + 2000 >= sent_ms;
                        if ev.source == "cq" && ty == Some(confirm) && fresh {
                            return Ok(s);
                        }
                        if ev.source == "cq" && ty == Some("rejected") && fresh {
                            bail!("{}", ev.payload.get("reason").and_then(|r| r.as_str()).unwrap_or("rejected"));
                        }
                    }
                }
                Message::Error(e) => bail!("daemon: {e}"),
                _ => {}
            }
        }
        bail!("daemon connection closed")
    };
    tokio::time::timeout(limit, run).await.context("timed out waiting for the session")?
}
/// `cq run NAME --arg k=v…`: invokes a command through the daemon and prints its outcome.
pub async fn run_command(name: &str, args: &[String]) -> Result<i32> {
    let mut map = serde_json::Map::new();
    for a in args {
        let (k, v) = a.split_once('=').with_context(|| format!("expected NAME=VALUE, got '{a}'"))?;
        map.insert(k.to_owned(), serde_json::Value::String(v.to_owned()));
    }
    let mut conn = cq_client::DaemonClient::open(false).await?;
    let inv = cq_protocol::InvokeCommand {
        name: name.to_owned(),
        args: serde_json::Value::Object(map),
        id: Some(1),
        context: None,
    };
    conn.send(&Message::InvokeCommand(inv)).await?;
    let wait = async {
        while let Some(msg) = conn.msgs.recv().await {
            match msg? {
                Message::CommandResult(r) if r.id == 1 => return Ok(r),
                Message::Error(e) => bail!("daemon: {e}"),
                _ => {}
            }
        }
        bail!("daemon connection closed")
    };
    // Exec commands may run for up to two minutes in the daemon.
    let r = tokio::time::timeout(Duration::from_secs(130), wait).await.context("no result from the daemon")??;
    if !r.message.is_empty() {
        if r.ok { println!("{}", r.message) } else { eprintln!("{}", r.message) }
    }
    Ok(if r.ok { 0 } else { 1 })
}

/// Waits until every machine is reachable or failed, and returns the latest list.
async fn settled_machines(conn: &mut cq_client::DaemonClient, limit: Duration) -> Vec<cq_protocol::MachineInfo> {
    let deadline = tokio::time::Instant::now() + limit;
    let mut machines = Vec::new();
    while let Ok(Some(Ok(msg))) = tokio::time::timeout_at(deadline, conn.msgs.recv()).await {
        if let Message::MachineList(list) = msg {
            machines = list;
            if machines.iter().all(|m| m.online || m.error.is_some()) {
                break;
            }
        }
    }
    machines
}

/// `cq daemon status`: the local daemon and every registered machine.
pub async fn daemon_status() -> Result<()> {
    if !cq_client::daemon_running().await? {
        println!("{:<12} not running", crate::machine::local()?.name);
        return Ok(());
    }
    let mut conn = cq_client::DaemonClient::open(true).await?;
    for m in settled_machines(&mut conn, Duration::from_secs(10)).await {
        let state = if m.online { "running".to_owned() } else { m.error.unwrap_or_else(|| "connecting".into()) };
        println!("{:<12} {state}", m.name);
    }
    Ok(())
}

/// `cq daemon stop|restart`: the local daemon. Sessions keep running either way.
pub async fn daemon_stop(restart: bool) -> Result<()> {
    if cq_client::stop_daemon().await? {
        println!("stopped the local daemon");
    } else {
        println!("the local daemon is not running");
    }
    if restart {
        drop(cq_client::connect_daemon().await?);
        println!("started the local daemon");
    }
    Ok(())
}

/// The escape sequences that redraw a local session's current screen, read like a dashboard
/// does (without taking the input or changing the size).
pub async fn screen_snapshot(target: &str) -> Result<Vec<u8>> {
    let id = crate::session::pick(&crate::session::list()?, target)?.id.clone();
    let stream = crate::ipc::connect(&crate::paths::session_endpoint(&id)?, Duration::from_secs(2)).await?;
    let (mut rd, mut wr) = tokio::io::split(stream);
    let hello = cq_protocol::Hello::new(cq_protocol::PROTOCOL_VERSION, cq_protocol::ClientKind::Hub);
    cq_protocol::write_message(&mut wr, &Message::Hello(hello)).await?;
    while let Some(msg) = cq_protocol::read_message(&mut rd).await? {
        if let Message::Snapshot(bytes) = msg {
            return Ok(bytes);
        }
    }
    bail!("session {id} closed before sending its screen")
}
