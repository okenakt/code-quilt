//! CodeQuilt dashboard backend: Tauri commands over per-machine daemon connections.

mod hub;
mod queen;

use cq_proto::MachineInfo;
use hub::Hub;
use queen::Queen;
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
) -> Result<(), String> {
    hub.approve(&machine, &session, option).map_err(err)
}

#[tauri::command]
async fn launch(
    hub: State<'_, HubState>,
    machine: String,
    cwd: String,
    resume: Option<String>,
    args: Vec<String>,
) -> Result<String, String> {
    let id = hub.next_id();
    let req = cq_proto::Launch { id, machine, agent: "claude".into(), cwd, args, resume, cols: 120, rows: 36 };
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

#[tauri::command]
async fn queen_summarize(
    queen: State<'_, Arc<Queen>>,
    session: String,
    text: String,
) -> Result<String, String> {
    queen.summarize(&session, &text).await.map_err(err)
}

#[tauri::command]
async fn queen_route(
    queen: State<'_, Arc<Queen>>,
    message: String,
    candidates: Vec<queen::RouteCandidate>,
) -> Result<queen::RouteDecision, String> {
    queen.route(&message, &candidates).await.map_err(err)
}

#[tauri::command]
fn queen_usage(queen: State<'_, Arc<Queen>>) -> queen::QueenUsage {
    queen.usage()
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

/// Starts the Tauri application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let hub = Hub::start(app.handle().clone());
            app.manage(hub);
            app.manage(Arc::new(Queen::default()));
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
            launch,
            list_dir,
            queen_summarize,
            queen_route,
            queen_usage
        ])
        .run(tauri::generate_context!())
        .expect("error while running CodeQuilt");
}
