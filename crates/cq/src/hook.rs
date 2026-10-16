//! Event senders invoked by the agent: `cq hook` and `cq statusline`.
//!
//! Both must stay invisible to the agent: hooks print nothing (SessionStart/UserPromptSubmit
//! stdout is injected into the context) and never wait for an absent host.

use crate::{ipc, paths, session};
use anyhow::Result;
use cq_protocol::{AgentEvent, ClientKind, Hello, Message, PROTOCOL_VERSION, write_message};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

fn read_stdin() -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut buf);
    buf
}

async fn send_event(ev: AgentEvent) -> Result<()> {
    let Ok(id) = std::env::var("CQ_SESSION_ID") else { return Ok(()) };
    let endpoint = paths::session_endpoint(&id)?;
    let mut s = ipc::connect_now(&endpoint).await?;
    let hello = Hello::new(PROTOCOL_VERSION, ClientKind::Event);
    write_message(&mut s, &Message::Hello(hello)).await?;
    write_message(&mut s, &Message::Event(ev)).await?;
    Ok(())
}

fn parse(raw: &[u8]) -> serde_json::Value {
    serde_json::from_slice(raw).unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(raw).into_owned()))
}

/// Handles a hook invocation: forwards the payload to the host if reachable. Always succeeds
/// silently.
pub async fn hook() {
    let raw = read_stdin();
    if std::env::var_os("CQ_SESSION_ID").is_none() {
        return;
    }
    let ev = AgentEvent { source: "hook".into(), received_at_ms: session::now_ms(), payload: parse(&raw) };
    let r = tokio::time::timeout(Duration::from_millis(500), send_event(ev)).await;
    if std::env::var_os("CQ_DEBUG").is_some() {
        eprintln!("cq hook: {r:?}");
    }
}

/// Handles a status line invocation: forwards the payload to the host, then chains to the user's own
/// status line command so the local display is unchanged.
pub async fn statusline() {
    let raw = read_stdin();
    if std::env::var_os("CQ_SESSION_ID").is_some() {
        let ev = AgentEvent { source: "statusline".into(), received_at_ms: session::now_ms(), payload: parse(&raw) };
        let _ = tokio::time::timeout(Duration::from_millis(300), send_event(ev)).await;
    }
    if let Some(cmd) = user_statusline_command() {
        if let Some(out) = run_shell(&cmd, &raw) {
            let _ = std::io::stdout().write_all(&out);
        }
    }
}

/// Reads `statusLine.command` from the user's Claude settings, if any.
fn user_statusline_command() -> Option<String> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))?;
    let data = std::fs::read(dir.join("settings.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&data).ok()?;
    let sl = v.get("statusLine")?;
    if sl.get("type").and_then(|t| t.as_str()).is_some_and(|t| t != "command") {
        return None;
    }
    sl.get("command")?.as_str().map(str::to_owned)
}

/// Runs a command through the same kind of shell Claude Code uses (bash, falling back to cmd on
/// Windows), feeding `input` on stdin.
fn run_shell(cmd: &str, input: &[u8]) -> Option<Vec<u8>> {
    let mut c = if cfg!(windows) {
        match std::env::var("CLAUDE_CODE_GIT_BASH_PATH").ok().or_else(find_bash) {
            Some(bash) => {
                let mut c = Command::new(bash);
                c.args(["-c", cmd]);
                c
            }
            None => {
                let mut c = Command::new("cmd");
                c.args(["/C", cmd]);
                c
            }
        }
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", cmd]);
        c
    };
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    Some(child.wait_with_output().ok()?.stdout)
}

fn find_bash() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|p| p.join("bash.exe"))
        .find(|p| p.is_file() && !p.to_string_lossy().to_lowercase().contains("system32"))
        .map(|p| p.to_string_lossy().into_owned())
}
