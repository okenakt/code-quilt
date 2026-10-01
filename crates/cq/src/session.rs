//! Session metadata persistence and discovery.

use crate::paths;
use anyhow::{Context, Result};
use cq_proto::{SessionMeta, SessionStatus};
use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Generates a short random session id.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_owned()
}

/// Writes metadata atomically (write to temp file, then rename).
pub fn save(meta: &SessionMeta) -> Result<()> {
    let path = paths::session_file(&meta.id)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
    std::fs::rename(&tmp, &path).with_context(|| format!("write {}", path.display()))?;
    Ok(())
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
            let _ = save(&meta);
        }
        out.push(meta);
    }
    out.sort_by_key(|m| m.started_at_ms);
    Ok(out)
}

/// Resolves a session by full id or unique prefix.
pub fn resolve(prefix: &str) -> Result<SessionMeta> {
    let matches: Vec<_> = list()?.into_iter().filter(|m| m.id.starts_with(prefix)).collect();
    match matches.len() {
        0 => anyhow::bail!("no session matches '{prefix}'"),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => anyhow::bail!("'{prefix}' is ambiguous ({n} sessions)"),
    }
}

/// Returns whether a process with the given pid is alive.
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
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
