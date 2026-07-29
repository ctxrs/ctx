use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration as StdDuration, Instant},
};

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
#[cfg(unix)]
use std::{env, fs};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::{
    BatchHydrationRequest, ContentSourceResolver, EventHydrationRequest, HydrationFailure,
    HydrationFailureKind, SourceRecordLocator, StableEntityId,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::output::compact_json;
use crate::semantic::{
    daemon_wakeup::DaemonWakeup,
    health_search::{
        create_private_dir_all, semantic_model_cache_available, semantic_worker_cache_dir,
    },
    model_contract::semantic_model_key,
    model_runtime::SharedSemanticRuntime,
    paths_status::{
        daemon_root_path, daemon_source_backed_refresh_job_path, read_daemon_job_status,
    },
    source_backed_refresh_coordinator::{
        SourceBackedRefreshCoordinator, SourceBackedResolverAccessError,
    },
};

#[cfg(unix)]
use crate::semantic::paths_status::daemon_query_socket_path;

use super::hydration_budget::{
    provider_body_is_admitted, provider_read_reservation_bytes, retained_response_item_charge,
    successful_response_envelope_charge, HydrationBudgetError, HydrationBudgetSnapshot,
    HydrationByteBudget, SOURCE_HYDRATION_MAX_BYTES,
};
#[cfg(unix)]
use super::transport::read_daemon_query_request_unix;
#[cfg(windows)]
use super::transport::{
    daemon_query_pipe_name, read_daemon_query_request, windows_named_pipe_name_is_local,
    windows_wide_null, WindowsIoDeadline,
};
use super::transport::{
    remove_daemon_service_endpoint, write_daemon_service_endpoint, DaemonIpcService,
    DaemonQueryEndpoint,
};
#[cfg(windows)]
#[path = "windows_security.rs"]
mod windows_security;
#[cfg(windows)]
use windows_security::WindowsDaemonQueryPipeSecurity;

pub(in crate::semantic) struct DaemonQueryService {
    pub(in crate::semantic) data_root: PathBuf,
    pub(in crate::semantic) service: DaemonIpcService,
    pub(in crate::semantic) activity: Arc<DaemonQueryActivity>,
    pub(in crate::semantic) source_refresh: Arc<SourceBackedRefreshCoordinator>,
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
pub(in crate::semantic) const DAEMON_SOURCE_HYDRATION_MAX_ITEMS: usize = 128;
pub(in crate::semantic) const DAEMON_SOURCE_HYDRATION_MAX_WORKERS: usize = 8;
pub(in crate::semantic) const DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES: usize =
    SOURCE_HYDRATION_MAX_BYTES;

impl Drop for DaemonQueryService {
    fn drop(&mut self) {
        remove_daemon_service_endpoint(&self.data_root, self.service);
        self.activity.stop();
        #[cfg(unix)]
        {
            let _ = UnixStream::connect(&self.socket_path);
        }
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

    pub(in crate::semantic) fn resume_accepting(&self) {
        let mut state = self.state();
        if !state.stopping {
            state.accepting = true;
        }
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
#[cfg(test)]
pub(in crate::semantic) fn bind_daemon_query_listener(
    data_root: &Path,
) -> Result<(UnixListener, PathBuf, Option<PathBuf>)> {
    bind_daemon_service_listener(data_root, DaemonIpcService::SemanticQuery)
}

#[cfg(unix)]
pub(in crate::semantic) fn bind_daemon_service_listener(
    data_root: &Path,
    service: DaemonIpcService,
) -> Result<(UnixListener, PathBuf, Option<PathBuf>)> {
    let preferred = match service {
        DaemonIpcService::SemanticQuery => daemon_query_socket_path(data_root),
        DaemonIpcService::SourceRefresh => daemon_root_path(data_root).join("source-refresh.sock"),
    };
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
    start_daemon_service_with_request_timeout(
        data_root,
        runtime,
        DAEMON_QUERY_REQUEST_READ_TIMEOUT,
        DaemonIpcService::SemanticQuery,
        None,
    )
}

#[cfg(unix)]
#[cfg(test)]
pub(in crate::semantic) fn start_daemon_query_service_with_request_timeout(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
    request_read_timeout: StdDuration,
) -> Result<DaemonQueryService> {
    start_daemon_service_with_request_timeout(
        data_root,
        runtime,
        request_read_timeout,
        DaemonIpcService::SemanticQuery,
        None,
    )
}

#[cfg(unix)]
pub(in crate::semantic) fn start_daemon_source_refresh_service(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
    wakeup: Arc<DaemonWakeup>,
) -> Result<DaemonQueryService> {
    start_daemon_service_with_request_timeout(
        data_root,
        runtime,
        DAEMON_QUERY_REQUEST_READ_TIMEOUT,
        DaemonIpcService::SourceRefresh,
        Some(wakeup),
    )
}

#[cfg(unix)]
#[cfg(test)]
pub(in crate::semantic) fn start_daemon_source_refresh_service_with_request_timeout(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
    request_read_timeout: StdDuration,
) -> Result<DaemonQueryService> {
    start_daemon_service_with_request_timeout(
        data_root,
        runtime,
        request_read_timeout,
        DaemonIpcService::SourceRefresh,
        Some(Arc::new(DaemonWakeup::default())),
    )
}

#[cfg(unix)]
fn start_daemon_service_with_request_timeout(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
    request_read_timeout: StdDuration,
    service: DaemonIpcService,
    wakeup: Option<Arc<DaemonWakeup>>,
) -> Result<DaemonQueryService> {
    let root = daemon_root_path(data_root);
    create_private_dir_all(&root)?;
    let (listener, path, socket_runtime_dir) = bind_daemon_service_listener(data_root, service)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set daemon query socket permissions {}", path.display()))?;
    let endpoint = DaemonQueryEndpoint::Unix {
        path,
        token: Uuid::new_v4().simple().to_string(),
    };
    let socket_path = match &endpoint {
        DaemonQueryEndpoint::Unix { path, .. } => path.clone(),
    };
    if let Err(error) = write_daemon_service_endpoint(data_root, service, &endpoint) {
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
    let source_refresh = Arc::new(SourceBackedRefreshCoordinator::new());
    let thread_source_refresh = source_refresh.clone();
    let thread_wakeup = wakeup;
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
                            &thread_source_refresh,
                            service,
                            &thread_token,
                            stream,
                            request,
                            thread_wakeup.as_deref(),
                        );
                        if service == DaemonIpcService::SourceRefresh
                            && thread_source_refresh.has_pending_request()
                        {
                            if let Some(wakeup) = thread_wakeup.as_ref() {
                                wakeup.signal_ipc();
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    let thread = match spawn_result {
        Ok(thread) => thread,
        Err(error) => {
            remove_daemon_service_endpoint(data_root, service);
            let _ = fs::remove_file(socket_path);
            if let Some(dir) = socket_runtime_dir.as_ref() {
                let _ = fs::remove_dir(dir);
            }
            return Err(error).context("start daemon query service thread");
        }
    };
    Ok(DaemonQueryService {
        data_root: data_root.to_path_buf(),
        service,
        activity,
        source_refresh,
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
    start_daemon_service_with_request_timeout(
        data_root,
        runtime,
        DAEMON_QUERY_REQUEST_READ_TIMEOUT,
        DaemonIpcService::SemanticQuery,
        None,
    )
}

#[cfg(windows)]
#[cfg(test)]
pub(in crate::semantic) fn start_daemon_query_service_with_request_timeout(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
    request_read_timeout: StdDuration,
) -> Result<DaemonQueryService> {
    start_daemon_service_with_request_timeout(
        data_root,
        runtime,
        request_read_timeout,
        DaemonIpcService::SemanticQuery,
        None,
    )
}

#[cfg(windows)]
pub(in crate::semantic) fn start_daemon_source_refresh_service(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
    wakeup: Arc<DaemonWakeup>,
) -> Result<DaemonQueryService> {
    start_daemon_service_with_request_timeout(
        data_root,
        runtime,
        DAEMON_QUERY_REQUEST_READ_TIMEOUT,
        DaemonIpcService::SourceRefresh,
        Some(wakeup),
    )
}

#[cfg(windows)]
#[cfg(test)]
pub(in crate::semantic) fn start_daemon_source_refresh_service_with_request_timeout(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
    request_read_timeout: StdDuration,
) -> Result<DaemonQueryService> {
    start_daemon_service_with_request_timeout(
        data_root,
        runtime,
        request_read_timeout,
        DaemonIpcService::SourceRefresh,
        Some(Arc::new(DaemonWakeup::default())),
    )
}

#[cfg(windows)]
fn start_daemon_service_with_request_timeout(
    data_root: &Path,
    runtime: SharedSemanticRuntime,
    request_read_timeout: StdDuration,
    service: DaemonIpcService,
    wakeup: Option<Arc<DaemonWakeup>>,
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
    if let Err(error) = write_daemon_service_endpoint(data_root, service, &endpoint) {
        drop(first_stream);
        return Err(error);
    }
    let thread_data_root = data_root.to_path_buf();
    let thread_token = endpoint.token().to_owned();
    let activity = Arc::new(DaemonQueryActivity::new());
    let thread_activity = activity.clone();
    let source_refresh = Arc::new(SourceBackedRefreshCoordinator::new());
    let thread_source_refresh = source_refresh.clone();
    let thread_wakeup = wakeup;
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
                    &thread_source_refresh,
                    service,
                    &thread_token,
                    stream,
                    request,
                    thread_wakeup.as_deref(),
                );
                if service == DaemonIpcService::SourceRefresh
                    && thread_source_refresh.has_pending_request()
                {
                    if let Some(wakeup) = thread_wakeup.as_ref() {
                        wakeup.signal_ipc();
                    }
                }
            }
        });
    let thread = match spawn_result {
        Ok(thread) => thread,
        Err(error) => {
            remove_daemon_service_endpoint(data_root, service);
            return Err(error).context("start daemon query service thread");
        }
    };
    Ok(DaemonQueryService {
        data_root: data_root.to_path_buf(),
        service,
        activity,
        source_refresh,
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
    let mut pipe_security = WindowsDaemonQueryPipeSecurity::for_current_user_and_system()
        .context("build daemon query named pipe security descriptor")?;
    let security_attributes = pipe_security
        .attributes()
        .context("build daemon query named pipe security attributes")?;
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
            &security_attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("create daemon query named pipe {pipe_name}"));
    }
    let pipe = WindowsDaemonQueryPipe { handle };
    pipe_security
        .verify_handle(pipe.handle)
        .context("verify daemon query named pipe security descriptor")?;
    Ok(pipe)
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

#[cfg(not(any(unix, windows)))]
pub(in crate::semantic) fn start_daemon_source_refresh_service(
    _data_root: &Path,
    _runtime: SharedSemanticRuntime,
    _wakeup: Arc<DaemonWakeup>,
) -> Result<DaemonQueryService> {
    Err(anyhow!(
        "daemon source refresh service is not supported on this platform"
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceHydrationBatchItem {
    event_identity: StableEntityId,
    locator: SourceRecordLocator,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SourceHydrationMode {
    SearchDisplay { max_chars: usize },
    Complete,
}

struct SourceHydrationGroup {
    first_position: usize,
    positions: Vec<usize>,
    requests: Vec<EventHydrationRequest>,
}

struct SourceHydrationWork {
    first_position: usize,
    positions: Vec<usize>,
    requests: Vec<EventHydrationRequest>,
    reservation_bytes: usize,
}

struct HydratedSourceItem {
    position: usize,
    event_id: StableEntityId,
    text: String,
}

enum SourceHydrationWorkFailure {
    Budget(HydrationBudgetError),
    Resolver(HydrationFailure),
}

pub(in crate::semantic) fn handle_source_hydration_batch(
    data_root: &Path,
    source_refresh: &SourceBackedRefreshCoordinator,
    request: &Value,
) -> Value {
    let generation_id = request
        .get("generation_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !valid_source_generation_id(generation_id) {
        return source_hydration_protocol_failure(
            "invalid_generation",
            "invalid_locator",
            "source hydration generation ID must be a lowercase SHA-256 digest",
            false,
        );
    }
    let retained = match source_refresh.resolver_for_generation(data_root, generation_id) {
        Ok(retained) => retained,
        Err(error @ SourceBackedResolverAccessError::Missing { .. }) => {
            return source_hydration_protocol_failure(
                "resolver_generation_unavailable",
                "temporarily_unavailable",
                &error.to_string(),
                source_refresh.has_pending_request(),
            );
        }
        Err(error @ SourceBackedResolverAccessError::GenerationMismatch { .. }) => {
            return source_hydration_protocol_failure(
                "resolver_generation_mismatch",
                "stale_source_evidence",
                &error.to_string(),
                source_refresh.has_pending_request(),
            );
        }
    };
    if retained.generation_id() != generation_id {
        return source_hydration_protocol_failure(
            "resolver_generation_mismatch",
            "stale_source_evidence",
            "daemon resolver changed while accepting the hydration batch",
            true,
        );
    }
    handle_source_hydration_batch_with(request, generation_id, retained.resolver(), |failure| {
        source_refresh.handle_hydration_failure(data_root, generation_id, failure.clone());
        source_refresh.has_pending_request()
    })
}

pub(in crate::semantic) fn handle_source_hydration_batch_with<R, Refresh>(
    request: &Value,
    retained_generation_id: &str,
    resolver: &R,
    refresh: Refresh,
) -> Value
where
    R: ContentSourceResolver + Sync,
    Refresh: Fn(&HydrationFailure) -> bool,
{
    handle_source_hydration_batch_with_budget(
        request,
        retained_generation_id,
        resolver,
        refresh,
        DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
    )
    .0
}

fn handle_source_hydration_batch_with_budget<R, Refresh>(
    request: &Value,
    retained_generation_id: &str,
    resolver: &R,
    refresh: Refresh,
    budget_limit: usize,
) -> (Value, HydrationBudgetSnapshot)
where
    R: ContentSourceResolver + Sync,
    Refresh: Fn(&HydrationFailure) -> bool,
{
    let generation_id = request
        .get("generation_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if generation_id != retained_generation_id {
        return (
            source_hydration_protocol_failure(
                "resolver_generation_mismatch",
                "stale_source_evidence",
                &format!(
                    "requested source generation {generation_id:?}, retained {retained_generation_id:?}"
                ),
                false,
            ),
            HydrationBudgetSnapshot::default(),
        );
    }
    let budget = HydrationByteBudget::new(budget_limit);
    let response = (|| {
        let mode = match source_hydration_mode(request) {
            Ok(mode) => mode,
            Err(error) => {
                return source_hydration_protocol_failure(
                    "invalid_request",
                    "invalid_locator",
                    &format!("{error:#}"),
                    false,
                );
            }
        };
        let Some(values) = request.get("items").and_then(Value::as_array) else {
            return source_hydration_protocol_failure(
                "invalid_request",
                "invalid_locator",
                "source hydration request has no item array",
                false,
            );
        };
        if values.is_empty() || values.len() > DAEMON_SOURCE_HYDRATION_MAX_ITEMS {
            return source_hydration_protocol_failure(
                "item_limit",
                "invalid_locator",
                &format!(
                    "source hydration batch has {} items; expected 1..={DAEMON_SOURCE_HYDRATION_MAX_ITEMS}",
                    values.len()
                ),
                false,
            );
        }
        let mut requests = Vec::with_capacity(values.len());
        for value in values {
            let item: SourceHydrationBatchItem = match serde_json::from_value(value.clone()) {
                Ok(item) => item,
                Err(error) => {
                    return source_hydration_protocol_failure(
                        "invalid_request",
                        "invalid_locator",
                        &format!("decode typed source hydration item: {error}"),
                        false,
                    );
                }
            };
            let request =
                match EventHydrationRequest::new(item.event_identity, item.locator.clone()) {
                    Ok(request) => request,
                    Err(error) => {
                        return source_hydration_protocol_failure(
                            "invalid_request",
                            "invalid_locator",
                            &format!("validate source hydration locator: {error}"),
                            false,
                        );
                    }
                };
            requests.push(request);
        }
        let batch = match BatchHydrationRequest::new(requests) {
            Ok(batch) => batch,
            Err(error) => {
                return source_hydration_protocol_failure(
                    "invalid_request",
                    "invalid_locator",
                    &format!("validate ordered source hydration batch: {error}"),
                    false,
                )
            }
        };
        let envelope_charge = match successful_response_envelope_charge(retained_generation_id) {
            Ok(charge) => charge,
            Err(error) => return source_hydration_budget_failure(error),
        };
        if let Err(error) = budget.charge_retained(envelope_charge) {
            return source_hydration_budget_failure(error);
        }

        let mut grouped =
            BTreeMap::<[u8; 32], (usize, Vec<usize>, Vec<EventHydrationRequest>)>::new();
        for (position, request) in batch.events().iter().enumerate() {
            let key = request.locator().source().exact_descriptor_digest();
            let group = grouped
                .entry(key)
                .or_insert_with(|| (position, Vec::new(), Vec::new()));
            group.1.push(position);
            group.2.push(request.clone());
        }
        let groups = grouped
            .into_values()
            .map(
                |(first_position, positions, requests)| SourceHydrationGroup {
                    first_position,
                    positions,
                    requests,
                },
            )
            .collect::<Vec<_>>();
        let work = match plan_source_hydration_work(groups, mode, &budget) {
            Ok(work) => work,
            Err(error) => return source_hydration_budget_failure(error),
        };
        let next = AtomicUsize::new(0);
        let results = Mutex::new(Vec::<(
            usize,
            std::result::Result<Vec<HydratedSourceItem>, SourceHydrationWorkFailure>,
        )>::with_capacity(work.len()));
        std::thread::scope(|scope| {
            for _ in 0..work.len().min(DAEMON_SOURCE_HYDRATION_MAX_WORKERS) {
                scope.spawn(|| loop {
                    if budget.is_cancelled() {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = work.get(index) else {
                        break;
                    };
                    if budget.is_cancelled() {
                        break;
                    }
                    let result = hydrate_source_work(resolver, item, mode, &budget);
                    if result.is_err() {
                        budget.cancel();
                    }
                    results
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push((item.first_position, result));
                });
            }
        });
        let mut results = results
            .into_inner()
            .unwrap_or_else(|error| error.into_inner());
        results.sort_by_key(|(position, _)| *position);
        if let Some(failure) = results.iter().find_map(|(_, result)| match result {
            Err(SourceHydrationWorkFailure::Resolver(failure)) => Some(failure),
            _ => None,
        }) {
            let refresh_scheduled = refresh(failure);
            return source_hydration_protocol_failure(
                "source_hydration_failed",
                hydration_failure_kind_name(failure.kind),
                &failure.detail,
                refresh_scheduled,
            );
        }
        let budget_failed = results.iter().any(|(_, result)| {
            matches!(
                result,
                Err(SourceHydrationWorkFailure::Budget(
                    HydrationBudgetError::Cancelled | HydrationBudgetError::Exhausted
                ))
            )
        });
        if budget_failed || results.len() != work.len() || budget.snapshot().exhausted {
            return source_hydration_budget_failure(HydrationBudgetError::Exhausted);
        }

        let mut ordered = (0..batch.len())
            .map(|_| None)
            .collect::<Vec<Option<HydratedSourceItem>>>();
        for (_, result) in results {
            let Ok(items) = result else {
                continue;
            };
            for item in items {
                let Some(slot) = ordered.get_mut(item.position) else {
                    return source_hydration_protocol_failure(
                        "invalid_resolver_response",
                        "invalid_locator",
                        "source resolver returned an out-of-range event",
                        false,
                    );
                };
                if slot.replace(item).is_some() {
                    return source_hydration_protocol_failure(
                        "invalid_resolver_response",
                        "invalid_locator",
                        "source resolver returned a duplicate event",
                        false,
                    );
                }
            }
        }
        let mut response_items = Vec::with_capacity(batch.len());
        for (request, slot) in batch.events().iter().zip(&mut ordered) {
            let Some(item) = slot.take() else {
                return source_hydration_protocol_failure(
                    "invalid_resolver_response",
                    "missing_record",
                    &format!(
                        "source resolver omitted requested event {}",
                        request.event_id()
                    ),
                    false,
                );
            };
            if item.event_id != request.event_id() {
                return source_hydration_protocol_failure(
                    "invalid_resolver_response",
                    "invalid_locator",
                    "source resolver returned a mismatched event",
                    false,
                );
            }
            response_items.push(json!({
                "event_id": item.event_id.as_uuid(),
                "text": item.text,
            }));
        }
        compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "generation_id": retained_generation_id,
            "items": response_items,
        }))
    })();
    (response, budget.snapshot())
}

fn plan_source_hydration_work(
    groups: Vec<SourceHydrationGroup>,
    mode: SourceHydrationMode,
    budget: &HydrationByteBudget,
) -> std::result::Result<Vec<SourceHydrationWork>, HydrationBudgetError> {
    let display_copy_bytes = match mode {
        SourceHydrationMode::SearchDisplay { max_chars } => max_chars
            .checked_mul(4)
            .ok_or(HydrationBudgetError::Exhausted)?,
        SourceHydrationMode::Complete => 0,
    };
    let max_reservation = budget.available_when_idle();
    let mut work = Vec::new();
    for group in groups {
        let mut positions = Vec::new();
        let mut requests = Vec::new();
        let mut reservation_bytes = 0_usize;
        for (position, request) in group.positions.into_iter().zip(group.requests) {
            let item_bytes = provider_read_reservation_bytes(&request, display_copy_bytes)?;
            if item_bytes > max_reservation {
                return Err(HydrationBudgetError::Exhausted);
            }
            let next = reservation_bytes
                .checked_add(item_bytes)
                .ok_or(HydrationBudgetError::Exhausted)?;
            if !requests.is_empty() && next > max_reservation {
                work.push(SourceHydrationWork {
                    first_position: positions[0],
                    positions,
                    requests,
                    reservation_bytes,
                });
                positions = Vec::new();
                requests = Vec::new();
                reservation_bytes = 0;
            }
            reservation_bytes = reservation_bytes
                .checked_add(item_bytes)
                .ok_or(HydrationBudgetError::Exhausted)?;
            positions.push(position);
            requests.push(request);
        }
        if !requests.is_empty() {
            work.push(SourceHydrationWork {
                first_position: positions.first().copied().unwrap_or(group.first_position),
                positions,
                requests,
                reservation_bytes,
            });
        }
    }
    work.sort_by_key(|item| item.first_position);
    Ok(work)
}

fn hydrate_source_work(
    resolver: &impl ContentSourceResolver,
    work: &SourceHydrationWork,
    mode: SourceHydrationMode,
    budget: &HydrationByteBudget,
) -> std::result::Result<Vec<HydratedSourceItem>, SourceHydrationWorkFailure> {
    let reservation = budget
        .reserve(work.reservation_bytes)
        .map_err(SourceHydrationWorkFailure::Budget)?;
    let request = BatchHydrationRequest::new(work.requests.clone()).map_err(|error| {
        SourceHydrationWorkFailure::Resolver(HydrationFailure {
            kind: HydrationFailureKind::InvalidLocator,
            detail: format!("validate grouped source hydration request: {error}"),
        })
    })?;
    let result = resolver
        .hydrate_batch(&request)
        .map_err(SourceHydrationWorkFailure::Resolver)?;
    result
        .validate_for_request(&request)
        .map_err(SourceHydrationWorkFailure::Resolver)?;
    let mut retained_bytes = 0_usize;
    let items = request
        .events()
        .iter()
        .zip(&work.positions)
        .zip(result.into_records())
        .map(|((expected, position), record)| {
            if record.event_id != expected.event_id() {
                return Err(SourceHydrationWorkFailure::Resolver(HydrationFailure {
                    kind: HydrationFailureKind::InvalidLocator,
                    detail: format!(
                        "source resolver reordered event {} as {}",
                        expected.event_id(),
                        record.event_id
                    ),
                }));
            }
            if !provider_body_is_admitted(
                expected,
                record.provider_bytes.len(),
                record.provider_bytes.capacity(),
            ) {
                return Err(SourceHydrationWorkFailure::Budget(
                    HydrationBudgetError::Exhausted,
                ));
            }
            let text = String::from_utf8(record.provider_bytes).map_err(|error| {
                SourceHydrationWorkFailure::Resolver(HydrationFailure {
                    kind: HydrationFailureKind::UnsupportedParserRevision,
                    detail: format!(
                        "source resolver returned non-UTF-8 display content for {}: {}",
                        expected.event_id(),
                        error.utf8_error()
                    ),
                })
            })?;
            if text.is_empty() {
                return Err(SourceHydrationWorkFailure::Resolver(HydrationFailure {
                    kind: HydrationFailureKind::MissingRecord,
                    detail: format!(
                        "source resolver returned empty display content for {}",
                        expected.event_id()
                    ),
                }));
            }
            let text = match mode {
                SourceHydrationMode::SearchDisplay { max_chars } => {
                    bounded_search_display_text(text, max_chars)
                }
                SourceHydrationMode::Complete => text,
            };
            retained_bytes = retained_bytes
                .checked_add(
                    retained_response_item_charge(&text)
                        .map_err(SourceHydrationWorkFailure::Budget)?,
                )
                .ok_or(SourceHydrationWorkFailure::Budget(
                    HydrationBudgetError::Exhausted,
                ))?;
            Ok(HydratedSourceItem {
                position: *position,
                event_id: record.event_id,
                text,
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    reservation
        .commit(retained_bytes, items.len())
        .map_err(SourceHydrationWorkFailure::Budget)?;
    Ok(items)
}

fn bounded_search_display_text(text: String, max_chars: usize) -> String {
    let end = text
        .char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(index, _)| index);
    let mut bounded = String::with_capacity(end);
    bounded.push_str(&text[..end]);
    bounded
}

fn source_hydration_budget_failure(_error: HydrationBudgetError) -> Value {
    source_hydration_protocol_failure(
        "response_limit",
        "temporarily_unavailable",
        "source hydration response exceeds the daemon byte cap",
        false,
    )
}

fn source_hydration_mode(request: &Value) -> Result<SourceHydrationMode> {
    match request.get("mode").and_then(Value::as_str) {
        Some("search_display") => {
            let max_chars = request
                .get("max_chars")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| (1..=2_048).contains(value))
                .ok_or_else(|| anyhow!("search display max_chars must be between 1 and 2048"))?;
            Ok(SourceHydrationMode::SearchDisplay { max_chars })
        }
        Some("complete") if request.get("max_chars").is_none_or(Value::is_null) => {
            Ok(SourceHydrationMode::Complete)
        }
        Some(mode) => Err(anyhow!("invalid source hydration mode `{mode}`")),
        None => Err(anyhow!("source hydration mode is missing")),
    }
}

fn valid_source_generation_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn source_hydration_protocol_failure(
    code: &str,
    failure_kind: &str,
    detail: &str,
    refresh_scheduled: bool,
) -> Value {
    compact_json(json!({
        "ok": false,
        "schema_version": 1,
        "code": code,
        "failure_kind": failure_kind,
        "detail": detail,
        "refresh_scheduled": refresh_scheduled,
    }))
}

fn hydration_failure_kind_name(kind: HydrationFailureKind) -> &'static str {
    match kind {
        HydrationFailureKind::TemporarilyUnavailable => "temporarily_unavailable",
        HydrationFailureKind::ConfirmedDeleted => "confirmed_deleted",
        HydrationFailureKind::StaleSourceEvidence => "stale_source_evidence",
        HydrationFailureKind::StaleRecordEvidence => "stale_record_evidence",
        HydrationFailureKind::MissingRecord => "missing_record",
        HydrationFailureKind::UnsupportedParserRevision => "unsupported_parser_revision",
        HydrationFailureKind::InvalidLocator => "invalid_locator",
    }
}

pub(in crate::semantic) fn handle_daemon_query_stream<S: std::io::Write>(
    data_root: &Path,
    runtime: &SharedSemanticRuntime,
    source_refresh: &SourceBackedRefreshCoordinator,
    service: DaemonIpcService,
    token: &str,
    mut stream: S,
    request: Result<String>,
    wakeup: Option<&DaemonWakeup>,
) {
    let result = request.and_then(|body| {
        handle_daemon_query_stream_inner(
            data_root,
            runtime,
            source_refresh,
            service,
            token,
            &mut stream,
            &body,
            wakeup,
        )
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
    source_refresh: &SourceBackedRefreshCoordinator,
    service: DaemonIpcService,
    token: &str,
    stream: &mut S,
    body: &str,
    wakeup: Option<&DaemonWakeup>,
) -> Result<()> {
    let request: Value = serde_json::from_str(body).context("parse daemon query request")?;
    if request.get("token").and_then(Value::as_str) != Some(token) {
        return Err(anyhow!("daemon query authentication failed"));
    }
    let op = request.get("op").and_then(Value::as_str).unwrap_or("");
    if service == DaemonIpcService::SourceRefresh {
        if op == "source_hydrate_batch" {
            let response = handle_source_hydration_batch(data_root, source_refresh, &request);
            writeln!(stream, "{}", serde_json::to_string(&response)?)?;
            return Ok(());
        }
        if let Some(response) = source_refresh.handle_ipc_request(data_root, &request)? {
            writeln!(stream, "{}", serde_json::to_string(&response)?)?;
            return Ok(());
        }
        if op == "ping" {
            let published_generation = read_daemon_job_status(
                &daemon_source_backed_refresh_job_path(data_root),
            )
            .and_then(|job| {
                job.get("published_generation")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&compact_json(json!({
                    "ok": true,
                    "schema_version": 1,
                    "owner": "daemon",
                    "service": "source_refresh",
                    "pid": std::process::id(),
                    "published_generation": published_generation,
                })))?
            )?;
            return Ok(());
        }
        if op == "shutdown" {
            let config = crate::config::AppConfig::load(data_root)?;
            if config.daemon.enabled {
                return Err(anyhow!("daemon shutdown requires [daemon] enabled = false"));
            }
            let wakeup = wakeup.ok_or_else(|| anyhow!("daemon shutdown wakeup is unavailable"))?;
            wakeup.signal_shutdown();
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&compact_json(json!({
                    "ok": true,
                    "schema_version": 1,
                    "owner": "daemon",
                    "service": "source_refresh",
                    "shutdown": "accepted",
                    "pid": std::process::id(),
                })))?
            )?;
            return Ok(());
        }
        if op == "lifecycle_wakeup" {
            let wakeup = wakeup.ok_or_else(|| anyhow!("daemon lifecycle wakeup is unavailable"))?;
            wakeup.signal_ipc();
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&compact_json(json!({
                    "ok": true,
                    "schema_version": 1,
                    "owner": "daemon",
                    "service": "source_refresh",
                    "lifecycle_wakeup": "accepted",
                    "pid": std::process::id(),
                })))?
            )?;
            return Ok(());
        }
        if op == "supervisor_handoff" {
            let config = crate::config::AppConfig::load(data_root)?;
            if !config.daemon.enabled {
                return Err(anyhow!(
                    "native-supervisor handoff requires an enabled daemon"
                ));
            }
            let wakeup =
                wakeup.ok_or_else(|| anyhow!("daemon supervisor handoff wakeup is unavailable"))?;
            wakeup.signal_shutdown();
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&compact_json(json!({
                    "ok": true,
                    "schema_version": 1,
                    "owner": "daemon",
                    "service": "source_refresh",
                    "supervisor_handoff": "accepted",
                    "pid": std::process::id(),
                })))?
            )?;
            return Ok(());
        }
        return Err(anyhow!("unknown daemon source refresh operation `{op}`"));
    }
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

#[cfg(test)]
mod source_hydration_tests {
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Mutex,
        },
        time::Duration,
    };

    use ctx_history_core::{
        derive_event_id, derive_session_id, BatchHydrationResult, EventIdentityInput,
        HydratedProviderRecord, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
        NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
    };

    use crate::semantic::query_service::hydration_budget::SOURCE_HYDRATION_MAX_ITEM_BYTES;

    use super::*;

    const GENERATION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct FixtureLocator {
        event_id: StableEntityId,
        locator: SourceRecordLocator,
    }

    fn fixture(lineage: u8, sequence: u64) -> FixtureLocator {
        fixture_with_coordinate(
            lineage,
            sequence,
            "fixture_jsonl",
            NativeRecordCoordinate::ProviderNative {
                namespace: "fixture".to_owned(),
                coordinate: TypedKey::U64(sequence),
            },
        )
    }

    fn jsonl_fixture(lineage: u8, sequence: u64, byte_length: usize) -> FixtureLocator {
        fixture_with_coordinate(
            lineage,
            sequence,
            "fixture_jsonl",
            NativeRecordCoordinate::Jsonl {
                byte_offset: sequence.saturating_mul(SOURCE_HYDRATION_MAX_ITEM_BYTES as u64),
                byte_length: byte_length as u64,
                physical_ordinal: sequence,
                native_session_key: None,
                native_event_key: None,
            },
        )
    }

    fn sqlite_fixture(lineage: u8, sequence: u64) -> FixtureLocator {
        fixture_with_coordinate(
            lineage,
            sequence,
            "fixture_sqlite",
            NativeRecordCoordinate::ProviderSqlite {
                logical_relation: "fixture_messages".to_owned(),
                primary_key: TypedKey::U64(sequence),
                row_version: Some(TypedKey::U64(sequence)),
            },
        )
    }

    fn fixture_with_coordinate(
        lineage: u8,
        sequence: u64,
        source_format: &str,
        coordinate: NativeRecordCoordinate,
    ) -> FixtureLocator {
        let source = SourceKey::derive(
            "codex",
            source_format,
            "fixture",
            1,
            SourceAnchor::CatalogLineage([lineage; 32]),
        )
        .unwrap();
        let native_session_key = NativeSessionKey::native_id(
            "session",
            TypedKey::utf8(format!("session-{lineage}")).unwrap(),
        )
        .unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &native_session_key,
        })
        .unwrap();
        let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        let locator = SourceRecordLocator::new(
            source,
            coordinate,
            LocatorRevisionPolicy::ExactSourceRevision,
            Some([lineage; 32]),
            [sequence as u8; 32],
        )
        .unwrap();
        FixtureLocator { event_id, locator }
    }

    fn request(items: &[&FixtureLocator], mode: &str, max_chars: Option<usize>) -> Value {
        json!({
            "schema_version": 1,
            "op": "source_hydrate_batch",
            "generation_id": GENERATION,
            "mode": mode,
            "max_chars": max_chars,
            "items": items.iter().map(|item| json!({
                "event_identity": item.event_id,
                "locator": item.locator,
            })).collect::<Vec<_>>(),
        })
    }

    #[derive(Default)]
    struct MockResolver {
        bodies: HashMap<StableEntityId, Vec<u8>>,
        body_sizes: HashMap<StableEntityId, usize>,
        batch_calls: Mutex<Vec<Vec<StableEntityId>>>,
        failure: Option<HydrationFailure>,
        delayed: bool,
        active: AtomicUsize,
        max_active: AtomicUsize,
        allocated_items: AtomicUsize,
        allocated_bytes: AtomicUsize,
    }

    impl MockResolver {
        fn with_body(mut self, item: &FixtureLocator, text: impl Into<Vec<u8>>) -> Self {
            self.bodies.insert(item.event_id, text.into());
            self
        }

        fn with_body_size(mut self, item: &FixtureLocator, bytes: usize) -> Self {
            self.body_sizes.insert(item.event_id, bytes);
            self
        }

        fn with_failure(mut self, kind: HydrationFailureKind, detail: &str) -> Self {
            self.failure = Some(HydrationFailure {
                kind,
                detail: detail.to_owned(),
            });
            self
        }

        fn with_delay(mut self) -> Self {
            self.delayed = true;
            self
        }
    }

    impl ContentSourceResolver for MockResolver {
        fn hydrate_event(
            &self,
            request: &EventHydrationRequest,
        ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
            if let Some(failure) = self.failure.as_ref() {
                return Err(failure.clone());
            }
            let provider_bytes = if let Some(body) = self.bodies.get(&request.event_id()) {
                body.clone()
            } else if let Some(bytes) = self.body_sizes.get(&request.event_id()).copied() {
                self.allocated_items.fetch_add(1, Ordering::SeqCst);
                self.allocated_bytes.fetch_add(bytes, Ordering::SeqCst);
                vec![b'x'; bytes]
            } else {
                return Err(HydrationFailure {
                    kind: HydrationFailureKind::MissingRecord,
                    detail: "fixture body is absent".to_owned(),
                });
            };
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes,
            })
        }

        fn hydrate_batch(
            &self,
            request: &BatchHydrationRequest,
        ) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
            if self.delayed {
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_active.fetch_max(active, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(10));
            }
            self.batch_calls.lock().unwrap().push(
                request
                    .events()
                    .iter()
                    .map(|event| event.event_id())
                    .collect(),
            );
            let result = (|| {
                let records = request
                    .events()
                    .iter()
                    .map(|event| self.hydrate_event(event))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let result =
                    BatchHydrationResult::new(records).map_err(|error| HydrationFailure {
                        kind: HydrationFailureKind::InvalidLocator,
                        detail: error.to_string(),
                    })?;
                result.validate_for_request(request)?;
                Ok(result)
            })();
            if self.delayed {
                self.active.fetch_sub(1, Ordering::SeqCst);
            }
            result
        }
    }

    #[test]
    fn source_hydration_groups_by_exact_source_and_restores_request_order() {
        let first = fixture(1, 1);
        let second_source = fixture(2, 2);
        let third = fixture(1, 3);
        let resolver = MockResolver::default()
            .with_body(&first, "first")
            .with_body(&second_source, "second")
            .with_body(&third, "third");

        let response = handle_source_hydration_batch_with(
            &request(&[&second_source, &first, &third], "complete", None),
            GENERATION,
            &resolver,
            |_| false,
        );

        assert_eq!(response["ok"], true);
        assert_eq!(
            response["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["event_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                second_source.event_id.as_uuid().to_string(),
                first.event_id.as_uuid().to_string(),
                third.event_id.as_uuid().to_string(),
            ]
        );
        assert_eq!(
            response["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["second", "first", "third"]
        );
        let mut call_sizes = resolver
            .batch_calls
            .into_inner()
            .unwrap()
            .into_iter()
            .map(|call| call.len())
            .collect::<Vec<_>>();
        call_sizes.sort_unstable();
        assert_eq!(call_sizes, vec![1, 2]);
    }

    #[test]
    fn source_search_hydration_truncates_exact_content_by_character() {
        let item = fixture(3, 1);
        let resolver = MockResolver::default().with_body(&item, "αβγδ");
        let response = handle_source_hydration_batch_with(
            &request(&[&item], "search_display", Some(3)),
            GENERATION,
            &resolver,
            |_| false,
        );

        assert_eq!(response["ok"], true);
        assert_eq!(response["items"][0]["text"], "αβγ");
    }

    #[test]
    fn source_hydration_unknown_size_workers_share_one_aggregate_budget() {
        let items = (1..=9)
            .map(|lineage| fixture(lineage, 1))
            .collect::<Vec<_>>();
        let resolver = items
            .iter()
            .fold(MockResolver::default().with_delay(), |resolver, item| {
                resolver.with_body(item, format!("source {}", item.event_id))
            });
        let references = items.iter().collect::<Vec<_>>();
        let (response, snapshot) = handle_source_hydration_batch_with_budget(
            &request(&references, "complete", None),
            GENERATION,
            &resolver,
            |_| false,
            DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
        );

        assert_eq!(response["ok"], true);
        assert!((2..=3).contains(&resolver.max_active.load(Ordering::SeqCst)));
        assert_eq!(resolver.batch_calls.into_inner().unwrap().len(), 9);
        assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
        assert_eq!(snapshot.committed_items, 9);
    }

    #[test]
    fn source_hydration_without_resident_generation_is_typed_and_queues_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let item = fixture(10, 1);
        let coordinator = SourceBackedRefreshCoordinator::new();
        let response = handle_source_hydration_batch(
            temp.path(),
            &coordinator,
            &request(&[&item], "complete", None),
        );

        assert_eq!(response["ok"], false);
        assert_eq!(response["code"], "resolver_generation_unavailable");
        assert_eq!(response["failure_kind"], "temporarily_unavailable");
        assert_eq!(response["refresh_scheduled"], true);
        assert!(coordinator.has_pending_request());
    }

    #[test]
    fn source_hydration_preserves_typed_stale_failure_and_refresh_signal() {
        let item = fixture(4, 1);
        let refresh_called = AtomicBool::new(false);
        let resolver = MockResolver::default().with_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "fixture source revision changed",
        );
        let response = handle_source_hydration_batch_with(
            &request(&[&item], "complete", None),
            GENERATION,
            &resolver,
            |_| {
                refresh_called.store(true, Ordering::Relaxed);
                true
            },
        );

        assert_eq!(response["ok"], false);
        assert_eq!(response["code"], "source_hydration_failed");
        assert_eq!(response["failure_kind"], "stale_source_evidence");
        assert_eq!(response["refresh_scheduled"], true);
        assert!(refresh_called.load(Ordering::Relaxed));
    }

    #[test]
    fn source_hydration_rejects_generation_mismatch_before_resolver_access() {
        let item = fixture(5, 1);
        let resolver = MockResolver::default().with_body(&item, "body");
        let response = handle_source_hydration_batch_with(
            &request(&[&item], "complete", None),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &resolver,
            |_| false,
        );

        assert_eq!(response["ok"], false);
        assert_eq!(response["code"], "resolver_generation_mismatch");
        assert_eq!(response["failure_kind"], "stale_source_evidence");
        assert!(resolver.batch_calls.into_inner().unwrap().is_empty());
    }

    #[test]
    fn source_hydration_rejects_empty_content_instead_of_emitting_a_placeholder() {
        let item = fixture(6, 1);
        let resolver = MockResolver::default().with_body(&item, Vec::new());
        let response = handle_source_hydration_batch_with(
            &request(&[&item], "complete", None),
            GENERATION,
            &resolver,
            |_| false,
        );

        assert_eq!(response["ok"], false);
        assert_eq!(response["failure_kind"], "missing_record");
    }

    #[test]
    fn source_hydration_ordinary_jsonl_and_grouped_sqlite_batch_preserves_parity() {
        let jsonl = (0..64)
            .map(|sequence| jsonl_fixture(20, sequence, 64))
            .collect::<Vec<_>>();
        let sqlite = (0..64)
            .map(|sequence| sqlite_fixture(21, sequence))
            .collect::<Vec<_>>();
        let mut ordered = Vec::with_capacity(128);
        for index in 0..64 {
            ordered.push(&jsonl[index]);
            ordered.push(&sqlite[index]);
        }
        let resolver = ordered
            .iter()
            .enumerate()
            .fold(MockResolver::default(), |resolver, (index, item)| {
                resolver.with_body(item, format!("ordinary-{index}"))
            });
        let (response, snapshot) = handle_source_hydration_batch_with_budget(
            &request(&ordered, "complete", None),
            GENERATION,
            &resolver,
            |_| false,
            DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
        );

        assert_eq!(response["ok"], true);
        assert_eq!(response["items"].as_array().unwrap().len(), 128);
        assert!(serde_json::to_vec(&response).unwrap().len() <= snapshot.retained_bytes);
        assert_eq!(
            response["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            (0..128)
                .map(|index| format!("ordinary-{index}"))
                .collect::<Vec<_>>()
        );
        let call_sizes = resolver
            .batch_calls
            .into_inner()
            .unwrap()
            .into_iter()
            .map(|call| call.len())
            .collect::<Vec<_>>();
        assert!(call_sizes.contains(&64));
        assert!(call_sizes.iter().filter(|size| **size <= 3).count() >= 22);
        assert_eq!(snapshot.committed_items, 128);
        assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
        assert_eq!(snapshot.in_flight_bytes, 0);
    }

    #[test]
    fn source_hydration_jsonl_reservation_allows_certified_record_framing() {
        let item = jsonl_fixture(25, 0, SOURCE_HYDRATION_MAX_ITEM_BYTES + 2);
        let resolver = MockResolver::default().with_body(&item, "framed");
        let (response, snapshot) = handle_source_hydration_batch_with_budget(
            &request(&[&item], "complete", None),
            GENERATION,
            &resolver,
            |_| false,
            DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
        );

        assert_eq!(response["ok"], true);
        assert_eq!(response["items"][0]["text"], "framed");
        assert_eq!(snapshot.committed_items, 1);
        assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
    }

    #[test]
    fn source_hydration_exact_read_boundary_passes_and_next_byte_never_reaches_provider() {
        let exact = jsonl_fixture(26, 0, 100);
        let exact_request =
            EventHydrationRequest::new(exact.event_id, exact.locator.clone()).unwrap();
        let budget_limit = successful_response_envelope_charge(GENERATION).unwrap()
            + provider_read_reservation_bytes(&exact_request, 0).unwrap();
        let exact_resolver = MockResolver::default().with_body(&exact, "x");
        let (exact_response, exact_snapshot) = handle_source_hydration_batch_with_budget(
            &request(&[&exact], "complete", None),
            GENERATION,
            &exact_resolver,
            |_| false,
            budget_limit,
        );

        assert_eq!(exact_response["ok"], true);
        assert_eq!(exact_snapshot.peak_bytes, budget_limit);
        assert_eq!(exact_resolver.batch_calls.into_inner().unwrap().len(), 1);

        let over = jsonl_fixture(27, 0, 101);
        let over_resolver = MockResolver::default().with_body(&over, "x");
        let (over_response, over_snapshot) = handle_source_hydration_batch_with_budget(
            &request(&[&over], "complete", None),
            GENERATION,
            &over_resolver,
            |_| false,
            budget_limit,
        );

        assert_eq!(over_response["ok"], false);
        assert_eq!(over_response["code"], "response_limit");
        assert!(over_resolver.batch_calls.into_inner().unwrap().is_empty());
        assert_eq!(over_snapshot.reservations, 0);
    }

    #[test]
    fn source_hydration_128_near_limit_jsonl_items_stop_after_one_bounded_group() {
        let body_bytes = SOURCE_HYDRATION_MAX_ITEM_BYTES - 4 * 1024;
        let items = (0..DAEMON_SOURCE_HYDRATION_MAX_ITEMS as u64)
            .map(|sequence| jsonl_fixture(22, sequence, body_bytes))
            .collect::<Vec<_>>();
        let resolver = items
            .iter()
            .fold(MockResolver::default(), |resolver, item| {
                resolver.with_body_size(item, body_bytes)
            });
        let references = items.iter().collect::<Vec<_>>();
        let (response, snapshot) = handle_source_hydration_batch_with_budget(
            &request(&references, "complete", None),
            GENERATION,
            &resolver,
            |_| false,
            DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
        );

        assert_eq!(response["ok"], false);
        assert_eq!(response["code"], "response_limit");
        assert_eq!(resolver.allocated_items.load(Ordering::SeqCst), 4);
        assert_eq!(
            resolver.allocated_bytes.load(Ordering::SeqCst),
            body_bytes * 4
        );
        assert_eq!(resolver.batch_calls.into_inner().unwrap().len(), 1);
        assert_eq!(snapshot.committed_items, 4);
        assert!(snapshot.exhausted);
        assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
        assert_eq!(snapshot.in_flight_bytes, 0);
        eprintln!("jsonl near-limit budget evidence: {snapshot:?}");
    }

    #[test]
    fn source_hydration_128_near_limit_sqlite_items_use_the_same_group_budget() {
        let body_bytes = SOURCE_HYDRATION_MAX_ITEM_BYTES - 4 * 1024;
        let items = (0..DAEMON_SOURCE_HYDRATION_MAX_ITEMS as u64)
            .map(|sequence| sqlite_fixture(23, sequence))
            .collect::<Vec<_>>();
        let resolver = items
            .iter()
            .fold(MockResolver::default(), |resolver, item| {
                resolver.with_body_size(item, body_bytes)
            });
        let references = items.iter().collect::<Vec<_>>();
        let (response, snapshot) = handle_source_hydration_batch_with_budget(
            &request(&references, "complete", None),
            GENERATION,
            &resolver,
            |_| false,
            DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
        );

        assert_eq!(response["ok"], false);
        assert_eq!(response["code"], "response_limit");
        assert_eq!(resolver.allocated_items.load(Ordering::SeqCst), 3);
        assert_eq!(
            resolver.allocated_bytes.load(Ordering::SeqCst),
            body_bytes * 3
        );
        assert_eq!(resolver.batch_calls.into_inner().unwrap().len(), 1);
        assert_eq!(snapshot.committed_items, 3);
        assert!(snapshot.exhausted);
        assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
        assert_eq!(snapshot.in_flight_bytes, 0);
        eprintln!("sqlite near-limit budget evidence: {snapshot:?}");
    }

    #[test]
    fn source_hydration_mixed_small_and_huge_items_cancel_without_late_allocation() {
        let huge_bytes = SOURCE_HYDRATION_MAX_ITEM_BYTES - 4 * 1024;
        let mut items = Vec::with_capacity(DAEMON_SOURCE_HYDRATION_MAX_ITEMS);
        items.push(jsonl_fixture(24, 0, 32));
        for sequence in 1..=4 {
            items.push(jsonl_fixture(24, sequence, huge_bytes));
        }
        for sequence in 5..DAEMON_SOURCE_HYDRATION_MAX_ITEMS as u64 {
            items.push(jsonl_fixture(24, sequence, 32));
        }
        let resolver =
            items
                .iter()
                .enumerate()
                .fold(MockResolver::default(), |resolver, (index, item)| {
                    resolver.with_body_size(
                        item,
                        if (1..=4).contains(&index) {
                            huge_bytes
                        } else {
                            32
                        },
                    )
                });
        let references = items.iter().collect::<Vec<_>>();
        let (response, snapshot) = handle_source_hydration_batch_with_budget(
            &request(&references, "complete", None),
            GENERATION,
            &resolver,
            |_| false,
            DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
        );

        assert_eq!(response["ok"], false);
        assert_eq!(response["code"], "response_limit");
        assert_eq!(resolver.allocated_items.load(Ordering::SeqCst), 29);
        assert_eq!(
            resolver.allocated_bytes.load(Ordering::SeqCst),
            huge_bytes * 4 + 32 * 25
        );
        assert_eq!(resolver.batch_calls.into_inner().unwrap().len(), 1);
        assert_eq!(snapshot.committed_items, 29);
        assert!(snapshot.cancelled);
        assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
        assert_eq!(snapshot.in_flight_bytes, 0);
        eprintln!("mixed-size budget evidence: {snapshot:?}");
    }
}
