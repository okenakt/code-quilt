//! Queen: summarizes agent replies into cards and infers where a message should go, using the
//! local `claude -p` so everything stays within the user's subscription.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// Model used by Queen. A light model keeps the 5h budget impact small.
const QUEEN_MODEL: &str = "haiku";

/// Environment variables of an enclosing Claude Code session that must not leak into `claude -p`.
const NESTED_SESSION_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

/// Tokens and cost Queen itself has consumed since the app started.
#[derive(Debug, Clone, Default, Serialize)]
pub struct QueenUsage {
    pub calls: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub cost_usd: f64,
}

/// Queen state shared by commands.
#[derive(Default)]
pub struct Queen {
    usage: Mutex<QueenUsage>,
}

#[derive(Deserialize)]
struct PrintResult {
    #[serde(default)]
    result: String,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    total_cost_usd: f64,
    #[serde(default)]
    usage: serde_json::Value,
}

/// A routing candidate shown to the model.
#[derive(Debug, Deserialize)]
pub struct RouteCandidate {
    pub key: String,
    pub name: String,
    pub cwd: String,
    /// Recent card texts of the session, newest last.
    pub recent: Vec<String>,
}

/// Routing decision: a single target, or several plausible ones for the user to choose from.
#[derive(Debug, Serialize, Deserialize)]
pub struct RouteDecision {
    pub target: Option<String>,
    #[serde(default)]
    pub candidates: Vec<String>,
}

impl Queen {
    /// Returns Queen's own consumption.
    pub fn usage(&self) -> QueenUsage {
        self.usage.lock().unwrap().clone()
    }

    async fn ask(&self, system: &str, prompt: &str) -> Result<String> {
        let mut cmd = tokio::process::Command::new("claude");
        cmd.args(["-p", "--model", QUEEN_MODEL, "--output-format", "json", "--tools", ""])
            .args(["--no-session-persistence", "--system-prompt", system])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        if let Ok(root) = cq_proto::local::state_root() {
            // A neutral directory keeps project CLAUDE.md files out of Queen's context.
            let _ = std::fs::create_dir_all(&root);
            cmd.current_dir(root);
        }
        for v in NESTED_SESSION_VARS {
            cmd.env_remove(v);
        }
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().context("spawn claude -p")?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).await?;
        }
        let out = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output())
            .await
            .context("claude -p timed out")??;
        let r: PrintResult = serde_json::from_slice(&out.stdout).context("unexpected claude -p output")?;
        {
            let n = |k: &str| r.usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            let mut u = self.usage.lock().unwrap();
            u.calls += 1;
            u.input += n("input_tokens");
            u.output += n("output_tokens");
            u.cache_read += n("cache_read_input_tokens");
            u.cache_creation += n("cache_creation_input_tokens");
            u.cost_usd += r.total_cost_usd;
        }
        if r.is_error {
            bail!("claude -p failed: {}", r.result);
        }
        Ok(r.result.trim().to_owned())
    }

    /// Summarizes a long agent reply for a card.
    pub async fn summarize(&self, session: &str, text: &str) -> Result<String> {
        let system = "You condense a coding agent's reply into a dashboard card for a busy developer \
            who supervises many agents. Write 1-3 short sentences in the language of the reply: \
            what was done, and any question or decision the agent is waiting for. No preamble.";
        self.ask(system, &format!("Session: {session}\n\nReply:\n{text}")).await
    }

    /// Picks the session a free-form message is meant for.
    pub async fn route(&self, message: &str, candidates: &[RouteCandidate]) -> Result<RouteDecision> {
        let system = "You route a developer's message to one of several coding agent sessions. \
            Respond with JSON only: {\"target\": \"<key>\" | null, \"candidates\": [\"<key>\", ...]}. \
            Set target only when one session is clearly meant; otherwise list the plausible keys.";
        let mut prompt = String::from("Sessions:\n");
        for c in candidates {
            prompt.push_str(&format!("- key: {}\n  name: {}\n  cwd: {}\n", c.key, c.name, c.cwd));
            for r in c.recent.iter().rev().take(3) {
                prompt.push_str(&format!("  recent: {}\n", r.replace('\n', " ")));
            }
        }
        prompt.push_str(&format!("\nMessage:\n{message}\n"));
        let raw = self.ask(system, &prompt).await?;
        let json = raw
            .find('{')
            .and_then(|s| raw.rfind('}').map(|e| &raw[s..=e]))
            .context("no JSON in routing answer")?;
        let mut d: RouteDecision = serde_json::from_str(json).context("invalid routing answer")?;
        let known = |k: &String| candidates.iter().any(|c| &c.key == k);
        d.target = d.target.filter(known);
        d.candidates.retain(known);
        Ok(d)
    }
}
