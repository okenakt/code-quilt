//! cq-board: the CodeQuilt dashboard. Tauri commands over one connection to the local daemon.

// Prevents an additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod hub;
mod settings;

use cq_protocol::{CommandInfo, MachineInfo};
use hub::Hub;
use std::sync::Arc;
use tauri::{Manager, State};

type HubState = Arc<Hub>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[tauri::command]
fn machines(hub: State<'_, HubState>) -> Vec<MachineInfo> {
    hub.machines()
}

#[tauri::command]
fn replay(hub: State<'_, HubState>) -> hub::Replay {
    hub.replay()
}

#[tauri::command]
fn list_sessions(hub: State<'_, HubState>) -> Result<(), String> {
    hub.list_sessions().map_err(err)
}

#[tauri::command]
fn subscribe(
    hub: State<'_, HubState>,
    machine: String,
    session: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    hub.subscribe(&machine, &session, cols, rows).map_err(err)
}

#[tauri::command]
fn unsubscribe(hub: State<'_, HubState>, machine: String, session: String) -> Result<(), String> {
    hub.unsubscribe(&machine, &session).map_err(err)
}

#[tauri::command]
fn send_input(
    hub: State<'_, HubState>,
    machine: String,
    session: String,
    data: String,
) -> Result<(), String> {
    hub.input(&machine, &session, data.into_bytes()).map_err(err)
}

#[tauri::command]
fn resize(
    hub: State<'_, HubState>,
    machine: String,
    session: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    hub.resize(&machine, &session, cols, rows).map_err(err)
}

#[tauri::command]
fn approve(
    hub: State<'_, HubState>,
    machine: String,
    session: String,
    option: u8,
    prompt_id: Option<String>,
) -> Result<(), String> {
    hub.approve(&machine, &session, option, prompt_id).map_err(err)
}

#[tauri::command]
fn rename(hub: State<'_, HubState>, machine: String, session: String, name: Option<String>) -> Result<(), String> {
    hub.rename(&machine, &session, name).map_err(err)
}

#[tauri::command]
fn terminate(hub: State<'_, HubState>, machine: String, session: String) -> Result<(), String> {
    hub.terminate(&machine, &session).map_err(err)
}

#[tauri::command]
fn commands(hub: State<'_, HubState>) -> Vec<CommandInfo> {
    hub.commands()
}

#[tauri::command]
async fn invoke_command(hub: State<'_, HubState>, name: String, args: serde_json::Value) -> Result<String, String> {
    hub.invoke_command(name, args).await.map_err(err)
}

/// Stops the local daemon and starts a new one; the board's own connection follows by itself.
#[tauri::command]
async fn restart_daemon() -> Result<(), String> {
    cq_client::stop_daemon().await.map_err(err)?;
    cq_client::connect_daemon().await.map(drop).map_err(err)
}

#[tauri::command]
fn bind_key(key: String, command: String, args: std::collections::BTreeMap<String, String>, unbind: Vec<settings::KeyOf>) -> Result<(), String> {
    settings::bind_key(&key, &command, &args, &unbind)
}

#[tauri::command]
fn unbind_key(key: String, command: String) -> Result<(), String> {
    settings::unbind_key(&key, &command)
}

#[tauri::command]
fn ui_config() -> settings::UiConfig {
    settings::load()
}

#[tauri::command]
fn set_focus(hub: State<'_, HubState>, machine: Option<String>, session: Option<String>) -> Result<(), String> {
    let focus = machine.zip(session).map(|(machine, session)| cq_protocol::SessionRef { machine, session });
    hub.set_focus(focus).map_err(err)
}

#[tauri::command]
async fn launch(hub: State<'_, HubState>, machine: String, agent: String, cwd: String, args: Vec<String>) -> Result<String, String> {
    let id = hub.next_id();
    let req = cq_protocol::Launch { id, machine, agent, cwd, args, name: None, orchestrator: false, cols: 120, rows: 36 };
    launched(&hub, id, cq_protocol::Message::Launch(req)).await
}

#[tauri::command]
async fn resume(hub: State<'_, HubState>, machine: String, session: String) -> Result<String, String> {
    let id = hub.next_id();
    let req = cq_protocol::Resume { id, machine, session, cols: 120, rows: 36 };
    launched(&hub, id, cq_protocol::Message::Resume(req)).await
}

/// Sends a request that starts a session and returns the session's id.
async fn launched(hub: &HubState, id: u64, msg: cq_protocol::Message) -> Result<String, String> {
    match hub.call(id, msg).await.map_err(err)? {
        cq_protocol::Message::Launched(l) => Ok(l.session),
        other => Err(format!("unexpected reply {other:?}")),
    }
}

#[tauri::command]
async fn list_dir(
    hub: State<'_, HubState>,
    machine: String,
    path: String,
) -> Result<cq_protocol::DirList, String> {
    let id = hub.next_id();
    match hub.call(id, cq_protocol::Message::ListDir(cq_protocol::ListDir { id, machine, path })).await.map_err(err)? {
        cq_protocol::Message::DirList(l) => Ok(l),
        other => Err(format!("unexpected reply {other:?}")),
    }
}

/// Contents of a file a session shared, as raw bytes (an `ArrayBuffer` in the frontend).
#[tauri::command]
async fn read_file(
    hub: State<'_, HubState>,
    machine: String,
    session: String,
    path: String,
) -> Result<tauri::ipc::Response, String> {
    let id = hub.next_id();
    let req = cq_protocol::ReadFile { id, machine, session, path };
    match hub.call(id, cq_protocol::Message::ReadFile(req)).await.map_err(err)? {
        cq_protocol::Message::FileContent { data, .. } => Ok(tauri::ipc::Response::new(data)),
        other => Err(format!("unexpected reply {other:?}")),
    }
}

/// Creates the main window from config. Debug builds honor `CQ_DEVTOOLS_PORT` to expose the
/// WebView2 DevTools protocol for automated UI checks.
fn create_main_window(app: &tauri::App) -> tauri::Result<()> {
    let Some(cfg) = app.config().app.windows.iter().find(|w| w.label == "main") else {
        return Ok(());
    };
    #[allow(unused_mut)]
    let mut builder = tauri::WebviewWindowBuilder::from_config(app.handle(), cfg)?;
    #[cfg(all(debug_assertions, windows))]
    if let Ok(port) = std::env::var("CQ_DEVTOOLS_PORT") {
        builder = builder.additional_browser_args(&format!(
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --remote-debugging-port={port}"
        ));
    }
    builder.build()?;
    Ok(())
}

fn main() {
    // Like every cq-* next to cq, the board is asked for its plugin manifest; it declares no commands.
    if std::env::args().nth(1).as_deref() == Some("manifest") {
        println!(r#"{{"name":"board","commands":[]}}"#);
        return;
    }
    tauri::Builder::default()
        .setup(|app| {
            let hub = Hub::start(app.handle().clone());
            app.manage(hub);
            settings::write_defaults();
            settings::watch(app.handle().clone());
            create_main_window(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            machines,
            replay,
            list_sessions,
            subscribe,
            unsubscribe,
            send_input,
            resize,
            approve,
            terminate,
            set_focus,
            launch,
            rename,
            resume,
            list_dir,
            read_file,
            commands,
            invoke_command,
            ui_config,
            restart_daemon,
            bind_key,
            unbind_key
        ])
        .run(tauri::generate_context!())
        .expect("error while running CodeQuilt");
}
