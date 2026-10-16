//! Session metadata persistence and discovery.

use crate::paths;
use anyhow::{Context, Result};
use cq_protocol::{SessionMeta, SessionStatus};
use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Writes metadata atomically (write to temp file, then rename).
pub fn save(meta: &SessionMeta) -> Result<()> {
    let path = paths::session_file(&meta.id)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
    std::fs::rename(&tmp, &path).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Reads a file the session `id` shared; any other path is refused.
pub fn read_shared(id: &str, path: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()), "invalid session id");
    let meta: SessionMeta =
        serde_json::from_slice(&std::fs::read(paths::session_file(id)?).context("unknown session")?)?;
    anyhow::ensure!(meta.files.iter().any(|f| f.path == path), "the session did not share {path}");
    let len = std::fs::metadata(path).with_context(|| format!("read {path}"))?.len();
    anyhow::ensure!(len <= cq_protocol::MAX_SHARED_FILE_LEN, "{path} is larger than 10 MB");
    std::fs::read(path).with_context(|| format!("read {path}"))
}

/// Lists all sessions, marking running entries whose host process is gone as exited.
pub fn list() -> Result<Vec<SessionMeta>> {
    let mut out = Vec::new();
    let dir = paths::sessions_dir()?;
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(out) };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(data) = std::fs::read(&p) else { continue };
        let Ok(mut meta) = serde_json::from_slice::<SessionMeta>(&data) else { continue };
        if meta.status == SessionStatus::Running && !pid_alive(meta.host_pid) {
            meta.status = SessionStatus::Exited;
            meta.ended_at_ms.get_or_insert_with(now_ms);
            let _ = save(&meta);
        }
        out.push(meta);
    }
    out.sort_by_key(|m| m.started_at_ms);
    Ok(out)
}

/// Picks the running session that `key` refers to: an exact name, then an exact id, then a
/// unique id prefix. Same order as the MCP server uses.
pub fn pick<'a>(sessions: &'a [SessionMeta], key: &str) -> Result<&'a SessionMeta> {
    let running = || sessions.iter().filter(|m| m.status == SessionStatus::Running);
    let rules: [&dyn Fn(&SessionMeta) -> bool; 3] =
        [&|m| m.name.as_deref() == Some(key), &|m| m.id == key, &|m| m.id.starts_with(key)];
    for rule in rules {
        let hits: Vec<_> = running().filter(|m| rule(m)).collect();
        match hits.as_slice() {
            [] => continue,
            [one] => return Ok(one),
            many => {
                let ids: Vec<_> = many.iter().map(|m| m.id.as_str()).collect();
                anyhow::bail!("'{key}' is ambiguous: {}", ids.join(", "))
            }
        }
    }
    if sessions.iter().any(|m| m.name.as_deref() == Some(key) || m.id.starts_with(key)) {
        anyhow::bail!("session '{key}' has exited");
    }
    anyhow::bail!("no running session matches '{key}'")
}

/// Picks the ended session that `key` refers to: an exact id or a unique id prefix.
pub fn pick_ended<'a>(sessions: &'a [SessionMeta], key: &str) -> Result<&'a SessionMeta> {
    let hits: Vec<_> = sessions.iter().filter(|m| m.id.starts_with(key)).collect();
    match hits.as_slice() {
        [] => anyhow::bail!("no session matches '{key}'"),
        [one] if one.status == SessionStatus::Running => anyhow::bail!("session {} is still running", one.id),
        [one] => Ok(one),
        many => {
            anyhow::bail!("'{key}' is ambiguous: {}", many.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(", "))
        }
    }
}

/// The id of another running session already called `name`, if any. Names are unique among a
/// machine's running sessions so that a name alone identifies a session.
pub fn name_taken(name: &str, except_id: &str) -> Option<String> {
    list()
        .ok()?
        .into_iter()
        .find(|m| m.status == SessionStatus::Running && m.id != except_id && m.name.as_deref() == Some(name))
        .map(|m| m.id)
}

/// Resolves a local running session by name, id, or unique id prefix.
pub fn resolve(key: &str) -> Result<SessionMeta> {
    let sessions = list()?;
    pick(&sessions, key).cloned()
}

/// Returns whether a process with the given pid is alive.
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        ok != 0 && code == STILL_ACTIVE as u32
    }
}

/// Returns whether a process with the given pid is alive.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str, name: Option<&str>, running: bool) -> SessionMeta {
        SessionMeta {
            id: id.into(),
            agent: "claude".into(),
            cwd: String::new(),
            pipe: String::new(),
            host_pid: 0,
            child_pid: None,
            started_at_ms: 0,
            status: if running { SessionStatus::Running } else { SessionStatus::Exited },
            exit_code: None,
            ended_at_ms: None,
            agent_session_id: None,
            name: name.map(Into::into),
            account: None,
            files: Vec::new(),
            orchestrator: false,
        }
    }

    #[test]
    fn pick_prefers_name_then_id_then_prefix() {
        let s = [meta("abc123", Some("ab"), true), meta("ab9999", None, true), meta("ff0000", Some("q"), true)];
        assert_eq!(pick(&s, "ab").unwrap().id, "abc123");
        assert_eq!(pick(&s, "ab9999").unwrap().id, "ab9999");
        assert_eq!(pick(&s, "q").unwrap().id, "ff0000");
        assert_eq!(pick(&s, "ff").unwrap().id, "ff0000");
        assert!(pick(&s, "a").unwrap_err().to_string().contains("ambiguous"));
    }

    #[test]
    fn pick_skips_exited_sessions() {
        let s = [meta("abc123", Some("x"), false), meta("def456", Some("x"), true)];
        assert_eq!(pick(&s, "x").unwrap().id, "def456");
        assert!(pick(&s, "abc").unwrap_err().to_string().contains("exited"));
        assert_eq!(pick_ended(&s, "abc").unwrap().id, "abc123");
        assert!(pick_ended(&s, "def").unwrap_err().to_string().contains("still running"));
    }
}
