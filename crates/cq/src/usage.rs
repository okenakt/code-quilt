//! What the agents' own files tell about the user: the account, Codex's status, and the
//! directories the user trusted.
//!
//! These are private formats (`.claude.json`, transcripts, Codex's `auth.json` and `config.toml`),
//! so all format knowledge stays in this module and every read degrades to "unknown" instead of
//! failing.

use serde::Serialize;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Account the agent is logged in with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Account {
    pub id: String,
    pub label: String,
}

/// Claude Code's config directory: `CLAUDE_CONFIG_DIR`, else the home directory for
/// `.claude.json` (which lives at `~/.claude.json`, not inside `~/.claude`).
fn claude_json_path() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir).join(".claude.json")),
        None => dirs::home_dir().map(|h| h.join(".claude.json")),
    }
}

/// Reads the logged-in Claude account from `.claude.json`, falling back to `claude auth status`.
pub fn read_claude_account() -> Option<Account> {
    claude_json_path().and_then(|p| account_from_claude_json(&p)).or_else(account_from_cli)
}

fn account_from_claude_json(path: &Path) -> Option<Account> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let acct = v.get("oauthAccount")?;
    let id = acct.get("accountUuid")?.as_str()?.to_owned();
    let label = acct
        .get("emailAddress")
        .and_then(|e| e.as_str())
        .unwrap_or(&id)
        .to_owned();
    Some(Account { id, label })
}

fn account_from_cli() -> Option<Account> {
    let out = std::process::Command::new("claude")
        .args(["auth", "status"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let email = text
        .split(|c: char| c.is_whitespace() || c == '"' || c == ',')
        .find(|w| w.contains('@') && w.contains('.'))?
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_owned();
    Some(Account { id: format!("email:{email}"), label: email })
}

/// Modification time of the account file, used to detect `/login` during a session.
pub fn account_stamp() -> Option<std::time::SystemTime> {
    std::fs::metadata(claude_json_path()?).and_then(|m| m.modified()).ok()
}

/// Codex's home directory: `CODEX_HOME`, else `~/.codex`.
fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
}

/// Decodes base64url without padding (JWT segments).
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// Reads the logged-in ChatGPT account of Codex from the id token's claims in `auth.json`.
/// Only the account id and email claims are read; tokens are never kept or passed on.
pub fn read_codex_account() -> Option<Account> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(codex_home()?.join("auth.json")).ok()?).ok()?;
    let jwt = v.pointer("/tokens/id_token")?.as_str()?;
    let claims: serde_json::Value = serde_json::from_slice(&base64url_decode(jwt.split('.').nth(1)?)?).ok()?;
    let email = claims.get("email").and_then(|e| e.as_str()).map(str::to_owned);
    let id = claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .and_then(|e| e.as_str())
        .map(str::to_owned)
        .or_else(|| email.clone())?;
    Some(Account { label: email.unwrap_or_else(|| id.clone()), id })
}

/// Modification time of Codex's auth file, used to detect re-login.
pub fn codex_account_stamp() -> Option<std::time::SystemTime> {
    std::fs::metadata(codex_home()?.join("auth.json")).and_then(|m| m.modified()).ok()
}

/// Incrementally tails a Codex rollout file (`~/.codex/sessions/.../rollout-*.jsonl`).
#[derive(Default)]
pub struct CodexTail {
    path: Option<PathBuf>,
    offset: u64,
    partial: Vec<u8>,
    model: Option<String>,
}

impl CodexTail {
    /// Switches to a new rollout file.
    pub fn set_path(&mut self, path: PathBuf) {
        if self.path.as_ref() != Some(&path) {
            *self = Self { path: Some(path), ..Self::default() };
        }
    }

    /// Reads appended lines; returns the latest status, in the shape of Claude Code's status line
    /// payload (model, context, rate limits) so clients read both agents the same way.
    pub fn poll(&mut self) -> Option<serde_json::Value> {
        let path = self.path.clone()?;
        let mut f = std::fs::File::open(&path).ok()?;
        let len = f.metadata().ok()?.len();
        if len < self.offset {
            *self = Self { path: self.path.take(), ..Self::default() };
        }
        if len == self.offset {
            return None;
        }
        f.seek(SeekFrom::Start(self.offset)).ok()?;
        let mut buf = Vec::new();
        f.take(len - self.offset).read_to_end(&mut buf).ok()?;
        self.offset += buf.len() as u64;
        self.partial.extend_from_slice(&buf);
        let mut status = None;
        while let Some(nl) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=nl).collect();
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) else { continue };
            let payload = &v["payload"];
            if v["type"] == "turn_context" {
                if let Some(m) = payload.get("model").and_then(|m| m.as_str()) {
                    self.model = Some(m.to_owned());
                }
                continue;
            }
            if v["type"] != "event_msg" || payload["type"] != "token_count" {
                continue;
            }
            let n = |p: &str| payload.pointer(p).and_then(|x| x.as_u64()).unwrap_or(0);
            let model = self.model.clone().unwrap_or_else(|| "codex".into());
            let window = |w: &str| {
                let used = payload.pointer(&format!("/rate_limits/{w}/used_percent"))?.as_f64()?;
                Some(serde_json::json!({ "used_percentage": used, "resets_at": payload.pointer(&format!("/rate_limits/{w}/resets_at")) }))
            };
            let ctx = n("/info/model_context_window");
            let used_pct = (ctx > 0).then(|| n("/info/last_token_usage/total_tokens") as f64 * 100.0 / ctx as f64);
            status = Some(serde_json::json!({
                "model": { "id": model, "display_name": model },
                "context_window": { "used_percentage": used_pct, "context_window_size": ctx },
                "rate_limits": { "five_hour": window("primary"), "seven_day": window("secondary") },
            }));
        }
        status
    }
}

/// Directories the user trusted in Claude Code (`projects.<dir>.hasTrustDialogAccepted`) or in
/// Codex (`[projects."<dir>"] trust_level = "trusted"`).
pub fn trusted_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let claude = claude_json_path().and_then(|p| std::fs::read(p).ok());
    if let Some(v) = claude.and_then(|d| serde_json::from_slice::<serde_json::Value>(&d).ok()) {
        if let Some(projects) = v.get("projects").and_then(|p| p.as_object()) {
            let trusted = projects.iter().filter(|(_, p)| p.get("hasTrustDialogAccepted").and_then(|t| t.as_bool()) == Some(true));
            out.extend(trusted.map(|(dir, _)| PathBuf::from(dir)));
        }
    }
    let codex = codex_home().and_then(|h| std::fs::read_to_string(h.join("config.toml")).ok());
    if let Some(t) = codex.and_then(|s| s.parse::<toml::Table>().ok()) {
        if let Some(projects) = t.get("projects").and_then(|p| p.as_table()) {
            let trusted = projects.iter().filter(|(_, p)| p.get("trust_level").and_then(|l| l.as_str()) == Some("trusted"));
            out.extend(trusted.map(|(dir, _)| PathBuf::from(dir)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_codex_rollout_status() {
        let dir = std::env::temp_dir().join(format!("cq-codex-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rollout.jsonl");
        let lines = [
            r#"{"type":"turn_context","payload":{"model":"gpt-6"}}"#,
            r#"{"timestamp":"2026-10-05T01:59:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":600,"cache_write_input_tokens":50,"output_tokens":40},"last_token_usage":{"total_tokens":2000},"model_context_window":10000},"rate_limits":{"primary":{"used_percent":12.5,"window_minutes":300,"resets_at":1790000000},"secondary":{"used_percent":40.0,"window_minutes":10080,"resets_at":1790500000}}}}"#,
        ];
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        let mut t = CodexTail::default();
        t.set_path(p);
        let st = t.poll().unwrap();
        assert_eq!(st.pointer("/model/id").unwrap(), "gpt-6");
        assert_eq!(st.pointer("/rate_limits/five_hour/used_percentage").unwrap(), 12.5);
        assert_eq!(st.pointer("/rate_limits/seven_day/resets_at").unwrap(), 1790500000);
        assert_eq!(st.pointer("/context_window/used_percentage").unwrap(), 20.0);
        assert!(t.poll().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn decodes_base64url() {
        assert_eq!(base64url_decode("eyJhIjoxfQ").unwrap(), br#"{"a":1}"#);
    }

}
