//! Terminal client (`cq attach`, and the foreground half of `cq claude`).

use crate::term::{self, RawTerminal, TermEvent};
use crate::{ipc, paths};
use anyhow::{Result, bail};
use cq_proto::{
    ClientKind, Hello, Message, PROTOCOL_VERSION, Size, read_message, spawn_reader, write_message,
};
use std::io::Write;
use std::time::Duration;
use tokio::sync::mpsc;

/// Appends captured input bytes to `~/.codequilt/logs/input-trace.log` when `CQ_TRACE` is set.
fn trace_input(bytes: &[u8]) {
    if std::env::var_os("CQ_TRACE").is_none() {
        return;
    }
    let Ok(dir) = paths::root().map(|r| r.join("logs")) else { return };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("input-trace.log"))
    {
        let _ = writeln!(f, "{}", bytes.escape_ascii());
    }
}

/// Why the attach loop ended.
pub enum Outcome {
    Exited(Option<u32>),
    Detached,
    HostLost,
}

/// Attaches the current terminal to the session at `endpoint` until exit or detach.
pub async fn attach(endpoint: &str, connect_timeout: Duration) -> Result<Outcome> {
    let stream = ipc::connect(endpoint, connect_timeout).await?;
    let (mut rd, mut wr) = tokio::io::split(stream);
    let size = term::size();
    write_message(
        &mut wr,
        &Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            kind: ClientKind::Terminal,
            cols: size.cols,
            rows: size.rows,
            federate: false,
        }),
    )
    .await?;
    match read_message(&mut rd).await? {
        Some(Message::Welcome(_)) => {}
        Some(Message::Error(e)) => bail!("host refused connection: {e}"),
        _ => bail!("unexpected handshake reply"),
    }

    let raw = RawTerminal::enter()?;
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    term::spawn_reader(ev_tx.clone());
    {
        // Resize events are not reliable in VT input mode, so also poll the window size.
        tokio::spawn(async move {
            let mut last = size;
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let now = term::size();
                if now != last {
                    last = now;
                    if ev_tx.send(TermEvent::Resize(now)).is_err() {
                        return;
                    }
                }
            }
        });
    }

    let mut msgs = spawn_reader(rd);
    let outcome = loop {
        tokio::select! {
            msg = msgs.recv() => match msg {
                Some(Ok(Message::Snapshot(b) | Message::Output(b))) => raw.write(&b),
                Some(Ok(Message::Exit(e))) => break Outcome::Exited(e.code),
                Some(Ok(_)) => {}
                None | Some(Err(_)) => break Outcome::HostLost,
            },
            ev = ev_rx.recv() => match ev {
                Some(TermEvent::Input(bytes)) => {
                    trace_input(&bytes);
                    if term::is_detach(&bytes) {
                        break Outcome::Detached;
                    }
                    if write_message(&mut wr, &Message::Input(bytes)).await.is_err() {
                        break Outcome::HostLost;
                    }
                }
                Some(TermEvent::Resize(Size { cols, rows })) => {
                    let _ = write_message(&mut wr, &Message::Resize(Size { cols, rows })).await;
                }
                None => break Outcome::Detached,
            },
        }
    };
    drop(raw);
    Ok(outcome)
}

/// Prints a one-line summary of how the attach ended and returns the process exit code.
pub fn report(id: &str, outcome: Outcome) -> i32 {
    match outcome {
        Outcome::Exited(code) => code.map(|c| c as i32).unwrap_or(0),
        Outcome::Detached => {
            eprintln!("[cq] detached from {id}; reattach with `cq attach {id}`");
            0
        }
        Outcome::HostLost => {
            eprintln!("[cq] lost connection to session host {id}");
            1
        }
    }
}
