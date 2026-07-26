#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

#[cfg(target_os = "macos")]
use objc2_app_kit::NSWorkspace;
#[cfg(target_os = "macos")]
use objc2_foundation::{NSString, NSURL};
use serde::Serialize;
use serde_json::Value;
#[cfg(any(target_os = "linux", windows))]
use std::process::Command as ProcessCommand;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_shell::{
    ShellExt,
    process::{CommandChild, CommandEvent},
};
use tokio::sync::{mpsc, oneshot};

#[tauri::command]
async fn runtime_state(runtime: State<'_, RuntimeClient>) -> Result<Value, String> {
    runtime.request(RuntimeRequest::state()).await
}

#[tauri::command]
async fn runtime_status(runtime: State<'_, RuntimeClient>) -> Result<Value, String> {
    runtime.request(RuntimeRequest::status()).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompanionStatus {
    book_club_installed: bool,
}

#[tauri::command]
fn companion_status() -> CompanionStatus {
    CompanionStatus {
        book_club_installed: book_club_installed(),
    }
}

#[cfg(target_os = "macos")]
fn book_club_installed() -> bool {
    NSURL::URLWithString(&NSString::from_str("bookclub://")).is_some_and(|url| {
        NSWorkspace::sharedWorkspace()
            .URLForApplicationToOpenURL(&url)
            .is_some()
    })
}

#[cfg(target_os = "linux")]
fn book_club_installed() -> bool {
    ProcessCommand::new("xdg-mime")
        .args(["query", "default", "x-scheme-handler/bookclub"])
        .output()
        .is_ok_and(|output| {
            output.status.success() && output.stdout.iter().any(|byte| !byte.is_ascii_whitespace())
        })
}

#[cfg(windows)]
fn book_club_installed() -> bool {
    ProcessCommand::new("reg.exe")
        .args(["query", r"HKCR\bookclub\shell\open\command", "/ve"])
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
const fn book_club_installed() -> bool {
    false
}

#[tauri::command]
async fn runtime_action(
    runtime: State<'_, RuntimeClient>,
    action: String,
    input: Value,
) -> Result<Value, String> {
    const MAX_ACTION_INPUT: usize = 1_048_576;
    if action.is_empty()
        || action.len() > 128
        || !action
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_'))
    {
        return Err("Hydra action name is invalid.".to_owned());
    }
    let payload = serde_json::to_string(&input).map_err(|error| error.to_string())?;
    if payload.len() > MAX_ACTION_INPUT {
        return Err("Hydra action input exceeds 1 MiB.".to_owned());
    }
    runtime
        .request(RuntimeRequest::action(action, payload))
        .await
}

#[derive(Debug)]
struct RuntimeRequest {
    command: &'static str,
    action: Option<String>,
    input: Option<String>,
}

impl RuntimeRequest {
    const fn state() -> Self {
        Self {
            command: "state",
            action: None,
            input: None,
        }
    }

    const fn status() -> Self {
        Self {
            command: "status",
            action: None,
            input: None,
        }
    }

    const fn action(action: String, input: String) -> Self {
        Self {
            command: "action",
            action: Some(action),
            input: Some(input),
        }
    }
}

struct PendingRequest {
    request: RuntimeRequest,
    reply: oneshot::Sender<Result<Value, String>>,
}

#[derive(Clone)]
struct RuntimeClient {
    requests: mpsc::Sender<PendingRequest>,
}

impl RuntimeClient {
    fn start(app: &AppHandle) -> Result<Self, String> {
        let command = app
            .shell()
            .sidecar("hydra-runtime")
            .map_err(|error| format!("Hydra runtime is unavailable: {error}"))?
            .arg("desktop-host");
        let (events, child) = command
            .spawn()
            .map_err(|error| format!("Hydra runtime could not start: {error}"))?;
        let (requests, receiver) = mpsc::channel(32);
        tauri::async_runtime::spawn(runtime_session(receiver, events, child));
        Ok(Self { requests })
    }

    async fn request(&self, request: RuntimeRequest) -> Result<Value, String> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(PendingRequest { request, reply })
            .await
            .map_err(|_| "Hydra runtime session is unavailable.".to_owned())?;
        response
            .await
            .map_err(|_| "Hydra runtime session ended unexpectedly.".to_owned())?
    }
}

async fn runtime_session(
    mut requests: mpsc::Receiver<PendingRequest>,
    mut events: mpsc::Receiver<CommandEvent>,
    mut child: CommandChild,
) {
    while let Some(pending) = requests.recv().await {
        let result = exchange(&pending.request, &mut events, &mut child).await;
        let session_failed = result
            .as_ref()
            .is_err_and(|error| error.contains("session ended"));
        let _ = pending.reply.send(result);
        if session_failed {
            break;
        }
    }
    let _ = child.kill();
}

async fn exchange(
    request: &RuntimeRequest,
    events: &mut mpsc::Receiver<CommandEvent>,
    child: &mut CommandChild,
) -> Result<Value, String> {
    let mut encoded = serde_json::to_vec(&serde_json::json!({
        "command": request.command,
        "action": request.action,
        "input": request.input,
    }))
    .map_err(|error| error.to_string())?;
    encoded.push(b'\n');
    child
        .write(&encoded)
        .map_err(|error| format!("Hydra runtime session ended: {error}"))?;

    let timeout = if request.action.as_deref() == Some("reddit.oauth.connect") {
        Duration::from_secs(360)
    } else {
        Duration::from_secs(120)
    };
    let mut stderr = String::new();
    tokio::time::timeout(timeout, async {
        loop {
            let event = events
                .recv()
                .await
                .ok_or_else(|| "Hydra runtime session ended unexpectedly.".to_owned())?;
            match event {
                CommandEvent::Stdout(bytes) => {
                    let value: Value = serde_json::from_slice(&bytes)
                        .map_err(|error| format!("Hydra runtime returned invalid data: {error}"))?;
                    if value.get("ok").and_then(Value::as_bool) == Some(false) {
                        return Err(value
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("Hydra runtime rejected the request.")
                            .to_owned());
                    }
                    return Ok(value);
                }
                CommandEvent::Stderr(bytes) => {
                    if stderr.len() < 65_536 {
                        stderr.push_str(&String::from_utf8_lossy(&bytes));
                    }
                }
                CommandEvent::Error(error) => {
                    return Err(format!("Hydra runtime session ended: {error}"));
                }
                CommandEvent::Terminated(_) => {
                    let detail = stderr.trim();
                    return Err(if detail.is_empty() {
                        "Hydra runtime session ended unexpectedly.".to_owned()
                    } else {
                        format!("Hydra runtime session ended: {detail}")
                    });
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| "Hydra runtime request timed out; the session ended.".to_owned())?
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {}))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let runtime = RuntimeClient::start(app.handle()).map_err(std::io::Error::other)?;
            app.manage(runtime);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            runtime_state,
            runtime_status,
            runtime_action,
            companion_status
        ])
        .run(tauri::generate_context!())
        .expect("Hydra desktop application failed");
}
