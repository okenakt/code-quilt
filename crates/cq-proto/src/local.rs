//! Local endpoint naming and client connections, shared by `cq` and the UI backend.

use anyhow::{Context, Result};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

/// A bidirectional local IPC stream.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Stream for T {}

/// Boxed local IPC stream.
pub type BoxStream = Box<dyn Stream>;

/// Root directory of CodeQuilt state (`~/.codequilt`).
pub fn state_root() -> Result<std::path::PathBuf> {
    Ok(dirs::home_dir().context("cannot determine home directory")?.join(".codequilt"))
}

fn user_tag() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".into())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// IPC endpoint of a session host.
pub fn session_endpoint(id: &str) -> Result<String> {
    if cfg!(windows) {
        Ok(format!(r"\\.\pipe\codequilt-{id}"))
    } else {
        Ok(state_root()?.join("run").join(format!("{id}.sock")).to_string_lossy().into_owned())
    }
}

/// IPC endpoint of the machine daemon. Pipe names are machine-global on Windows, so the user name
/// is part of the name.
pub fn daemon_endpoint() -> Result<String> {
    if cfg!(windows) {
        Ok(format!(r"\\.\pipe\codequilt-cqd-{}", user_tag()))
    } else {
        Ok(state_root()?.join("run").join("cqd.sock").to_string_lossy().into_owned())
    }
}

/// Connects to an endpoint, retrying until `timeout` while it is missing or busy.
#[cfg(windows)]
pub async fn connect(name: &str, timeout: Duration) -> Result<BoxStream> {
    use tokio::net::windows::named_pipe::ClientOptions;
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PIPE_BUSY: i32 = 231;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match ClientOptions::new().open(name) {
            Ok(c) => return Ok(Box::new(c)),
            Err(e)
                if matches!(e.raw_os_error(), Some(ERROR_PIPE_BUSY | ERROR_FILE_NOT_FOUND))
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => return Err(e).with_context(|| format!("connect {name}")),
        }
    }
}

/// Connects to an endpoint, retrying until `timeout` while it does not exist yet.
#[cfg(unix)]
pub async fn connect(name: &str, timeout: Duration) -> Result<BoxStream> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::net::UnixStream::connect(name).await {
            Ok(s) => return Ok(Box::new(s)),
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => return Err(e).with_context(|| format!("connect {name}")),
        }
    }
}
