//! Wire protocol shared by `cq` subcommands (host, attach, hook, statusline) and the UI backend.
//!
//! Every message is a frame: `[u32 LE length][u8 kind][payload]`, where `length` covers the kind
//! byte and the payload. Control messages carry JSON payloads; terminal output and input carry raw
//! bytes so escape sequences pass through untouched.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Protocol version exchanged during the handshake.
pub const PROTOCOL_VERSION: u32 = 1;

/// Upper bound for a single frame, to reject corrupted or hostile length prefixes.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Kind of peer connecting to a session host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// Interactive terminal client (`cq claude`, `cq attach`).
    Terminal,
    /// Dashboard client.
    Hub,
    /// One-shot event sender (`cq hook`, `cq statusline`).
    Event,
}

/// First message sent by a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub kind: ClientKind,
    pub cols: u16,
    pub rows: u16,
}

/// Static metadata describing a session, also persisted as `sessions/<id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub agent: String,
    pub cwd: String,
    pub pipe: String,
    pub host_pid: u32,
    pub child_pid: Option<u32>,
    pub started_at_ms: u64,
    pub status: SessionStatus,
    pub exit_code: Option<u32>,
    /// Agent-side session id (Claude Code `session_id`), learned from the SessionStart hook.
    pub agent_session_id: Option<String>,
}

/// Lifecycle status of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Exited,
}

/// Host reply to [`Hello`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol_version: u32,
    pub client_id: u64,
    pub session: SessionMeta,
}

/// Terminal size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

/// Notification of which client currently holds the input right.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputOwner {
    pub owner: Option<u64>,
}

/// Agent event forwarded from hooks or the status line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    /// `hook` or `statusline`.
    pub source: String,
    pub received_at_ms: u64,
    /// Raw JSON received on the hook's stdin.
    pub payload: serde_json::Value,
}

/// Process exit notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exit {
    pub code: Option<u32>,
}

/// A decoded protocol message.
#[derive(Debug, Clone)]
pub enum Message {
    Hello(Hello),
    Welcome(Welcome),
    /// Escape sequences that reproduce the current screen and terminal modes.
    Snapshot(Vec<u8>),
    Output(Vec<u8>),
    Input(Vec<u8>),
    Resize(Size),
    InputOwner(InputOwner),
    Event(AgentEvent),
    Exit(Exit),
    Error(String),
}

mod kind {
    pub const HELLO: u8 = 0x01;
    pub const WELCOME: u8 = 0x02;
    pub const SNAPSHOT: u8 = 0x03;
    pub const OUTPUT: u8 = 0x10;
    pub const INPUT: u8 = 0x11;
    pub const RESIZE: u8 = 0x12;
    pub const INPUT_OWNER: u8 = 0x13;
    pub const EVENT: u8 = 0x20;
    pub const EXIT: u8 = 0x21;
    pub const ERROR: u8 = 0x7f;
}

impl Message {
    /// Encodes the message into a complete frame.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let (k, payload): (u8, Vec<u8>) = match self {
            Message::Hello(v) => (kind::HELLO, serde_json::to_vec(v)?),
            Message::Welcome(v) => (kind::WELCOME, serde_json::to_vec(v)?),
            Message::Snapshot(b) => (kind::SNAPSHOT, b.clone()),
            Message::Output(b) => (kind::OUTPUT, b.clone()),
            Message::Input(b) => (kind::INPUT, b.clone()),
            Message::Resize(v) => (kind::RESIZE, serde_json::to_vec(v)?),
            Message::InputOwner(v) => (kind::INPUT_OWNER, serde_json::to_vec(v)?),
            Message::Event(v) => (kind::EVENT, serde_json::to_vec(v)?),
            Message::Exit(v) => (kind::EXIT, serde_json::to_vec(v)?),
            Message::Error(s) => (kind::ERROR, s.as_bytes().to_vec()),
        };
        let len = u32::try_from(payload.len() + 1).context("frame too large")?;
        let mut frame = Vec::with_capacity(payload.len() + 5);
        frame.extend_from_slice(&len.to_le_bytes());
        frame.push(k);
        frame.extend_from_slice(&payload);
        Ok(frame)
    }

    /// Decodes a message from its kind byte and payload.
    pub fn decode(k: u8, payload: Vec<u8>) -> Result<Self> {
        Ok(match k {
            kind::HELLO => Message::Hello(serde_json::from_slice(&payload)?),
            kind::WELCOME => Message::Welcome(serde_json::from_slice(&payload)?),
            kind::SNAPSHOT => Message::Snapshot(payload),
            kind::OUTPUT => Message::Output(payload),
            kind::INPUT => Message::Input(payload),
            kind::RESIZE => Message::Resize(serde_json::from_slice(&payload)?),
            kind::INPUT_OWNER => Message::InputOwner(serde_json::from_slice(&payload)?),
            kind::EVENT => Message::Event(serde_json::from_slice(&payload)?),
            kind::EXIT => Message::Exit(serde_json::from_slice(&payload)?),
            kind::ERROR => Message::Error(String::from_utf8_lossy(&payload).into_owned()),
            other => bail!("unknown message kind 0x{other:02x}"),
        })
    }
}

/// Reads one message. Returns `Ok(None)` on a clean EOF at a frame boundary.
pub async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Message>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > MAX_FRAME_LEN {
        bail!("invalid frame length {len}");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    let k = buf[0];
    buf.remove(0);
    Message::decode(k, buf).map(Some)
}

/// Writes one message and flushes.
pub async fn write_message<W: AsyncWrite + Unpin>(w: &mut W, msg: &Message) -> Result<()> {
    w.write_all(&msg.encode()?).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip() {
        let msgs = vec![
            Message::Output(b"\x1b[31mhi\x1b[0m".to_vec()),
            Message::Resize(Size { cols: 80, rows: 24 }),
            Message::Exit(Exit { code: Some(0) }),
        ];
        let mut buf = Vec::new();
        for m in &msgs {
            write_message(&mut buf, m).await.unwrap();
        }
        let mut cur = std::io::Cursor::new(buf);
        assert!(matches!(read_message(&mut cur).await.unwrap(), Some(Message::Output(b)) if b == b"\x1b[31mhi\x1b[0m"));
        assert!(matches!(read_message(&mut cur).await.unwrap(), Some(Message::Resize(Size { cols: 80, rows: 24 }))));
        assert!(matches!(read_message(&mut cur).await.unwrap(), Some(Message::Exit(Exit { code: Some(0) }))));
        assert!(read_message(&mut cur).await.unwrap().is_none());
    }
}
