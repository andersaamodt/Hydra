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
use tokio::sync::{Mutex, mpsc, oneshot};

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

struct RuntimeClient {
    app: AppHandle,
    requests: Mutex<mpsc::Sender<PendingRequest>>,
}

impl RuntimeClient {
    fn spawn_session(app: &AppHandle) -> Result<mpsc::Sender<PendingRequest>, String> {
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
        Ok(requests)
    }

    fn start(app: &AppHandle) -> Result<Self, String> {
        Ok(Self {
            app: app.clone(),
            requests: Mutex::new(Self::spawn_session(app)?),
        })
    }

    async fn request(&self, request: RuntimeRequest) -> Result<Value, String> {
        request_with_one_restart(&self.requests, request, || Self::spawn_session(&self.app)).await
    }
}

async fn request_with_one_restart<F>(
    requests: &Mutex<mpsc::Sender<PendingRequest>>,
    request: RuntimeRequest,
    restart: F,
) -> Result<Value, String>
where
    F: FnOnce() -> Result<mpsc::Sender<PendingRequest>, String>,
{
    let mut session = requests.lock().await;
    let (reply, response) = oneshot::channel();
    enqueue_with_one_restart(&mut session, PendingRequest { request, reply }, restart).await?;
    let result = response
        .await
        .map_err(|_| "Hydra runtime session ended unexpectedly.".to_owned())?;
    drop(session);
    result
}

async fn enqueue_with_one_restart<F>(
    requests: &mut mpsc::Sender<PendingRequest>,
    pending: PendingRequest,
    restart: F,
) -> Result<(), String>
where
    F: FnOnce() -> Result<mpsc::Sender<PendingRequest>, String>,
{
    let pending = match requests.send(pending).await {
        Ok(()) => return Ok(()),
        Err(error) => error.0,
    };
    *requests = restart()?;
    requests
        .send(pending)
        .await
        .map_err(|_| "Hydra runtime session could not restart.".to_owned())
}

enum ExchangeError {
    Request(String),
    Session(String),
}

fn publish_exchange_result(
    requests: &mut mpsc::Receiver<PendingRequest>,
    pending: PendingRequest,
    result: Result<Value, ExchangeError>,
) -> bool {
    match result {
        Ok(value) => {
            let _ = pending.reply.send(Ok(value));
            false
        }
        Err(ExchangeError::Request(error)) => {
            let _ = pending.reply.send(Err(error));
            false
        }
        Err(ExchangeError::Session(error)) => {
            requests.close();
            let _ = pending.reply.send(Err(error));
            true
        }
    }
}

async fn runtime_session(
    mut requests: mpsc::Receiver<PendingRequest>,
    mut events: mpsc::Receiver<CommandEvent>,
    mut child: CommandChild,
) {
    while let Some(pending) = requests.recv().await {
        let result = exchange(&pending.request, &mut events, &mut child).await;
        if publish_exchange_result(&mut requests, pending, result) {
            break;
        }
    }
    let _ = child.kill();
}

async fn exchange(
    request: &RuntimeRequest,
    events: &mut mpsc::Receiver<CommandEvent>,
    child: &mut CommandChild,
) -> Result<Value, ExchangeError> {
    let mut encoded = serde_json::to_vec(&serde_json::json!({
        "command": request.command,
        "action": request.action,
        "input": request.input,
    }))
    .map_err(|error| ExchangeError::Request(error.to_string()))?;
    encoded.push(b'\n');
    child
        .write(&encoded)
        .map_err(|error| ExchangeError::Session(format!("Hydra runtime session ended: {error}")))?;

    let timeout = if request.action.as_deref() == Some("reddit.oauth.connect") {
        Duration::from_secs(360)
    } else {
        Duration::from_secs(120)
    };
    let mut stderr = String::new();
    tokio::time::timeout(timeout, async {
        loop {
            let event = events.recv().await.ok_or_else(|| {
                ExchangeError::Session("Hydra runtime session ended unexpectedly.".to_owned())
            })?;
            match event {
                CommandEvent::Stdout(bytes) => {
                    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                        ExchangeError::Session(format!(
                            "Hydra runtime returned invalid data: {error}"
                        ))
                    })?;
                    if value.get("ok").and_then(Value::as_bool) == Some(false) {
                        return Err(ExchangeError::Request(
                            value
                                .get("error")
                                .and_then(Value::as_str)
                                .unwrap_or("Hydra runtime rejected the request.")
                                .to_owned(),
                        ));
                    }
                    return Ok(value);
                }
                CommandEvent::Stderr(bytes) => {
                    if stderr.len() < 65_536 {
                        stderr.push_str(&String::from_utf8_lossy(&bytes));
                    }
                }
                CommandEvent::Error(error) => {
                    return Err(ExchangeError::Session(format!(
                        "Hydra runtime session ended: {error}"
                    )));
                }
                CommandEvent::Terminated(_) => {
                    let detail = stderr.trim();
                    return Err(ExchangeError::Session(if detail.is_empty() {
                        "Hydra runtime session ended unexpectedly.".to_owned()
                    } else {
                        format!("Hydra runtime session ended: {detail}")
                    }));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| {
        ExchangeError::Session("Hydra runtime request timed out; the session ended.".to_owned())
    })?
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

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[test]
    fn unaccepted_request_restarts_one_dead_session() {
        tauri::async_runtime::block_on(async {
            let (dead, receiver) = mpsc::channel(1);
            drop(receiver);
            let requests = Mutex::new(dead);
            let restarts = Arc::new(AtomicUsize::new(0));
            let restart_count = Arc::clone(&restarts);

            let response =
                request_with_one_restart(&requests, RuntimeRequest::state(), move || {
                    restart_count.fetch_add(1, Ordering::Relaxed);
                    let (sender, mut receiver) = mpsc::channel::<PendingRequest>(1);
                    tokio::spawn(async move {
                        let pending = receiver.recv().await.expect("replacement receives request");
                        assert_eq!(pending.request.command, "state");
                        let _ = pending.reply.send(Ok(serde_json::json!({ "ok": true })));
                    });
                    Ok(sender)
                })
                .await
                .expect("dead session should restart");

            assert_eq!(restarts.load(Ordering::Relaxed), 1);
            assert_eq!(response, serde_json::json!({ "ok": true }));
        });
    }

    #[test]
    fn accepted_request_is_never_replayed_when_its_reply_is_dropped() {
        tauri::async_runtime::block_on(async {
            let (sender, mut receiver) = mpsc::channel::<PendingRequest>(1);
            let requests = Mutex::new(sender);
            let restarts = Arc::new(AtomicUsize::new(0));
            let executions = Arc::new(AtomicUsize::new(0));
            let execution_count = Arc::clone(&executions);
            let worker = tokio::spawn(async move {
                let pending = receiver.recv().await.expect("worker receives request");
                execution_count.fetch_add(1, Ordering::Relaxed);
                drop(pending);
            });
            let restart_count = Arc::clone(&restarts);

            let error = request_with_one_restart(
                &requests,
                RuntimeRequest::action("post.create".to_owned(), "{}".to_owned()),
                move || {
                    restart_count.fetch_add(1, Ordering::Relaxed);
                    Err("accepted requests must not restart inline".to_owned())
                },
            )
            .await
            .expect_err("dropped reply should surface without replay");
            worker.await.expect("worker should finish");

            assert_eq!(error, "Hydra runtime session ended unexpectedly.");
            assert_eq!(executions.load(Ordering::Relaxed), 1);
            assert_eq!(restarts.load(Ordering::Relaxed), 0);
        });
    }

    #[test]
    fn failed_replacement_is_attempted_only_once() {
        tauri::async_runtime::block_on(async {
            let (dead, receiver) = mpsc::channel(1);
            drop(receiver);
            let requests = Mutex::new(dead);
            let restarts = Arc::new(AtomicUsize::new(0));
            let restart_count = Arc::clone(&restarts);

            let error = request_with_one_restart(&requests, RuntimeRequest::state(), move || {
                restart_count.fetch_add(1, Ordering::Relaxed);
                Err("Hydra runtime could not start: unavailable".to_owned())
            })
            .await
            .expect_err("failed replacement should surface once");

            assert_eq!(error, "Hydra runtime could not start: unavailable");
            assert_eq!(restarts.load(Ordering::Relaxed), 1);
        });
    }

    #[test]
    fn next_request_waits_for_the_full_previous_lifecycle() {
        tauri::async_runtime::block_on(async {
            let (sender, mut receiver) = mpsc::channel::<PendingRequest>(2);
            let requests = Arc::new(Mutex::new(sender));
            let first_requests = Arc::clone(&requests);
            let first = tokio::spawn(async move {
                request_with_one_restart(&first_requests, RuntimeRequest::state(), || {
                    Err("first request must not restart".to_owned())
                })
                .await
            });
            let first_pending = receiver.recv().await.expect("first request should enqueue");
            assert_eq!(first_pending.request.command, "state");

            let second_requests = Arc::clone(&requests);
            let (second_started, started) = oneshot::channel();
            let second = tokio::spawn(async move {
                let _ = second_started.send(());
                request_with_one_restart(&second_requests, RuntimeRequest::status(), || {
                    Err("second request must not restart".to_owned())
                })
                .await
            });
            started.await.expect("second request should start");
            assert!(
                tokio::time::timeout(Duration::from_millis(50), receiver.recv())
                    .await
                    .is_err(),
                "second request must not enqueue while the first reply is pending"
            );

            let _ = first_pending
                .reply
                .send(Ok(serde_json::json!({ "request": 1 })));
            assert_eq!(
                first.await.expect("first task should finish"),
                Ok(serde_json::json!({ "request": 1 }))
            );

            let second_pending = receiver
                .recv()
                .await
                .expect("second request should enqueue after the first resolves");
            assert_eq!(second_pending.request.command, "status");
            let _ = second_pending
                .reply
                .send(Ok(serde_json::json!({ "request": 2 })));
            assert_eq!(
                second.await.expect("second task should finish"),
                Ok(serde_json::json!({ "request": 2 }))
            );
        });
    }

    #[test]
    fn request_error_text_that_mentions_session_ended_is_nonfatal() {
        tauri::async_runtime::block_on(async {
            let (sender, mut requests) = mpsc::channel(1);
            let (reply, response) = oneshot::channel();
            let pending = PendingRequest {
                request: RuntimeRequest::state(),
                reply,
            };
            let message = "The remote note says its session ended yesterday.";

            assert!(!publish_exchange_result(
                &mut requests,
                pending,
                Err(ExchangeError::Request(message.to_owned()))
            ));
            assert!(!sender.is_closed());
            let (next_reply, _next_response) = oneshot::channel();
            assert!(
                sender
                    .try_send(PendingRequest {
                        request: RuntimeRequest::status(),
                        reply: next_reply,
                    })
                    .is_ok()
            );
            assert_eq!(
                response.await.expect("request error should be published"),
                Err(message.to_owned())
            );
        });
    }

    #[test]
    fn fatal_exchange_closes_requests_before_publishing_its_reply() {
        tauri::async_runtime::block_on(async {
            let (sender, mut requests) = mpsc::channel(1);
            let (reply, response) = oneshot::channel();
            let pending = PendingRequest {
                request: RuntimeRequest::state(),
                reply,
            };
            let message = "Hydra runtime session ended unexpectedly.";

            assert!(publish_exchange_result(
                &mut requests,
                pending,
                Err(ExchangeError::Session(message.to_owned()))
            ));
            assert!(sender.is_closed());
            let (next_reply, _next_response) = oneshot::channel();
            assert!(matches!(
                sender.try_send(PendingRequest {
                    request: RuntimeRequest::status(),
                    reply: next_reply,
                }),
                Err(mpsc::error::TrySendError::Closed(_))
            ));
            assert_eq!(
                response.await.expect("session error should be published"),
                Err(message.to_owned())
            );
        });
    }
}
