//! Wire protocol shared by `cq` subcommands (host, attach, hook, statusline) and the UI backend.
//!
//! Every message is a frame: `[u32 LE length][u8 kind][payload]`, where `length` covers the kind
//! byte and the payload. Control messages carry JSON payloads; terminal output and input carry raw
//! bytes so escape sequences pass through untouched.

#[cfg(feature = "local")]
pub mod local;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Version of the host protocol (terminal, hub, monitor and event clients of `cq host`).
pub const PROTOCOL_VERSION: u32 = 1;

/// Version of the daemon protocol (machine-tagged relay through `cq daemon`). Versioned
/// separately so running hosts stay reachable when only the daemon protocol changes.
pub const DAEMON_PROTOCOL_VERSION: u32 = 5;

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
    /// Observer that receives events, input-owner changes and exit, but no terminal output.
    Monitor,
}

/// First message sent by a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub kind: ClientKind,
    pub cols: u16,
    pub rows: u16,
    /// Daemon clients only: also relay the registered remote machines.
    #[serde(default)]
    pub federate: bool,
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
    /// When the session ended (milliseconds since the epoch).
    #[serde(default)]
    pub ended_at_ms: Option<u64>,
    /// Agent-side session id (Claude Code `session_id`), learned from the SessionStart hook.
    pub agent_session_id: Option<String>,
    /// User-given name, unique among the machine's running sessions, used to address the session
    /// (e.g. `cq attach NAME`, `@name`).
    #[serde(default)]
    pub name: Option<String>,
    /// Account the agent was last seen logged in with (its label, e.g. an email address).
    #[serde(default)]
    pub account: Option<String>,
    /// Tokens used per UTC hour, keyed `YYYY-MM-DDTHH`, recounted from the agent's whole log (a
    /// resumed session keeps its id and counts its earlier turns again, not twice).
    #[serde(default)]
    pub tokens_by_hour: BTreeMap<String, TokenCounts>,
    /// Files the agent shared for the user to view, oldest first. Only these can be read back
    /// through [`ReadFile`].
    #[serde(default)]
    pub files: Vec<SharedFile>,
}

/// A file an agent shared with the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedFile {
    /// Absolute path on the session's machine.
    pub path: String,
    #[serde(default)]
    pub caption: Option<String>,
    pub shared_at_ms: u64,
}

/// Largest file that can be shared and read back.
pub const MAX_SHARED_FILE_LEN: u64 = 10 * 1024 * 1024;

/// Token counts of one or more API responses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl TokenCounts {
    /// Adds `o` to these counts.
    pub fn add(&mut self, o: &TokenCounts) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_creation += o.cache_creation;
    }
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

/// Answer to a permission prompt, by 1-based option number as shown on screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Approve {
    pub option: u8,
    /// Id of the prompt being answered; the host refuses the answer if a different prompt is
    /// on screen by now.
    #[serde(default)]
    pub prompt_id: Option<String>,
}

/// Text to type into a session once it is idle and nobody is typing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inject {
    /// Client-chosen id echoed in the `inject_result` event.
    pub id: u64,
    pub text: String,
    /// Press Enter after pasting.
    #[serde(default)]
    pub submit: bool,
    /// How long terminal input must have been quiet before injecting.
    #[serde(default = "default_quiet_ms")]
    pub quiet_ms: u64,
}

fn default_quiet_ms() -> u64 {
    3000
}

/// Request to end a session: ask the agent to exit, then kill it after `grace_ms`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Terminate {
    /// How long to wait for a graceful exit before killing the agent.
    #[serde(default = "default_grace_ms")]
    pub grace_ms: u64,
    /// Kill right away without asking the agent to exit.
    #[serde(default)]
    pub force: bool,
}

fn default_grace_ms() -> u64 {
    10_000
}

/// New name of a session; `None` removes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rename {
    #[serde(default)]
    pub name: Option<String>,
}

/// A command: declared in a plugin manifest, or registered at run time by a client that
/// handles it (e.g. `channel:<session>`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInfo {
    pub name: String,
    pub title: String,
    /// Shown in the command palette and bindable to keys. Commands only registered at run time
    /// (e.g. `channel:<session>`) are for programs and stay hidden.
    #[serde(default)]
    pub palette: bool,
    /// Plugin that declares the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// Arguments the palette asks for before invoking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<CommandArg>,
    /// Default key binding suggested by the plugin (e.g. `ctrl+shift+q`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keybinding: Option<String>,
}

/// An argument of a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandArg {
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// Invocation of a command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeCommand {
    pub name: String,
    #[serde(default)]
    pub args: serde_json::Value,
    /// Caller-chosen id echoed in the `CommandResult`; none means no result is wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// The session the user is working in when invoking (the desktop focus).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<SessionRef>,
}

/// Outcome of an invocation that carried an id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub id: u64,
    pub ok: bool,
    #[serde(default)]
    pub message: String,
}

/// Request to open a relayed connection to a session through the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscribe {
    pub machine: String,
    pub session: String,
    pub kind: ClientKind,
    pub cols: u16,
    pub rows: u16,
}

/// A session on a machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionRef {
    pub machine: String,
    pub session: String,
}

/// A machine known to the daemon and whether it is reachable. Each machine makes its own id once
/// and keeps it, so the id names the machine the same way everywhere; users see and address the
/// machine by its name. A daemon's first message is a list holding only itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInfo {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub error: Option<String>,
}

/// Sessions of one machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionList {
    pub machine: String,
    pub sessions: Vec<SessionMeta>,
}

/// Request to start a new agent session on a machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Launch {
    /// Client-chosen id echoed in the reply.
    pub id: u64,
    pub machine: String,
    pub agent: String,
    pub cwd: String,
    /// Extra agent arguments.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// Leave out CodeQuilt's worker MCP server, for a session that brings its own (an orchestrator).
    #[serde(default)]
    pub no_mcp: bool,
    pub cols: u16,
    pub rows: u16,
}

/// Request to start an ended session again under its id, resuming the agent's conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resume {
    /// Client-chosen id echoed in the reply.
    pub id: u64,
    pub machine: String,
    pub session: String,
    pub cols: u16,
    pub rows: u16,
}

/// Reply to [`Launch`] and [`Resume`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Launched {
    pub id: u64,
    pub machine: String,
    pub session: String,
}

/// Request to list subdirectories (an empty path lists the allowed roots).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListDir {
    pub id: u64,
    pub machine: String,
    pub path: String,
}

/// Request to read a file a session shared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadFile {
    pub id: u64,
    pub machine: String,
    pub session: String,
    pub path: String,
}

/// Failure reply to an id-carrying request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestFailed {
    pub id: u64,
    pub error: String,
}

/// Directory listing for choosing a launch directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirList {
    /// Id of the [`ListDir`] request.
    #[serde(default)]
    pub id: u64,
    /// Listed directory, or empty when listing the allowed roots.
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<String>,
    pub roots: Vec<String>,
    /// Directories the user trusted in an agent, offered as shortcuts with the roots.
    #[serde(default)]
    pub known: Vec<String>,
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
    Approve(Approve),
    Error(String),
    /// Client to daemon: request the session lists of all machines.
    ListSessions,
    /// Daemon to client: all known sessions of one machine.
    SessionList(SessionList),
    /// Daemon to client: machines and their reachability.
    MachineList(Vec<MachineInfo>),
    /// Client to daemon: open a relayed connection to a session.
    Subscribe(Subscribe),
    /// Client to daemon: close a relayed connection.
    Unsubscribe(SessionRef),
    /// A session message relayed by the daemon, tagged with machine and session.
    Routed { machine: String, session: String, inner: Box<Message> },
    /// Client to daemon: start a session.
    Launch(Launch),
    /// Client to daemon: start an ended session again.
    Resume(Resume),
    /// Daemon to client: a launched session.
    Launched(Launched),
    /// Client to daemon: list subdirectories.
    ListDir(ListDir),
    /// Daemon to client: directory listing.
    DirList(DirList),
    /// Client to host: add a file to the session's shared files.
    ShareFile(SharedFile),
    /// Client to daemon: read a shared file.
    ReadFile(ReadFile),
    /// Daemon to client: contents of the file of the [`ReadFile`] request `id`.
    FileContent { id: u64, data: Vec<u8> },
    /// Daemon to client: an id-carrying request failed.
    RequestFailed(RequestFailed),
    /// Client to daemon: stop the local daemon. Sessions keep running; the next connection starts
    /// a new daemon, so stopping is also how the daemon is restarted.
    Shutdown,
    /// Client to host: type text once the session is idle.
    Inject(Inject),
    /// Client to host: end the session gracefully.
    Terminate(Terminate),
    /// Client to host: change the name.
    Rename(Rename),
    /// Client to daemon: the session the user is typing into on this desktop.
    SetFocus(Option<SessionRef>),
    /// Daemon to client: current focus.
    Focus(Option<SessionRef>),
    /// Client to daemon: offer a command.
    RegisterCommand(CommandInfo),
    /// Daemon to client: all registered commands.
    CommandList(Vec<CommandInfo>),
    /// Client to daemon (and daemon to the registering client): run a command.
    InvokeCommand(InvokeCommand),
    /// Daemon to the invoking client: outcome of an invocation with an id.
    CommandResult(CommandResult),
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
    pub const APPROVE: u8 = 0x22;
    pub const LIST_SESSIONS: u8 = 0x30;
    pub const SESSION_LIST: u8 = 0x31;
    pub const SUBSCRIBE: u8 = 0x32;
    pub const UNSUBSCRIBE: u8 = 0x33;
    pub const ROUTED: u8 = 0x34;
    pub const LAUNCH: u8 = 0x35;
    pub const LAUNCHED: u8 = 0x36;
    pub const LIST_DIR: u8 = 0x37;
    pub const DIR_LIST: u8 = 0x38;
    pub const MACHINE_LIST: u8 = 0x39;
    pub const REQUEST_FAILED: u8 = 0x3a;
    pub const SHUTDOWN: u8 = 0x3b;
    pub const READ_FILE: u8 = 0x3c;
    pub const FILE_CONTENT: u8 = 0x3d;
    pub const SHARE_FILE: u8 = 0x26;
    pub const INJECT: u8 = 0x23;
    pub const RENAME: u8 = 0x24;
    pub const TERMINATE: u8 = 0x25;
    pub const SET_FOCUS: u8 = 0x40;
    pub const FOCUS: u8 = 0x41;
    pub const REGISTER_COMMAND: u8 = 0x42;
    pub const COMMAND_LIST: u8 = 0x43;
    pub const INVOKE_COMMAND: u8 = 0x44;
    pub const COMMAND_RESULT: u8 = 0x45;
    pub const RESUME: u8 = 0x46;
    pub const ERROR: u8 = 0x7f;
}

impl Message {
    /// Id of a request that expects a reply or [`Message::RequestFailed`].
    pub fn request_id(&self) -> Option<u64> {
        match self {
            Message::Launch(l) => Some(l.id),
            Message::Resume(r) => Some(r.id),
            Message::ListDir(l) => Some(l.id),
            Message::ReadFile(r) => Some(r.id),
            _ => None,
        }
    }

    /// Encodes the message into a complete frame.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let (k, payload) = self.encode_parts()?;
        let len = u32::try_from(payload.len() + 1).context("frame too large")?;
        let mut frame = Vec::with_capacity(payload.len() + 5);
        frame.extend_from_slice(&len.to_le_bytes());
        frame.push(k);
        frame.extend_from_slice(&payload);
        Ok(frame)
    }

    fn encode_parts(&self) -> Result<(u8, Vec<u8>)> {
        Ok(match self {
            Message::Hello(v) => (kind::HELLO, serde_json::to_vec(v)?),
            Message::Welcome(v) => (kind::WELCOME, serde_json::to_vec(v)?),
            Message::Snapshot(b) => (kind::SNAPSHOT, b.clone()),
            Message::Output(b) => (kind::OUTPUT, b.clone()),
            Message::Input(b) => (kind::INPUT, b.clone()),
            Message::Resize(v) => (kind::RESIZE, serde_json::to_vec(v)?),
            Message::InputOwner(v) => (kind::INPUT_OWNER, serde_json::to_vec(v)?),
            Message::Event(v) => (kind::EVENT, serde_json::to_vec(v)?),
            Message::Exit(v) => (kind::EXIT, serde_json::to_vec(v)?),
            Message::Approve(v) => (kind::APPROVE, serde_json::to_vec(v)?),
            Message::Error(s) => (kind::ERROR, s.as_bytes().to_vec()),
            Message::ListSessions => (kind::LIST_SESSIONS, Vec::new()),
            Message::SessionList(v) => (kind::SESSION_LIST, serde_json::to_vec(v)?),
            Message::Subscribe(v) => (kind::SUBSCRIBE, serde_json::to_vec(v)?),
            Message::Unsubscribe(v) => (kind::UNSUBSCRIBE, serde_json::to_vec(v)?),
            Message::Routed { machine, session, inner } => {
                // [u16 LE len][machine][u16 LE len][session][inner kind][inner payload]
                let (ik, ip) = inner.encode_parts()?;
                let mut b = Vec::with_capacity(machine.len() + session.len() + ip.len() + 5);
                for part in [machine.as_bytes(), session.as_bytes()] {
                    b.extend_from_slice(&u16::try_from(part.len())?.to_le_bytes());
                    b.extend_from_slice(part);
                }
                b.push(ik);
                b.extend_from_slice(&ip);
                (kind::ROUTED, b)
            }
            Message::MachineList(v) => (kind::MACHINE_LIST, serde_json::to_vec(v)?),
            Message::Launch(v) => (kind::LAUNCH, serde_json::to_vec(v)?),
            Message::Resume(v) => (kind::RESUME, serde_json::to_vec(v)?),
            Message::Launched(v) => (kind::LAUNCHED, serde_json::to_vec(v)?),
            Message::ListDir(v) => (kind::LIST_DIR, serde_json::to_vec(v)?),
            Message::DirList(v) => (kind::DIR_LIST, serde_json::to_vec(v)?),
            Message::RequestFailed(v) => (kind::REQUEST_FAILED, serde_json::to_vec(v)?),
            Message::ShareFile(v) => (kind::SHARE_FILE, serde_json::to_vec(v)?),
            Message::ReadFile(v) => (kind::READ_FILE, serde_json::to_vec(v)?),
            // [u64 LE id][raw bytes]
            Message::FileContent { id, data } => (kind::FILE_CONTENT, [&id.to_le_bytes()[..], data].concat()),
            Message::Shutdown => (kind::SHUTDOWN, Vec::new()),
            Message::Inject(v) => (kind::INJECT, serde_json::to_vec(v)?),
            Message::Rename(v) => (kind::RENAME, serde_json::to_vec(v)?),
            Message::Terminate(v) => (kind::TERMINATE, serde_json::to_vec(v)?),
            Message::SetFocus(v) => (kind::SET_FOCUS, serde_json::to_vec(v)?),
            Message::Focus(v) => (kind::FOCUS, serde_json::to_vec(v)?),
            Message::RegisterCommand(v) => (kind::REGISTER_COMMAND, serde_json::to_vec(v)?),
            Message::CommandList(v) => (kind::COMMAND_LIST, serde_json::to_vec(v)?),
            Message::InvokeCommand(v) => (kind::INVOKE_COMMAND, serde_json::to_vec(v)?),
            Message::CommandResult(v) => (kind::COMMAND_RESULT, serde_json::to_vec(v)?),
        })
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
            kind::APPROVE => Message::Approve(serde_json::from_slice(&payload)?),
            kind::ERROR => Message::Error(String::from_utf8_lossy(&payload).into_owned()),
            kind::LIST_SESSIONS => Message::ListSessions,
            kind::SESSION_LIST => Message::SessionList(serde_json::from_slice(&payload)?),
            kind::SUBSCRIBE => Message::Subscribe(serde_json::from_slice(&payload)?),
            kind::UNSUBSCRIBE => Message::Unsubscribe(serde_json::from_slice(&payload)?),
            kind::ROUTED => {
                let mut pos = 0usize;
                let mut take = || -> Result<String> {
                    let len_end = pos + 2;
                    if payload.len() < len_end {
                        bail!("short routed frame");
                    }
                    let n = u16::from_le_bytes([payload[pos], payload[pos + 1]]) as usize;
                    if payload.len() < len_end + n {
                        bail!("short routed frame");
                    }
                    let s = String::from_utf8(payload[len_end..len_end + n].to_vec())?;
                    pos = len_end + n;
                    Ok(s)
                };
                let machine = take()?;
                let session = take()?;
                let Some(&ik) = payload.get(pos) else { bail!("short routed frame") };
                let inner = Box::new(Message::decode(ik, payload[pos + 1..].to_vec())?);
                Message::Routed { machine, session, inner }
            }
            kind::MACHINE_LIST => Message::MachineList(serde_json::from_slice(&payload)?),
            kind::LAUNCH => Message::Launch(serde_json::from_slice(&payload)?),
            kind::RESUME => Message::Resume(serde_json::from_slice(&payload)?),
            kind::LAUNCHED => Message::Launched(serde_json::from_slice(&payload)?),
            kind::LIST_DIR => Message::ListDir(serde_json::from_slice(&payload)?),
            kind::DIR_LIST => Message::DirList(serde_json::from_slice(&payload)?),
            kind::REQUEST_FAILED => Message::RequestFailed(serde_json::from_slice(&payload)?),
            kind::SHARE_FILE => Message::ShareFile(serde_json::from_slice(&payload)?),
            kind::READ_FILE => Message::ReadFile(serde_json::from_slice(&payload)?),
            kind::FILE_CONTENT => {
                let Some(id) = payload.first_chunk::<8>() else { bail!("short file content frame") };
                Message::FileContent { id: u64::from_le_bytes(*id), data: payload[8..].to_vec() }
            }
            kind::SHUTDOWN => Message::Shutdown,
            kind::INJECT => Message::Inject(serde_json::from_slice(&payload)?),
            kind::RENAME => Message::Rename(serde_json::from_slice(&payload)?),
            kind::TERMINATE => Message::Terminate(serde_json::from_slice(&payload)?),
            kind::SET_FOCUS => Message::SetFocus(serde_json::from_slice(&payload)?),
            kind::FOCUS => Message::Focus(serde_json::from_slice(&payload)?),
            kind::REGISTER_COMMAND => Message::RegisterCommand(serde_json::from_slice(&payload)?),
            kind::COMMAND_LIST => Message::CommandList(serde_json::from_slice(&payload)?),
            kind::INVOKE_COMMAND => Message::InvokeCommand(serde_json::from_slice(&payload)?),
            kind::COMMAND_RESULT => Message::CommandResult(serde_json::from_slice(&payload)?),
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

/// Reads messages on a dedicated task and delivers them through a channel.
///
/// `read_message` is not cancel-safe: using it directly as a `select!` branch drops partially
/// read frames whenever another branch wins, which desynchronizes the stream. Select on the
/// returned channel instead. The channel closes on EOF; a read error is delivered as `Err`.
pub fn spawn_reader<R>(mut rd: R) -> tokio::sync::mpsc::UnboundedReceiver<Result<Message>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            match read_message(&mut rd).await {
                Ok(Some(m)) => {
                    if tx.send(Ok(m)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    break;
                }
            }
        }
    });
    rx
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
            Message::Routed {
                machine: "devbox".into(),
                session: "abc".into(),
                inner: Box::new(Message::Input(b"x".to_vec())),
            },
            Message::FileContent { id: 7, data: vec![0, 255, 1] },
        ];
        let mut buf = Vec::new();
        for m in &msgs {
            write_message(&mut buf, m).await.unwrap();
        }
        let mut cur = std::io::Cursor::new(buf);
        assert!(matches!(read_message(&mut cur).await.unwrap(), Some(Message::Output(b)) if b == b"\x1b[31mhi\x1b[0m"));
        assert!(matches!(read_message(&mut cur).await.unwrap(), Some(Message::Resize(Size { cols: 80, rows: 24 }))));
        assert!(matches!(read_message(&mut cur).await.unwrap(), Some(Message::Exit(Exit { code: Some(0) }))));
        match read_message(&mut cur).await.unwrap() {
            Some(Message::Routed { machine, session, inner }) => {
                assert_eq!(machine, "devbox");
                assert_eq!(session, "abc");
                assert!(matches!(*inner, Message::Input(b) if b == b"x"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(read_message(&mut cur).await.unwrap(), Some(Message::FileContent { id: 7, data }) if data == [0, 255, 1]));
        assert!(read_message(&mut cur).await.unwrap().is_none());
    }
}
