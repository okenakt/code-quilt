//! Account identification and token accounting for a session, published as host events.
//!
//! Both read Claude Code's private files (`.claude.json`, transcript jsonl), so all format
//! knowledge stays in this module and every read degrades to "unknown" instead of failing.

use serde::Serialize;
use std::collections::HashMap;
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

/// Token counts in one usage record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl Counts {
    fn add(&mut self, o: &Counts) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_creation += o.cache_creation;
    }
}

/// Aggregated usage of a transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TokenTotals {
    pub total: Counts,
    pub by_model: HashMap<String, Counts>,
}

/// Incrementally tails a transcript jsonl and aggregates usage per API message.
#[derive(Default)]
pub struct TranscriptTail {
    path: Option<PathBuf>,
    offset: u64,
    partial: Vec<u8>,
    /// Usage keyed by message id: one response is logged once per content block.
    messages: HashMap<String, (String, Counts)>,
}

impl TranscriptTail {
    /// Switches to a new transcript file, resetting state when the path changes.
    pub fn set_path(&mut self, path: PathBuf) {
        if self.path.as_ref() != Some(&path) {
            *self = Self { path: Some(path), ..Self::default() };
        }
    }

    /// Reads appended lines; returns new totals when anything changed.
    pub fn poll(&mut self) -> Option<TokenTotals> {
        let path = self.path.clone()?;
        let mut f = std::fs::File::open(&path).ok()?;
        let len = f.metadata().ok()?.len();
        if len < self.offset {
            // Truncated or replaced: start over.
            self.offset = 0;
            self.partial.clear();
            self.messages.clear();
        }
        if len == self.offset {
            return None;
        }
        f.seek(SeekFrom::Start(self.offset)).ok()?;
        let mut buf = Vec::with_capacity((len - self.offset) as usize);
        f.take(len - self.offset).read_to_end(&mut buf).ok()?;
        self.offset += buf.len() as u64;
        self.partial.extend_from_slice(&buf);
        let mut changed = false;
        while let Some(nl) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=nl).collect();
            changed |= self.ingest(&line);
        }
        changed.then(|| self.totals())
    }

    fn ingest(&mut self, line: &[u8]) -> bool {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else { return false };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            return false;
        }
        let Some(msg) = v.get("message") else { return false };
        let Some(usage) = msg.get("usage") else { return false };
        let n = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let counts = Counts {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_creation: n("cache_creation_input_tokens"),
        };
        let model = msg.get("model").and_then(|m| m.as_str()).unwrap_or("unknown").to_owned();
        if model == "<synthetic>" {
            return false;
        }
        let id = msg
            .get("id")
            .and_then(|i| i.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("line:{}", self.messages.len()));
        let prev = self.messages.insert(id, (model.clone(), counts));
        prev != Some((model, counts))
    }

    fn totals(&self) -> TokenTotals {
        let mut t = TokenTotals::default();
        for (model, c) in self.messages.values() {
            t.total.add(c);
            t.by_model.entry(model.clone()).or_default().add(c);
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupes_blocks_of_one_message() {
        let dir = std::env::temp_dir().join(format!("cq-usage-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.jsonl");
        let line = |id: &str, out: u64| {
            format!(
                r#"{{"type":"assistant","message":{{"id":"{id}","model":"m1","usage":{{"input_tokens":10,"output_tokens":{out},"cache_read_input_tokens":5,"cache_creation_input_tokens":1}}}}}}"#
            )
        };
        std::fs::write(&p, format!("{}\n{}\n{{\"type\":\"user\"}}\n", line("a", 3), line("a", 3))).unwrap();
        let mut t = TranscriptTail::default();
        t.set_path(p.clone());
        let totals = t.poll().unwrap();
        assert_eq!(totals.total, Counts { input: 10, output: 3, cache_read: 5, cache_creation: 1 });
        assert!(t.poll().is_none());
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        std::io::Write::write_all(&mut f, format!("{}\n", line("b", 7)).as_bytes()).unwrap();
        let totals = t.poll().unwrap();
        assert_eq!(totals.total.output, 10);
        assert_eq!(totals.by_model["m1"].input, 20);
        let _ = std::fs::remove_dir_all(dir);
    }
}
