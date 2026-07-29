use std::{path::Path, sync::Arc, time::Duration as StdDuration};

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::{env, fs};

use anyhow::{anyhow, Context, Result};
use uuid::Uuid;

use crate::semantic::{
    daemon_wakeup::DaemonWakeup, health_search::create_private_dir_all,
    model_runtime::SharedSemanticRuntime, paths_status::daemon_root_path,
    source_backed_refresh_coordinator::SourceBackedRefreshCoordinator,
};

#[cfg(unix)]
use crate::semantic::paths_status::daemon_query_socket_path;

#[cfg(unix)]
use super::super::transport::read_daemon_query_request_unix;
#[cfg(windows)]
use super::super::transport::{
    daemon_query_pipe_name, read_daemon_query_request, windows_named_pipe_name_is_local,
    windows_wide_null, WindowsIoDeadline,
};
use super::super::transport::{
    remove_daemon_service_endpoint, write_daemon_service_endpoint, DaemonIpcService,
    DaemonQueryEndpoint,
};
#[cfg(windows)]
use super::windows_security::WindowsDaemonQueryPipeSecurity;
use super::{
    dispatch::handle_daemon_query_stream, DaemonQueryActivity, DaemonQueryService,
    DAEMON_QUERY_REQUEST_MAX_BYTES, DAEMON_QUERY_REQUEST_READ_TIMEOUT,
};

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
