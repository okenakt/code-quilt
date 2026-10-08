//! CLI operations on sessions of any machine, performed through the local daemon.

use anyhow::{Context, Result, bail};
use cq_proto::{
    LOCAL_MACHINE, Message, SessionStatus,
};
use std::time::Duration;

#[derive(Default)]
struct UsageSession {
    machine: String,
    meta: Option<cq_proto::SessionMeta>,
    status: Option<(u64, serde_json::Value)>,
    account: Option<serde_json::Value>,
    tokens: Option<serde_json::Value>,
}

fn fmt_k(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
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

/// `cq usage`: rate limits per account and tokens per session, collected through the daemon.
pub async fn usage(local_only: bool, as_json: bool) -> Result<()> {
    use serde_json::{Value, json};
    use std::collections::{BTreeMap, HashMap};

    let mut conn = cq_client::DaemonClient::open(!local_only).await?;
    conn.send(&Message::ListSessions).await?;

    let mut sessions: HashMap<(String, String), UsageSession> = HashMap::new();
    let mut machines: Vec<cq_proto::MachineInfo> = Vec::new();
    let mut listed: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    // Hosts replay their latest status on connect; stop once every reachable machine has
    // reported and nothing new arrived for a moment.
    loop {
        let all_listed = if local_only {
            listed.iter().any(|m| m == LOCAL_MACHINE)
        } else {
            !machines.is_empty()
                && machines.iter().all(|m| listed.contains(&m.id) || (!m.online && m.error.is_some()))
        };
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
                    ("cq", Some("tokens")) => e.tokens = ev.payload.get("tokens").cloned(),
                    _ => {}
                }
            }
            Message::Error(e) => bail!("daemon: {e}"),
            _ => {}
        }
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
                    "tokens": s.tokens,
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
    println!("{:<24}  {:<16}  {:<12}  {:>4}  {:>8}  {:>8}  {:>8}  {:>8}", "SESSION", "NAME", "MODEL", "CTX", "IN", "OUT", "CACHE R", "CACHE W");
    for s in running {
        let m = s.meta.as_ref().expect("filtered");
        let st = s.status.as_ref().map(|(_, v)| v);
        let tok = |k: &str| s.tokens.as_ref().and_then(|t| t.pointer(&format!("/total/{k}"))).and_then(Value::as_u64).map_or("-".into(), fmt_k);
        println!(
            "{:<24}  {:<16}  {:<12}  {:>4}  {:>8}  {:>8}  {:>8}  {:>8}",
            format!("{}:{}", s.machine, m.id),
            m.name.as_deref().unwrap_or("-"),
            st.and_then(|v| v.pointer("/model/display_name")).and_then(Value::as_str).unwrap_or("-"),
            st.and_then(|v| v.pointer("/context_window/used_percentage")).and_then(Value::as_f64).map_or("-".into(), |v| format!("{v:.0}%")),
            tok("input"),
            tok("output"),
            tok("cache_read"),
            tok("cache_creation"),
        );
    }
    for m in machines.iter().filter(|m| !m.online) {
        println!("({} offline: {})", m.label, m.error.as_deref().unwrap_or("not connected"));
    }
    Ok(())
}

/// Splits `machine:id` (or a bare local id) into its parts.
pub fn parse_target(target: &str) -> (String, String) {
    match target.split_once(':') {
        Some((m, id)) if !m.is_empty() && !m.contains(['\\', '/']) => (m.to_owned(), id.to_owned()),
        _ => (LOCAL_MACHINE.to_owned(), target.to_owned()),
    }
}

/// Waits for the session to exit instead of a confirmation event.
pub const EXITED: &str = "<exit>";

/// Sends a control message to a running session and waits until the host confirms it with a
/// `cq` event of type `confirm`, or until the session exits when `confirm` is [`EXITED`].
pub async fn session_op(target: &str, inner: Message, confirm: &str) -> Result<String> {
    let limit = Duration::from_secs(20)
        + match &inner {
            Message::Terminate(t) => Duration::from_millis(t.grace_ms),
            _ => Duration::ZERO,
        };
    let (machine, key) = parse_target(target);
    let mut conn = cq_client::DaemonClient::open(machine != LOCAL_MACHINE).await?;
    conn.send(&Message::ListSessions).await?;

    let run = async {
        let mut session: Option<String> = None;
        let mut inner = Some(inner);
        let mut sent_ms = 0u64;
        while let Some(msg) = conn.msgs.recv().await {
            match msg? {
                Message::SessionList(l) if l.machine == machine && session.is_none() => {
                    let id = crate::session::pick(&l.sessions, &key)?.id.clone();
                    let msg = Message::Routed {
                        machine: machine.clone(),
                        session: id.clone(),
                        inner: Box::new(inner.take().expect("sent once")),
                    };
                    sent_ms = crate::session::now_ms();
                    conn.send(&msg).await?;
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
    let inv = cq_proto::InvokeCommand {
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

async fn local_daemon_running() -> Result<bool> {
    Ok(cq_proto::local::connect(&cq_proto::local::daemon_endpoint()?, Duration::ZERO).await.is_ok())
}

/// Waits until every machine is reachable or failed, and returns the latest list.
async fn settled_machines(conn: &mut cq_client::DaemonClient, limit: Duration) -> Vec<cq_proto::MachineInfo> {
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
    if !local_daemon_running().await? {
        println!("{LOCAL_MACHINE:<12} not running");
        return Ok(());
    }
    let mut conn = cq_client::DaemonClient::open(true).await?;
    for m in settled_machines(&mut conn, Duration::from_secs(10)).await {
        let state = if m.online { "running".to_owned() } else { m.error.unwrap_or_else(|| "connecting".into()) };
        println!("{:<12} {state}", m.id);
    }
    Ok(())
}

/// `cq daemon stop|restart [--machine ID]`. Sessions keep running either way.
pub async fn daemon_stop(machine: &str, restart: bool) -> Result<()> {
    let shutdown = Message::Shutdown(cq_proto::Shutdown { machine: machine.to_owned() });
    if machine == LOCAL_MACHINE {
        if local_daemon_running().await? {
            cq_client::DaemonClient::open(false).await?.send(&shutdown).await?;
            for _ in 0..50 {
                if !local_daemon_running().await? {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            println!("stopped the local daemon");
        } else {
            println!("the local daemon is not running");
        }
        if restart {
            drop(cq_client::DaemonClient::open(false).await?);
            println!("started the local daemon");
        }
        return Ok(());
    }
    let mut conn = cq_client::DaemonClient::open(true).await?;
    let before = settled_machines(&mut conn, Duration::from_secs(15)).await;
    match before.iter().find(|m| m.id == machine) {
        None => bail!("unknown machine '{machine}'"),
        Some(m) if !m.online => bail!("machine {machine} is not reachable: {}", m.error.as_deref().unwrap_or("connecting")),
        _ => {}
    }
    conn.send(&shutdown).await?;
    println!("stopped the daemon on {machine}");
    if restart {
        // The relay reconnects through `cq bridge`, which starts a new daemon.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut conn = cq_client::DaemonClient::open(true).await?;
        let after = settled_machines(&mut conn, Duration::from_secs(15)).await;
        match after.iter().find(|m| m.id == machine) {
            Some(m) if m.online => println!("started the daemon on {machine}"),
            _ => bail!("the daemon on {machine} did not come back"),
        }
    }
    Ok(())
}
