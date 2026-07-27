use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant},
};

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
#[cfg(unix)]
use std::{env, fs};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::output::compact_json;
use crate::semantic::{
    health_search::{
        create_private_dir_all, semantic_model_cache_available, semantic_worker_cache_dir,
    },
    model_contract::semantic_model_key,
    model_runtime::SharedSemanticRuntime,
    paths_status::daemon_root_path,
};

#[cfg(unix)]
use crate::semantic::paths_status::daemon_query_socket_path;

#[cfg(unix)]
use super::transport::read_daemon_query_request_unix;
#[cfg(windows)]
use super::transport::{
    daemon_query_pipe_name, read_daemon_query_request, windows_named_pipe_name_is_local,
    windows_wide_null, WindowsIoDeadline,
};
use super::transport::{
    remove_daemon_query_endpoint, write_daemon_query_endpoint, DaemonQueryEndpoint,
};

pub(in crate::semantic) struct DaemonQueryService {
    pub(in crate::semantic) data_root: PathBuf,
    pub(in crate::semantic) activity: Arc<DaemonQueryActivity>,
    pub(in crate::semantic) thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(unix)]
    pub(in crate::semantic) socket_path: PathBuf,
    #[cfg(unix)]
    pub(in crate::semantic) socket_runtime_dir: Option<PathBuf>,
    #[cfg(windows)]
    pub(in crate::semantic) pipe_name: String,
}

pub(in crate::semantic) const DAEMON_QUERY_REQUEST_MAX_BYTES: usize = 256 * 1024;
pub(in crate::semantic) const DAEMON_QUERY_REQUEST_READ_TIMEOUT: StdDuration =
    StdDuration::from_secs(2);

impl Drop for DaemonQueryService {
    fn drop(&mut self) {
        remove_daemon_query_endpoint(&self.data_root);
        self.activity.stop();
        #[cfg(windows)]
        wake_windows_daemon_query_pipe(&self.pipe_name);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        #[cfg(unix)]
        {
            let _ = fs::remove_file(&self.socket_path);
            if let Some(dir) = self.socket_runtime_dir.as_ref() {
                let _ = fs::remove_dir(dir);
            }
        }
    }
}

#[derive(Default)]
pub(in crate::semantic) struct DaemonQueryActivity {
    pub(in crate::semantic) state: Mutex<DaemonQueryActivityState>,
}

#[derive(Default)]
pub(in crate::semantic) struct DaemonQueryActivityState {
    pub(in crate::semantic) accepting: bool,
    pub(in crate::semantic) stopping: bool,
    pub(in crate::semantic) active_requests: usize,
    pub(in crate::semantic) generation: u64,
}

pub(in crate::semantic) struct DaemonQueryRequestGuard {
    pub(in crate::semantic) activity: Arc<DaemonQueryActivity>,
}

impl DaemonQueryActivity {
    pub(in crate::semantic) fn new() -> Self {
        Self {
            state: Mutex::new(DaemonQueryActivityState {
                accepting: true,
                ..DaemonQueryActivityState::default()
            }),
        }
    }

    pub(in crate::semantic) fn state(&self) -> std::sync::MutexGuard<'_, DaemonQueryActivityState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(in crate::semantic) fn begin_request(self: &Arc<Self>) -> Option<DaemonQueryRequestGuard> {
        let mut state = self.state();
        if !state.accepting || state.stopping {
            return None;
        }
        state.active_requests = state.active_requests.saturating_add(1);
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        Some(DaemonQueryRequestGuard {
            activity: self.clone(),
        })
    }

    pub(in crate::semantic) fn snapshot(&self) -> (usize, u64) {
        let state = self.state();
        (state.active_requests, state.generation)
    }

    pub(in crate::semantic) fn try_stop_accepting_if_idle(&self, observed_generation: u64) -> bool {
        let mut state = self.state();
        if state.active_requests != 0 || state.generation != observed_generation {
            return false;
        }
        state.accepting = false;
        true
    }

    pub(in crate::semantic) fn stop(&self) {
        let mut state = self.state();
        state.accepting = false;
        state.stopping = true;
    }

    pub(in crate::semantic) fn stopping(&self) -> bool {
        self.state().stopping
    }
}

impl Drop for DaemonQueryRequestGuard {
    fn drop(&mut self) {
        let mut state = self.activity.state();
        state.active_requests = state.active_requests.saturating_sub(1);
        state.generation = state.generation.wrapping_add(1);
    }
}

pub(in crate::semantic) fn observe_daemon_query_activity(
    activity: Option<&DaemonQueryActivity>,
    idle_since: &mut Option<Instant>,
    observed_generation: &mut u64,
) {
    let Some(activity) = activity else {
        return;
    };
    let (active_requests, generation) = activity.snapshot();
    if active_requests != 0 || generation != *observed_generation {
        *idle_since = None;
        *observed_generation = generation;
    }
}

pub(in crate::semantic) fn daemon_can_begin_idle_shutdown(
    activity: Option<&DaemonQueryActivity>,
    observed_generation: u64,
) -> bool {
    activity.is_none_or(|activity| activity.try_stop_accepting_if_idle(observed_generation))
}

#[cfg(unix)]
pub(in crate::semantic) const DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES: usize = 90;

#[cfg(unix)]
pub(in crate::semantic) fn bind_daemon_query_listener(
    data_root: &Path,
) -> Result<(UnixListener, PathBuf, Option<PathBuf>)> {
    let preferred = daemon_query_socket_path(data_root);
    if preferred.as_os_str().as_bytes().len() <= DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES {
        let _ = fs::remove_file(&preferred);
        let listener = UnixListener::bind(&preferred)
            .with_context(|| format!("bind daemon query socket {}", preferred.display()))?;
        return Ok((listener, preferred, None));
    }

    let mut roots = vec![PathBuf::from("/tmp")];
    let env_tmp = env::temp_dir();
    if env_tmp != roots[0] {
        roots.push(env_tmp);
    }
    let mut failures = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for _ in 0..8 {
            let runtime_dir = root.join(format!("ctx-q-{}", Uuid::new_v4().simple()));
            match fs::create_dir(&runtime_dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    failures.push(format!("create {}: {error}", runtime_dir.display()));
                    break;
                }
            }
            if let Err(error) = fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700))
            {
                let _ = fs::remove_dir(&runtime_dir);
                failures.push(format!("secure {}: {error}", runtime_dir.display()));
                continue;
            }
            let path = runtime_dir.join("q.sock");
            if path.as_os_str().as_bytes().len() > DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES {
                let _ = fs::remove_dir(&runtime_dir);
                failures.push(format!(
                    "fallback socket path is still too long: {}",
                    path.display()
                ));
                continue;
            }
            match UnixListener::bind(&path) {
                Ok(listener) => return Ok((listener, path, Some(runtime_dir))),
                Err(error) => {
                    let _ = fs::remove_file(&path);
                    let _ = fs::remove_dir(&runtime_dir);
                    failures.push(format!("bind {}: {error}", path.display()));
                }
            }
        }
    }
    Err(anyhow!(
        "daemon query socket path is too long and no short private runtime directory was available: {}",
        failures.join("; ")
    ))
}

#[cfg(unix)]
pub(in crate::semantic) fn start_daemon_query_service(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
) -> Result<DaemonQueryService> {
    start_daemon_query_service_with_request_timeout(
        data_root,
        runtime,
        DAEMON_QUERY_REQUEST_READ_TIMEOUT,
    )
}

#[cfg(unix)]
pub(in crate::semantic) fn start_daemon_query_service_with_request_timeout(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
    request_read_timeout: StdDuration,
) -> Result<DaemonQueryService> {
    let root = daemon_root_path(data_root);
    create_private_dir_all(&root)?;
    let (listener, path, socket_runtime_dir) = bind_daemon_query_listener(data_root)?;
    listener
        .set_nonblocking(true)
        .context("make daemon query socket nonblocking")?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set daemon query socket permissions {}", path.display()))?;
    let endpoint = DaemonQueryEndpoint::Unix {
        path,
        token: Uuid::new_v4().simple().to_string(),
    };
    let socket_path = match &endpoint {
        DaemonQueryEndpoint::Unix { path, .. } => path.clone(),
    };
    if let Err(error) = write_daemon_query_endpoint(data_root, &endpoint) {
        let _ = fs::remove_file(socket_path);
        if let Some(dir) = socket_runtime_dir.as_ref() {
            let _ = fs::remove_dir(dir);
        }
        return Err(error);
    }
    let thread_data_root = data_root.to_path_buf();
    let thread_token = endpoint.token().to_owned();
    let activity = Arc::new(DaemonQueryActivity::new());
    let thread_activity = activity.clone();
    let spawn_result = std::thread::Builder::new()
        .name("ctx-daemon-query".to_owned())
        .spawn(move || {
            while !thread_activity.stopping() {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Accepted Unix sockets inherit nonblocking mode on
                        // macOS. A 384-float response exceeds the default
                        // socket buffer, so restore bounded blocking writes
                        // before serving the request.
                        if configure_daemon_query_stream_unix(&stream, request_read_timeout)
                            .is_err()
                        {
                            continue;
                        }
                        let Some(_request) = thread_activity.begin_request() else {
                            continue;
                        };
                        let request = read_daemon_query_request_unix(
                            &mut stream,
                            DAEMON_QUERY_REQUEST_MAX_BYTES,
                            request_read_timeout,
                        );
                        handle_daemon_query_stream(
                            &thread_data_root,
                            &runtime,
                            &thread_token,
                            stream,
                            request,
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(StdDuration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });
    let thread = match spawn_result {
        Ok(thread) => thread,
        Err(error) => {
            remove_daemon_query_endpoint(data_root);
            let _ = fs::remove_file(socket_path);
            if let Some(dir) = socket_runtime_dir.as_ref() {
                let _ = fs::remove_dir(dir);
            }
            return Err(error).context("start daemon query service thread");
        }
    };
    Ok(DaemonQueryService {
        data_root: data_root.to_path_buf(),
        activity,
        thread: Some(thread),
        socket_path: match endpoint {
            DaemonQueryEndpoint::Unix { path, .. } => path,
        },
        socket_runtime_dir,
    })
}

#[cfg(unix)]
pub(in crate::semantic) fn configure_daemon_query_stream_unix(
    stream: &UnixStream,
    write_timeout: StdDuration,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(write_timeout))
}

#[cfg(windows)]
pub(in crate::semantic) fn start_daemon_query_service(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
) -> Result<DaemonQueryService> {
    start_daemon_query_service_with_request_timeout(
        data_root,
        runtime,
        DAEMON_QUERY_REQUEST_READ_TIMEOUT,
    )
}

#[cfg(windows)]
pub(in crate::semantic) fn start_daemon_query_service_with_request_timeout(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
    request_read_timeout: StdDuration,
) -> Result<DaemonQueryService> {
    let root = daemon_root_path(data_root);
    create_private_dir_all(&root)?;
    let endpoint = DaemonQueryEndpoint::WindowsNamedPipe {
        pipe_name: daemon_query_pipe_name(),
        token: Uuid::new_v4().simple().to_string(),
    };
    let pipe_name = match &endpoint {
        DaemonQueryEndpoint::WindowsNamedPipe { pipe_name, .. } => pipe_name.clone(),
    };
    let first_stream = create_windows_daemon_query_pipe(&pipe_name, true)?;
    if let Err(error) = write_daemon_query_endpoint(data_root, &endpoint) {
        drop(first_stream);
        return Err(error);
    }
    let thread_data_root = data_root.to_path_buf();
    let thread_token = endpoint.token().to_owned();
    let activity = Arc::new(DaemonQueryActivity::new());
    let thread_activity = activity.clone();
    let thread_pipe_name = pipe_name.clone();
    let spawn_result = std::thread::Builder::new()
        .name("ctx-daemon-query".to_owned())
        .spawn(move || {
            let mut next_stream = Some(first_stream);
            while !thread_activity.stopping() {
                let stream = match next_stream.take() {
                    Some(stream) => stream,
                    None => match create_windows_daemon_query_pipe(&thread_pipe_name, false) {
                        Ok(stream) => stream,
                        Err(_) => break,
                    },
                };
                if connect_windows_daemon_query_pipe(&stream).is_err() {
                    break;
                }
                let Some(_request) = thread_activity.begin_request() else {
                    break;
                };
                let stream = stream;
                let request = read_daemon_query_request_windows(
                    &stream,
                    DAEMON_QUERY_REQUEST_MAX_BYTES,
                    request_read_timeout,
                );
                handle_daemon_query_stream(
                    &thread_data_root,
                    &runtime,
                    &thread_token,
                    stream,
                    request,
                );
            }
        });
    let thread = match spawn_result {
        Ok(thread) => thread,
        Err(error) => {
            remove_daemon_query_endpoint(data_root);
            return Err(error).context("start daemon query service thread");
        }
    };
    Ok(DaemonQueryService {
        data_root: data_root.to_path_buf(),
        activity,
        thread: Some(thread),
        pipe_name,
    })
}

#[cfg(windows)]
pub(in crate::semantic) struct WindowsDaemonQueryPipe {
    pub(in crate::semantic) handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
unsafe impl Send for WindowsDaemonQueryPipe {}

#[cfg(windows)]
impl Drop for WindowsDaemonQueryPipe {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Pipes::DisconnectNamedPipe;

        unsafe {
            let _ = DisconnectNamedPipe(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
pub(in crate::semantic) struct WindowsDaemonQueryRequestReader<'a> {
    pub(in crate::semantic) pipe: &'a WindowsDaemonQueryPipe,
    pub(in crate::semantic) deadline: WindowsIoDeadline,
}

#[cfg(windows)]
impl WindowsDaemonQueryRequestReader<'_> {
    pub(in crate::semantic) fn new(
        pipe: &WindowsDaemonQueryPipe,
        timeout: StdDuration,
    ) -> WindowsDaemonQueryRequestReader<'_> {
        WindowsDaemonQueryRequestReader {
            pipe,
            deadline: WindowsIoDeadline::new(timeout),
        }
    }
}

#[cfg(windows)]
impl std::io::Read for WindowsDaemonQueryRequestReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use windows_sys::Win32::Foundation::{
            GetLastError, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED,
        };
        use windows_sys::Win32::Storage::FileSystem::ReadFile;
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;

        if buf.is_empty() {
            return Ok(0);
        }

        loop {
            let mut available = 0u32;
            let ok = unsafe {
                PeekNamedPipe(
                    self.pipe.handle,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let error = unsafe { GetLastError() };
                if matches!(
                    error,
                    ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED
                ) {
                    return Ok(0);
                }
                return Err(std::io::Error::from_raw_os_error(error as i32));
            }
            if available == 0 {
                let wait_ms = self.deadline.remaining_ms("request read")?.min(10);
                std::thread::sleep(StdDuration::from_millis(u64::from(wait_ms)));
                continue;
            }

            let mut bytes_read = 0u32;
            let read_len = buf.len().min(available as usize).min(u32::MAX as usize) as u32;
            let ok = unsafe {
                ReadFile(
                    self.pipe.handle,
                    buf.as_mut_ptr(),
                    read_len,
                    &mut bytes_read,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let error = unsafe { GetLastError() };
                if matches!(
                    error,
                    ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED
                ) {
                    return Ok(0);
                }
                return Err(std::io::Error::from_raw_os_error(error as i32));
            }
            return Ok(bytes_read as usize);
        }
    }
}

#[cfg(windows)]
pub(in crate::semantic) fn read_daemon_query_request_windows(
    pipe: &WindowsDaemonQueryPipe,
    max_bytes: usize,
    timeout: StdDuration,
) -> Result<String> {
    read_daemon_query_request(
        &mut WindowsDaemonQueryRequestReader::new(pipe, timeout),
        max_bytes,
    )
}

#[cfg(windows)]
impl std::io::Write for WindowsDaemonQueryPipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use windows_sys::Win32::Storage::FileSystem::WriteFile;

        if buf.is_empty() {
            return Ok(0);
        }
        let mut bytes_written = 0u32;
        let write_len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let ok = unsafe {
            WriteFile(
                self.handle,
                buf.as_ptr(),
                write_len,
                &mut bytes_written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(bytes_written as usize)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // FlushFileBuffers waits for the client to drain a named pipe and lets a
        // stalled client block the single query-service thread indefinitely.
        // WriteFile has already copied the response into the pipe buffer.
        Ok(())
    }
}

#[cfg(windows)]
pub(in crate::semantic) fn create_windows_daemon_query_pipe(
    pipe_name: &str,
    first_instance: bool,
) -> Result<WindowsDaemonQueryPipe> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    if !windows_named_pipe_name_is_local(pipe_name) {
        return Err(anyhow!("daemon query pipe name is not local"));
    }
    let pipe_name_w = windows_wide_null(pipe_name);
    let access = PIPE_ACCESS_DUPLEX
        | if first_instance {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    let handle = unsafe {
        CreateNamedPipeW(
            pipe_name_w.as_ptr(),
            access,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            1024 * 1024,
            256 * 1024,
            0,
            std::ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("create daemon query named pipe {pipe_name}"));
    }
    Ok(WindowsDaemonQueryPipe { handle })
}

#[cfg(windows)]
pub(in crate::semantic) fn connect_windows_daemon_query_pipe(
    stream: &WindowsDaemonQueryPipe,
) -> Result<()> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_PIPE_CONNECTED};
    use windows_sys::Win32::System::Pipes::ConnectNamedPipe;

    let ok = unsafe { ConnectNamedPipe(stream.handle, std::ptr::null_mut()) };
    if ok != 0 {
        return Ok(());
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_PIPE_CONNECTED {
        return Ok(());
    }
    Err(std::io::Error::last_os_error()).context("connect daemon query named pipe")
}

#[cfg(windows)]
pub(in crate::semantic) fn wake_windows_daemon_query_pipe(pipe_name: &str) {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, OPEN_EXISTING,
    };

    let pipe_name_w = windows_wide_null(pipe_name);
    let handle = unsafe {
        CreateFileW(
            pipe_name_w.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        unsafe {
            let _ = CloseHandle(handle);
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub(in crate::semantic) fn start_daemon_query_service(
    _data_root: &Path,
    _runtime: SharedSemanticRuntime,
) -> Result<DaemonQueryService> {
    Err(anyhow!(
        "daemon query service is not supported on this platform"
    ))
}

pub(in crate::semantic) fn handle_daemon_query_stream<S: std::io::Write>(
    data_root: &Path,
    runtime: &SharedSemanticRuntime,
    token: &str,
    mut stream: S,
    request: Result<String>,
) {
    let result = request.and_then(|body| {
        handle_daemon_query_stream_inner(data_root, runtime, token, &mut stream, &body)
    });
    if let Err(error) = result {
        let _ = writeln!(
            stream,
            "{}",
            serde_json::to_string(&compact_json(json!({
                "ok": false,
                "error": format!("{error:#}"),
            })))
            .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"query failed\"}".to_owned())
        );
    }
}

pub(in crate::semantic) fn handle_daemon_query_stream_inner<S: std::io::Write>(
    data_root: &Path,
    runtime: &SharedSemanticRuntime,
    token: &str,
    stream: &mut S,
    body: &str,
) -> Result<()> {
    let request: Value = serde_json::from_str(body).context("parse daemon query request")?;
    if request.get("token").and_then(Value::as_str) != Some(token) {
        return Err(anyhow!("daemon query authentication failed"));
    }
    let op = request.get("op").and_then(Value::as_str).unwrap_or("");
    if op == "ping" {
        let (embedding_runtime, busy) = runtime.try_runtime_status_json()?;
        writeln!(
            stream,
            "{}",
            serde_json::to_string(&compact_json(json!({
                "ok": true,
                "schema_version": 1,
                "model_key": semantic_model_key(),
                "embedding_runtime": embedding_runtime,
                "busy": busy,
            })))?
        )?;
        return Ok(());
    }
    if op != "embed_query" {
        return Err(anyhow!("unknown daemon query operation `{op}`"));
    }
    let model_key = request
        .get("model_key")
        .and_then(Value::as_str)
        .unwrap_or("");
    if model_key != semantic_model_key() {
        return Err(anyhow!("daemon query model key mismatch"));
    }
    let text = request
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return Err(anyhow!("daemon query text is empty"));
    }
    let started = Instant::now();
    let cache_dir = semantic_worker_cache_dir(data_root);
    if !runtime.is_loaded() && !semantic_model_cache_available(&cache_dir) {
        return Err(anyhow!(
            "semantic model cache is not available to daemon query service"
        ));
    }
    runtime.ensure_loaded_from_cache(&cache_dir)?;
    let (embedding, embedding_runtime) = runtime.embed_query(&cache_dir, text.to_owned())?;
    let query_embed_ms = started.elapsed().as_millis() as u64;
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&compact_json(json!({
            "ok": true,
            "model_key": semantic_model_key(),
            "embedding_runtime": embedding_runtime.to_json(),
            "query_embed_ms": query_embed_ms,
            "embedding": embedding,
        })))?
    )?;
    Ok(())
}
