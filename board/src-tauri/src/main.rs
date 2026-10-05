//! cq-board: the CodeQuilt dashboard. Tauri commands over one connection to the local daemon.

// Prevents an additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod hub;
mod settings;

use cq_proto::{CommandInfo, MachineInfo};
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
fn terminate(hub: State<'_, HubState>, machine: String, session: String) -> Result<(), String> {
    hub.terminate(&machine, &session).map_err(err)
}

#[tauri::command]
fn commands(hub: State<'_, HubState>) -> Vec<CommandInfo> {
    hub.commands()
}

#[tauri::command]
async fn invoke_command(
    hub: State<'_, HubState>,
    name: String,
    args: serde_json::Value,
    phase: String,
) -> Result<String, String> {
    hub.invoke_command(name, args, phase).await.map_err(err)
}

#[tauri::command]
fn restart_daemon(hub: State<'_, HubState>, machine: String) -> Result<(), String> {
    hub.restart_daemon(machine).map_err(err)
}

#[tauri::command]
fn ui_config() -> settings::UiConfig {
    settings::load()
}

#[tauri::command]
fn set_focus(hub: State<'_, HubState>, machine: Option<String>, session: Option<String>) -> Result<(), String> {
    let focus = machine.zip(session).map(|(machine, session)| cq_proto::SessionRef { machine, session });
    hub.set_focus(focus).map_err(err)
}

#[tauri::command]
async fn launch(
    hub: State<'_, HubState>,
    machine: String,
    agent: String,
    cwd: String,
    resume: Option<String>,
    args: Vec<String>,
) -> Result<String, String> {
    let id = hub.next_id();
    let req = cq_proto::Launch {
        id,
        machine,
        agent,
        cwd,
        args,
        resume,
        name: None,
        settings_extra: None,
        cols: 120,
        rows: 36,
    };
    match hub.call(id, cq_proto::Message::Launch(req)).await.map_err(err)? {
        cq_proto::Message::Launched(l) => Ok(l.session),
        other => Err(format!("unexpected reply {other:?}")),
    }
}

#[tauri::command]
async fn list_dir(
    hub: State<'_, HubState>,
    machine: String,
    path: String,
) -> Result<cq_proto::DirList, String> {
    let id = hub.next_id();
    match hub.call(id, cq_proto::Message::ListDir(cq_proto::ListDir { id, machine, path })).await.map_err(err)? {
        cq_proto::Message::DirList(l) => Ok(l),
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
        .plugin(tauri_plugin_notification::init())
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
            list_dir,
            commands,
            invoke_command,
            ui_config,
            restart_daemon
        ])
        .run(tauri::generate_context!())
        .expect("error while running CodeQuilt");
}
