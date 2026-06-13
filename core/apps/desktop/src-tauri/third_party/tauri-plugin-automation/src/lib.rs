use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use tauri::{LogicalPosition, LogicalSize, Manager, Runtime, State};

type MessageHandler = unsafe extern "C" fn(*mut c_void);

type MessageHandlerFn = dyn Fn(&mut Message) + Send + Sync;
static MESSAGE_HANDLER: OnceLock<Box<MessageHandlerFn>> = OnceLock::new();

#[tauri::command]
async fn resolve<R: Runtime>(
    _app: tauri::AppHandle<R>,
    automation: State<'_, Automation>,
    id: String,
    result: Option<serde_json::Value>,
) -> Result<(), ()> {
    let mut pending = automation
        .pending_scripts
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(sender) = pending.remove(&id) {
        let _ = sender.send(result.unwrap_or_default());
    }
    Ok(())
}

#[allow(dead_code)]
#[derive(Debug)]
enum MessageKind {
    EvalScript {
        id: String,
        label: Option<String>,
        script: String,
    },
    GetWindowHandle,
    GetWindowHandles,
    CloseWindow {
        label: String,
    },
    GetWindowRect {
        label: Option<String>,
    },
    SetWindowRect {
        label: Option<String>,
        x: Option<i32>,
        y: Option<i32>,
        width: Option<i32>,
        height: Option<i32>,
    },
    FullscreenWindow {
        label: Option<String>,
    },
    MinimizeWindow {
        label: Option<String>,
    },
    MaximizeWindow {
        label: Option<String>,
    },
}

struct Message {
    kind: MessageKind,
    response_tx: Option<tokio::sync::oneshot::Sender<serde_json::Value>>,
}

struct Automation {
    pending_scripts: Mutex<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>,
    ready_labels: Mutex<HashSet<String>>,
}

fn take_response(
    message: &mut Message,
) -> Option<tokio::sync::oneshot::Sender<serde_json::Value>> {
    message.response_tx.take()
}

fn try_send_response(
    sender: tokio::sync::oneshot::Sender<serde_json::Value>,
    value: serde_json::Value,
) {
    let _ = sender.send(value);
}

#[cfg(target_os = "macos")]
fn automation_library_codesign_args(lib_path: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "--force".into(),
        "--sign".into(),
        "-".into(),
        "--timestamp=none".into(),
        lib_path.as_os_str().to_os_string(),
    ]
}

#[cfg(target_os = "macos")]
fn prepare_automation_library_for_load(lib_path: &Path) -> std::io::Result<()> {
    let output = std::process::Command::new("/usr/bin/codesign")
        .args(automation_library_codesign_args(lib_path))
        .output()
        .map_err(|err| {
            std::io::Error::other(format!(
                "failed to spawn codesign for automation library {}: {err}",
                lib_path.display()
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = format!("{stderr}\n{stdout}").trim().to_string();
        if detail.is_empty() {
            return Err(std::io::Error::other(format!(
                "codesign failed for automation library {} with status {}",
                lib_path.display(),
                output.status
            )));
        }
        return Err(std::io::Error::other(format!(
            "codesign failed for automation library {}: {}",
            lib_path.display(),
            detail
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn prepare_automation_library_for_load(_lib_path: &Path) -> std::io::Result<()> {
    Ok(())
}

pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    let (webview_created_tx, webview_created_rx) = tokio::sync::broadcast::channel(16);

    tauri::plugin::Builder::new("automation")
        .invoke_handler(tauri::generate_handler![resolve])
        .js_init_script(include_str!("init.js").to_string())
        .on_webview_ready(move |webview| {
            let automation = webview.app_handle().state::<Automation>();
            automation
                .ready_labels
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert(webview.label().to_string());
            webview_created_tx
                .send(webview.get_webview_window(webview.label()).expect(&format!(
                    "Failed to get webview window for label {}",
                    webview.label()
                )))
                // This could fail if there's no task on the receiving end,
                //  we can ignore safely.
                .unwrap_or_default();
        })
        .setup(|app, _api| {
            app.manage(Automation {
                pending_scripts: Mutex::new(HashMap::new()),
                ready_labels: Mutex::new(HashSet::new()),
            });

            app.add_capability(
                tauri::ipc::CapabilityBuilder::new("automation")
                    .local(true)
                    .window("*")
                    .remote("http://*".into())
                    .remote("https://*".into())
                    .permission("automation:default"),
            )?;

            unsafe {
                if let Some(lib_path) =
                    std::env::var_os("AUTOMATION_LIBRARY_PATH").map(PathBuf::from)
                {
                    prepare_automation_library_for_load(&lib_path)?;
                    let lib = libloading::Library::new(lib_path).expect("Could not load library");
                    let start: libloading::Symbol<unsafe extern "C" fn(MessageHandler)> = lib
                        .get(b"tauri_plugin_automation_start")
                        .expect("Failed to get the automation start function from automation lib");

                    let app_ = app.clone();
                    MESSAGE_HANDLER
                        .set(Box::new(move |message| match &message.kind {
                            MessageKind::EvalScript { id, label, script } => {
                                let id = id.clone();
                                let label = label.clone();
                                let script = script.clone();
                                let automation = app_.state::<Automation>();
                                let mut pending = automation
                                    .pending_scripts
                                    .lock()
                                    .unwrap_or_else(|poison| poison.into_inner());
                                let response_tx = match take_response(message) {
                                    Some(response_tx) => response_tx,
                                    None => return,
                                };
                                pending.insert(id.clone(), response_tx);

                                with_window(
                                    &app_,
                                    label.as_deref(),
                                    &webview_created_rx,
                                    move |window| {
                                        let _ = window.eval(&script);
                                    },
                                );
                            }
                            MessageKind::GetWindowHandle => {
                                let response_tx = match take_response(message) {
                                    Some(response_tx) => response_tx,
                                    None => return,
                                };
                                send_window_handle(&app_, &webview_created_rx, response_tx);
                            }
                            MessageKind::GetWindowHandles => {
                                let response_tx = match take_response(message) {
                                    Some(response_tx) => response_tx,
                                    None => return,
                                };
                                send_window_handles(&app_, &webview_created_rx, response_tx);
                            }
                            MessageKind::CloseWindow { label } => {
                                let label = label.clone();
                                let window = app_.get_webview_window(&label);
                                if let Some(window) = &window {
                                    window.close().expect("Failed to close the window");
                                }
                                if let Some(response_tx) = take_response(message) {
                                    try_send_response(response_tx, window.is_some().into());
                                }
                            }
                            MessageKind::GetWindowRect { label } => {
                                let label = label.clone();
                                let response_tx = match take_response(message) {
                                    Some(response_tx) => response_tx,
                                    None => return,
                                };
                                with_window(
                                    &app_,
                                    label.as_deref(),
                                    &webview_created_rx,
                                    move |window| {
                                        let scale_factor = window
                                            .scale_factor()
                                            .expect("Failed to get window scale factor");
                                        let size = window
                                            .inner_size()
                                            .expect("Failed to get window inner size")
                                            .to_logical::<i32>(scale_factor);
                                        let position = window
                                            .inner_position()
                                            .expect("Failed to get window inner position")
                                            .to_logical::<i32>(scale_factor);
                                        try_send_response(
                                            response_tx,
                                            serde_json::json!({
                                                "x": position.x,
                                                "y": position.y,
                                                "width": size.width,
                                                "height": size.height,
                                            }),
                                        );
                                    },
                                );
                            }
                            MessageKind::SetWindowRect {
                                label,
                                x,
                                y,
                                width,
                                height,
                            } => {
                                let label = label.clone();
                                let x = *x;
                                let y = *y;
                                let width = *width;
                                let height = *height;
                                let response_tx = match take_response(message) {
                                    Some(response_tx) => response_tx,
                                    None => return,
                                };
                                with_window(
                                    &app_,
                                    label.as_deref(),
                                    &webview_created_rx,
                                    move |window| {
                                        if let (Some(x), Some(y)) = (x, y) {
                                            window
                                                .set_position(LogicalPosition::new(x, y))
                                                .expect("Failed to set window position");
                                        }
                                        if let (Some(width), Some(height)) = (width, height) {
                                            window
                                                .set_size(LogicalSize::new(width, height))
                                                .expect("Failed to set window size");
                                        }
                                        try_send_response(response_tx, true.into());
                                    },
                                );
                            }
                            MessageKind::FullscreenWindow { label } => {
                                let label = label.clone();
                                let response_tx = match take_response(message) {
                                    Some(response_tx) => response_tx,
                                    None => return,
                                };
                                with_window(
                                    &app_,
                                    label.as_deref(),
                                    &webview_created_rx,
                                    move |window| {
                                        window
                                            .set_fullscreen(true)
                                            .expect("Failed to fullscreen the window");
                                        try_send_response(response_tx, true.into());
                                    },
                                );
                            }
                            MessageKind::MinimizeWindow { label } => {
                                let label = label.clone();
                                let response_tx = match take_response(message) {
                                    Some(response_tx) => response_tx,
                                    None => return,
                                };
                                with_window(
                                    &app_,
                                    label.as_deref(),
                                    &webview_created_rx,
                                    move |window| {
                                        window.minimize().expect("Failed to minimize the window");
                                        try_send_response(response_tx, true.into());
                                    },
                                );
                            }
                            MessageKind::MaximizeWindow { label } => {
                                let label = label.clone();
                                let response_tx = match take_response(message) {
                                    Some(response_tx) => response_tx,
                                    None => return,
                                };
                                with_window(
                                    &app_,
                                    label.as_deref(),
                                    &webview_created_rx,
                                    move |window| {
                                        window.maximize().expect("Failed to maximize window");
                                        try_send_response(response_tx, true.into());
                                    },
                                );
                            }
                        }))
                        .unwrap_or_else(|_| {
                            panic!("Failed to set message handler");
                        });

                    start(handle_message);
                }
            }

            Ok(())
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn automation_library_codesign_args_match_expected_shape() {
        let lib_path = Path::new("/tmp/automation_bindings");
        let args = automation_library_codesign_args(lib_path);
        let rendered = args
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            rendered,
            vec![
                "--force".to_string(),
                "--sign".to_string(),
                "-".to_string(),
                "--timestamp=none".to_string(),
                "/tmp/automation_bindings".to_string(),
            ]
        );
    }
}

extern "C" fn handle_message(message: *mut c_void) {
    let message = unsafe { &mut *(message as *mut Message) };
    MESSAGE_HANDLER.get().unwrap()(message);
}

fn with_window<R: Runtime, F: FnOnce(tauri::WebviewWindow<R>) + Send + 'static>(
    app: &tauri::AppHandle<R>,
    label: Option<&str>,
    webview_created_rx: &tokio::sync::broadcast::Receiver<tauri::WebviewWindow<R>>,
    f: F,
) {
    if let Some(window) = ready_window_by_label(app, label) {
        f(window);
    } else {
        let wanted_label = label.map(str::to_string);
        let mut webview_created_rx = webview_created_rx.resubscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                let window = webview_created_rx.recv().await;
                if let Ok(webview) = window {
                    if wanted_label
                        .as_deref()
                        .map(|label| webview.label() == label)
                        .unwrap_or(true)
                    {
                        f(webview);
                        break;
                    }
                }
            }
        });
    }
}

fn send_window_handle<R: Runtime>(
    app: &tauri::AppHandle<R>,
    webview_created_rx: &tokio::sync::broadcast::Receiver<tauri::WebviewWindow<R>>,
    response_tx: tokio::sync::oneshot::Sender<serde_json::Value>,
) {
    if let Some(window) = ready_window_by_label(app, None) {
        try_send_response(response_tx, window.label().to_string().into());
    } else {
        let mut webview_created_rx = webview_created_rx.resubscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                let window = webview_created_rx.recv().await;
                if let Ok(webview) = window {
                    try_send_response(response_tx, webview.label().to_string().into());
                    break;
                }
            }
        });
    }
}

fn send_window_handles<R: Runtime>(
    app: &tauri::AppHandle<R>,
    webview_created_rx: &tokio::sync::broadcast::Receiver<tauri::WebviewWindow<R>>,
    response_tx: tokio::sync::oneshot::Sender<serde_json::Value>,
) {
    let handles: Vec<String> = ready_window_labels(app);
    if handles.is_empty() {
        let app = app.clone();
        let mut webview_created_rx = webview_created_rx.resubscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                let window = webview_created_rx.recv().await;
                if window.is_ok() {
                    try_send_response(response_tx, ready_window_labels(&app).into());
                    break;
                }
            }
        });
    } else {
        try_send_response(response_tx, handles.into());
    }
}

fn ready_window_labels<R: Runtime>(app: &tauri::AppHandle<R>) -> Vec<String> {
    let automation = app.state::<Automation>();
    let ready_labels = automation
        .ready_labels
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let mut labels: Vec<String> = ready_labels
        .iter()
        .filter_map(|label| app.get_webview_window(label).map(|_| label.clone()))
        .collect();
    labels.sort();
    labels
}

fn ready_window_by_label<R: Runtime>(
    app: &tauri::AppHandle<R>,
    label: Option<&str>,
) -> Option<tauri::WebviewWindow<R>> {
    let automation = app.state::<Automation>();
    let ready_labels = automation
        .ready_labels
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(label) = label {
        if !ready_labels.contains(label) {
            return None;
        }
        app.get_webview_window(label)
    } else if ready_labels.contains("main") {
        app.get_webview_window("main")
    } else {
        ready_labels
            .iter()
            .find_map(|label| app.get_webview_window(label))
    }
}
