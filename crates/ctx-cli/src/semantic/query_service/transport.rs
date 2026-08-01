#[derive(Debug, Clone, Eq, PartialEq)]
pub(in crate::semantic) enum DaemonQueryEndpoint {
    #[cfg(unix)]
    Unix { path: PathBuf, token: String },
    #[cfg(windows)]
    WindowsNamedPipe { pipe_name: String, token: String },
    #[cfg(not(any(unix, windows)))]
    #[allow(dead_code)]
    Unsupported,
}

#[cfg(unix)]
mod unix_response;
#[cfg(unix)]
pub(in crate::semantic) use unix_response::read_daemon_query_response_unix;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in crate::semantic) enum DaemonIpcService {
    SemanticQuery,
    SourceRefresh,
}

impl DaemonIpcService {
    fn endpoint_file(self) -> &'static str {
        match self {
            Self::SemanticQuery => DAEMON_QUERY_ENDPOINT_FILE,
            Self::SourceRefresh => "source-refresh-endpoint.json",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(in crate::semantic) struct DaemonQueryEndpointIdentity {
    pub(in crate::semantic) endpoint: DaemonQueryEndpoint,
    pub(in crate::semantic) owner_pid: u32,
}

#[derive(Debug)]
pub(in crate::semantic) struct DaemonQueryServiceUnavailable;

impl fmt::Display for DaemonQueryServiceUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "daemon semantic query service is unavailable; run `ctx daemon run --force` in another terminal or retry with `--refresh background`",
        )
    }
}

impl std::error::Error for DaemonQueryServiceUnavailable {}

#[derive(Debug)]
pub(in crate::semantic) struct DaemonSourceRefreshServiceUnavailable;

impl fmt::Display for DaemonSourceRefreshServiceUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("daemon source refresh service is unavailable")
    }
}

impl std::error::Error for DaemonSourceRefreshServiceUnavailable {}

#[derive(Debug)]
pub(in crate::semantic) struct DaemonQueryResponseTooLarge {
    limit: u64,
}

impl DaemonQueryResponseTooLarge {
    pub(in crate::semantic) fn new(limit: u64) -> Self {
        Self { limit }
    }
}

impl fmt::Display for DaemonQueryResponseTooLarge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "daemon query response exceeded its {}-byte transport limit",
            self.limit
        )
    }
}

impl std::error::Error for DaemonQueryResponseTooLarge {}

impl DaemonQueryEndpoint {
    pub(in crate::semantic) fn token(&self) -> &str {
        match self {
            #[cfg(unix)]
            Self::Unix { token, .. } => token,
            #[cfg(windows)]
            Self::WindowsNamedPipe { token, .. } => token,
            #[cfg(not(any(unix, windows)))]
            Self::Unsupported => "",
        }
    }
}

#[cfg(test)]
pub(in crate::semantic) fn daemon_query_endpoint_path(data_root: &Path) -> PathBuf {
    daemon_service_endpoint_path(data_root, DaemonIpcService::SemanticQuery)
}

pub(in crate::semantic) fn daemon_service_endpoint_path(
    data_root: &Path,
    service: DaemonIpcService,
) -> PathBuf {
    daemon_root_path(data_root).join(service.endpoint_file())
}

#[cfg(test)]
pub(in crate::semantic) fn write_daemon_query_endpoint(
    data_root: &Path,
    endpoint: &DaemonQueryEndpoint,
) -> Result<()> {
    write_daemon_service_endpoint(data_root, DaemonIpcService::SemanticQuery, endpoint)
}

pub(in crate::semantic) fn write_daemon_service_endpoint(
    data_root: &Path,
    service: DaemonIpcService,
    endpoint: &DaemonQueryEndpoint,
) -> Result<()> {
    let value = match endpoint {
        #[cfg(unix)]
        DaemonQueryEndpoint::Unix { path, token } => compact_json(json!({
            "schema_version": 1,
            "transport": "unix",
            "path": path,
            "token": token,
            "pid": process::id(),
        })),
        #[cfg(windows)]
        DaemonQueryEndpoint::WindowsNamedPipe { pipe_name, token } => compact_json(json!({
            "schema_version": 1,
            "transport": "windows_named_pipe",
            "pipe_name": pipe_name,
            "token": token,
            "pid": process::id(),
        })),
        #[cfg(not(any(unix, windows)))]
        DaemonQueryEndpoint::Unsupported => {
            return Err(anyhow!(
                "daemon query service is not supported on this platform"
            ));
        }
    };
    write_private_json_file(&daemon_service_endpoint_path(data_root, service), &value)
}

pub(in crate::semantic) fn remove_daemon_service_endpoint(
    data_root: &Path,
    service: DaemonIpcService,
) {
    let _ = fs::remove_file(daemon_service_endpoint_path(data_root, service));
}

#[cfg(test)]
pub(in crate::semantic) fn read_daemon_query_endpoint(
    data_root: &Path,
) -> Result<Option<DaemonQueryEndpoint>> {
    Ok(read_daemon_query_endpoint_identity(data_root)?.map(|identity| identity.endpoint))
}

#[cfg(test)]
pub(in crate::semantic) fn read_daemon_query_endpoint_identity(
    data_root: &Path,
) -> Result<Option<DaemonQueryEndpointIdentity>> {
    read_daemon_service_endpoint_identity(data_root, DaemonIpcService::SemanticQuery)
}

pub(in crate::semantic) fn read_daemon_service_endpoint_identity(
    data_root: &Path,
    service: DaemonIpcService,
) -> Result<Option<DaemonQueryEndpointIdentity>> {
    let path = daemon_service_endpoint_path(data_root, service);
    let Some(parent) = path.parent() else {
        return Err(anyhow!("daemon query endpoint has no parent directory"));
    };
    match verify_private_directory(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("verify daemon query directory {}", parent.display()));
        }
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    #[cfg(windows)]
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("open daemon query endpoint {}", path.display()));
        }
    };
    #[cfg(windows)]
    verify_private_file_handle(&file)
        .with_context(|| format!("verify daemon query endpoint {}", path.display()))?;
    #[cfg(not(windows))]
    verify_private_file(&path)
        .with_context(|| format!("verify daemon query endpoint {}", path.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .with_context(|| format!("read daemon query endpoint {}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("parse daemon query endpoint {}", path.display()))?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Ok(None);
    }
    read_daemon_query_endpoint_identity_value(value)
}

pub(in crate::semantic) fn read_daemon_query_endpoint_identity_value(
    value: Value,
) -> Result<Option<DaemonQueryEndpointIdentity>> {
    let Some(token) = value
        .get("token")
        .and_then(Value::as_str)
        .filter(|token| token.len() >= 32)
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    let Some(owner_pid) = value
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
    else {
        return Ok(None);
    };
    let endpoint = match value.get("transport").and_then(Value::as_str) {
        #[cfg(unix)]
        Some("unix") => {
            let path = value.get("path").and_then(Value::as_str).map(PathBuf::from);
            path.map(|path| DaemonQueryEndpoint::Unix { path, token })
        }
        #[cfg(windows)]
        Some("windows_named_pipe") => {
            let pipe_name = value
                .get("pipe_name")
                .and_then(Value::as_str)
                .filter(|pipe_name| windows_named_pipe_name_is_local(pipe_name))
                .map(str::to_owned);
            pipe_name.map(|pipe_name| DaemonQueryEndpoint::WindowsNamedPipe { pipe_name, token })
        }
        _ => None,
    };
    Ok(endpoint.map(|endpoint| DaemonQueryEndpointIdentity {
        endpoint,
        owner_pid,
    }))
}

#[cfg(test)]
pub(in crate::semantic) fn remove_daemon_query_endpoint_if_matches(
    data_root: &Path,
    expected: &DaemonQueryEndpointIdentity,
) {
    remove_daemon_service_endpoint_if_matches(data_root, DaemonIpcService::SemanticQuery, expected);
}

pub(in crate::semantic) fn remove_daemon_service_endpoint_if_matches(
    data_root: &Path,
    service: DaemonIpcService,
    expected: &DaemonQueryEndpointIdentity,
) {
    // The daemon owns this guard for the endpoint lifetime. Acquiring it makes
    // the identity re-read and unlink atomic with a replacement daemon start.
    let guard_path = pid_lock_guard_path(&daemon_lock_path(data_root));
    let Ok((guard, _)) = open_or_create_pid_lock_file(&guard_path) else {
        return;
    };
    if secure_private_file_permissions(&guard_path).is_err() {
        return;
    }
    let Ok(true) = try_lock_pid_file(&guard) else {
        return;
    };
    let current = read_daemon_service_endpoint_identity(data_root, service)
        .ok()
        .flatten();
    if current.as_ref() == Some(expected) {
        let _ = fs::remove_file(daemon_service_endpoint_path(data_root, service));
    }
    let _ = fs2::FileExt::unlock(&guard);
}

pub(in crate::semantic) fn daemon_query_request(
    data_root: &Path,
    request: Value,
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<Option<Value>> {
    daemon_service_request(
        data_root,
        DaemonIpcService::SemanticQuery,
        request,
        timeout,
        max_response_bytes,
    )
}

pub(in crate::semantic) fn daemon_source_refresh_request(
    data_root: &Path,
    request: Value,
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<Option<Value>> {
    daemon_service_request(
        data_root,
        DaemonIpcService::SourceRefresh,
        request,
        timeout,
        max_response_bytes,
    )
}

pub(in crate::semantic) fn daemon_service_request(
    data_root: &Path,
    service: DaemonIpcService,
    mut request: Value,
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<Option<Value>> {
    let Some(identity) = read_daemon_service_endpoint_identity(data_root, service)? else {
        return Ok(None);
    };
    let endpoint = &identity.endpoint;
    request["token"] = Value::String(endpoint.token().to_owned());
    let request = format!("{}\n", serde_json::to_string(&compact_json(request))?);
    let body =
        match daemon_query_roundtrip(endpoint, request.as_bytes(), timeout, max_response_bytes) {
            Ok(body) => body,
            Err(error) if daemon_query_roundtrip_error_is_unavailable(endpoint, &error) => {
                remove_daemon_service_endpoint_if_matches(data_root, service, &identity);
                return match service {
                    DaemonIpcService::SemanticQuery => Err(DaemonQueryServiceUnavailable.into()),
                    DaemonIpcService::SourceRefresh => {
                        Err(DaemonSourceRefreshServiceUnavailable.into())
                    }
                };
            }
            Err(error) => return Err(error),
        };
    let response: Value = serde_json::from_str(&body).context("parse daemon query response")?;
    Ok(Some(response))
}

pub(in crate::semantic) fn daemon_query_roundtrip(
    endpoint: &DaemonQueryEndpoint,
    request: &[u8],
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<String> {
    match endpoint {
        #[cfg(unix)]
        DaemonQueryEndpoint::Unix { path, .. } => {
            if max_response_bytes == 0 {
                return Err(DaemonQueryResponseTooLarge::new(0).into());
            }
            let mut stream = UnixStream::connect(path)
                .with_context(|| format!("connect daemon query socket {}", path.display()))?;
            stream
                .set_write_timeout(Some(timeout))
                .context("set daemon query write timeout")?;
            stream
                .write_all(request)
                .context("write daemon query request")?;
            let _ = stream.shutdown(Shutdown::Write);
            let body = read_daemon_query_response_unix(&mut stream, max_response_bytes, timeout)
                .context("read daemon query response")?;
            String::from_utf8(body).context("daemon query response is not UTF-8")
        }
        #[cfg(windows)]
        DaemonQueryEndpoint::WindowsNamedPipe { pipe_name, .. } => {
            daemon_query_roundtrip_windows(pipe_name, request, timeout, max_response_bytes)
        }
        #[cfg(not(any(unix, windows)))]
        DaemonQueryEndpoint::Unsupported => Err(anyhow!(
            "daemon query service is not supported on this platform"
        )),
    }
}

pub(in crate::semantic) fn daemon_query_roundtrip_error_is_unavailable(
    endpoint: &DaemonQueryEndpoint,
    error: &anyhow::Error,
) -> bool {
    let Some(io_error) = error.downcast_ref::<std::io::Error>() else {
        return false;
    };
    match endpoint {
        #[cfg(unix)]
        DaemonQueryEndpoint::Unix { .. } => {
            daemon_query_unix_io_error_is_unavailable(io_error.kind())
        }
        #[cfg(windows)]
        DaemonQueryEndpoint::WindowsNamedPipe { .. } => {
            daemon_query_windows_io_error_is_unavailable(io_error.kind(), io_error.raw_os_error())
        }
        #[cfg(not(any(unix, windows)))]
        DaemonQueryEndpoint::Unsupported => false,
    }
}

#[cfg(any(unix, test))]
pub(in crate::semantic) fn daemon_query_unix_io_error_is_unavailable(
    kind: std::io::ErrorKind,
) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::NotConnected
    )
}

#[cfg(any(windows, test))]
pub(in crate::semantic) fn daemon_query_windows_io_error_is_unavailable(
    kind: std::io::ErrorKind,
    raw_os_error: Option<i32>,
) -> bool {
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PATH_NOT_FOUND: i32 = 3;
    const ERROR_BROKEN_PIPE: i32 = 109;
    const ERROR_BAD_PIPE: i32 = 230;
    const ERROR_NO_DATA: i32 = 232;
    const ERROR_PIPE_NOT_CONNECTED: i32 = 233;

    matches!(
        kind,
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::NotConnected
    ) || matches!(
        raw_os_error,
        Some(
            ERROR_FILE_NOT_FOUND
                | ERROR_PATH_NOT_FOUND
                | ERROR_BROKEN_PIPE
                | ERROR_BAD_PIPE
                | ERROR_NO_DATA
                | ERROR_PIPE_NOT_CONNECTED
        )
    )
}

pub(in crate::semantic) fn read_daemon_query_request<S: std::io::Read>(
    stream: &mut S,
    max_bytes: usize,
) -> Result<String> {
    let mut body = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    while body.len() < max_bytes {
        let read_limit = (max_bytes - body.len()).min(chunk.len());
        let read = stream
            .read(&mut chunk[..read_limit])
            .context("read daemon query request")?;
        if read == 0 {
            break;
        }
        if let Some(newline) = chunk[..read].iter().position(|byte| *byte == b'\n') {
            body.extend_from_slice(&chunk[..newline]);
            return String::from_utf8(body).context("daemon query request is not UTF-8");
        }
        body.extend_from_slice(&chunk[..read]);
    }
    if body.len() >= max_bytes {
        return Err(anyhow!("daemon query request is too large"));
    }
    String::from_utf8(body).context("daemon query request is not UTF-8")
}

#[cfg(unix)]
pub(in crate::semantic) fn read_daemon_query_request_unix(
    stream: &mut UnixStream,
    max_bytes: usize,
    timeout: StdDuration,
) -> Result<String> {
    struct DeadlineReader<'a> {
        stream: &'a mut UnixStream,
        started: Instant,
        timeout: StdDuration,
    }

    impl std::io::Read for DeadlineReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let remaining = self.timeout.saturating_sub(self.started.elapsed());
            if remaining.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "daemon query request read timed out",
                ));
            }
            self.stream.set_read_timeout(Some(remaining))?;
            self.stream.read(buffer).map_err(|error| {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "daemon query request read timed out",
                    )
                } else {
                    error
                }
            })
        }
    }

    read_daemon_query_request(
        &mut DeadlineReader {
            stream,
            started: Instant::now(),
            timeout,
        },
        max_bytes,
    )
}

#[cfg(windows)]
pub(in crate::semantic) fn daemon_query_pipe_name() -> String {
    format!(r"\\.\pipe\ctx-daemon-query-{}", Uuid::new_v4().simple())
}

#[cfg(windows)]
pub(in crate::semantic) fn windows_named_pipe_name_is_local(pipe_name: &str) -> bool {
    pipe_name
        .strip_prefix(r"\\.\pipe\ctx-daemon-query-")
        .is_some_and(|suffix| {
            suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

#[cfg(windows)]
pub(in crate::semantic) fn daemon_query_roundtrip_windows(
    pipe_name: &str,
    request: &[u8],
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<String> {
    if !windows_named_pipe_name_is_local(pipe_name) {
        return Err(anyhow!("daemon query pipe name is not local"));
    }
    if max_response_bytes == 0 {
        return Err(anyhow!(
            "daemon query response limit must be positive for Windows named pipe"
        ));
    }
    let response_limit = usize::try_from(max_response_bytes).ok().ok_or_else(|| {
        anyhow!("daemon query response limit is too large for Windows named pipe")
    })?;
    let deadline = WindowsIoDeadline::new(timeout);
    let pipe_name = windows_wide_null(pipe_name);
    let pipe = open_windows_daemon_query_pipe(&pipe_name, &deadline)?;
    write_all_windows_daemon_query_pipe(&pipe, request, &deadline)?;
    let response = read_windows_daemon_query_pipe(&pipe, response_limit, &deadline)?;
    String::from_utf8(response).context("daemon query response is not UTF-8")
}

#[cfg(windows)]
pub(in crate::semantic) struct WindowsQueryHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for WindowsQueryHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
pub(in crate::semantic) struct WindowsIoDeadline {
    started: std::time::Instant,
    timeout: StdDuration,
}

#[cfg(windows)]
impl WindowsIoDeadline {
    pub(in crate::semantic) fn new(timeout: StdDuration) -> Self {
        Self {
            started: std::time::Instant::now(),
            timeout,
        }
    }

    pub(in crate::semantic) fn remaining_ms(&self, operation: &str) -> std::io::Result<u32> {
        let remaining = self.timeout.saturating_sub(self.started.elapsed());
        if remaining.is_zero() {
            return Err(windows_daemon_query_timeout(operation));
        }
        let millis = remaining.as_millis().max(1).min(u128::from(u32::MAX - 1));
        Ok(millis as u32)
    }
}

#[cfg(windows)]
pub(in crate::semantic) fn windows_daemon_query_timeout(operation: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("daemon query named pipe {operation} timed out"),
    )
}

#[cfg(windows)]
pub(in crate::semantic) fn open_windows_daemon_query_pipe(
    pipe_name: &[u16],
    deadline: &WindowsIoDeadline,
) -> Result<WindowsQueryHandle> {
    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_PIPE_BUSY, ERROR_SEM_TIMEOUT, GENERIC_READ, GENERIC_WRITE,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

    loop {
        let handle = unsafe {
            CreateFileW(
                pipe_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            return Ok(WindowsQueryHandle(handle));
        }
        let error = unsafe { GetLastError() };
        if error != ERROR_PIPE_BUSY {
            return Err(std::io::Error::from_raw_os_error(error as i32))
                .context("open daemon query named pipe");
        }

        let wait_ms = deadline
            .remaining_ms("connect")
            .context("wait for daemon query named pipe")?;
        let ok = unsafe { WaitNamedPipeW(pipe_name.as_ptr(), wait_ms) };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_SEM_TIMEOUT {
                return Err(windows_daemon_query_timeout("connect"))
                    .context("wait for daemon query named pipe");
            }
            return Err(std::io::Error::from_raw_os_error(error as i32))
                .context("wait for daemon query named pipe");
        }
    }
}

#[cfg(windows)]
pub(in crate::semantic) fn write_all_windows_daemon_query_pipe(
    pipe: &WindowsQueryHandle,
    mut request: &[u8],
    deadline: &WindowsIoDeadline,
) -> Result<()> {
    use windows_sys::Win32::Storage::FileSystem::WriteFile;

    while !request.is_empty() {
        let write_len = request.len().min(u32::MAX as usize) as u32;
        let written =
            windows_overlapped_io(pipe, deadline, "write", |transferred, overlapped| unsafe {
                WriteFile(pipe.0, request.as_ptr(), write_len, transferred, overlapped)
            })
            .context("write daemon query named pipe")?;
        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "daemon query named pipe wrote zero bytes",
            ))
            .context("write daemon query named pipe");
        }
        request = &request[written as usize..];
    }
    Ok(())
}

#[cfg(windows)]
pub(in crate::semantic) fn read_windows_daemon_query_pipe(
    pipe: &WindowsQueryHandle,
    response_limit: usize,
    deadline: &WindowsIoDeadline,
) -> Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::{
        ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::ReadFile;

    const READ_CHUNK_BYTES: usize = 64 * 1024;
    let mut response = Vec::with_capacity(response_limit.min(READ_CHUNK_BYTES));
    let mut chunk = vec![0u8; READ_CHUNK_BYTES];
    loop {
        let read_limit = (response_limit - response.len())
            .saturating_add(1)
            .min(chunk.len());
        let read =
            windows_overlapped_io(pipe, deadline, "read", |transferred, overlapped| unsafe {
                ReadFile(
                    pipe.0,
                    chunk.as_mut_ptr(),
                    read_limit as u32,
                    transferred,
                    overlapped,
                )
            });
        let read = match read {
            Ok(read) => read as usize,
            Err(error)
                if matches!(
                    error.raw_os_error().map(|code| code as u32),
                    Some(ERROR_BROKEN_PIPE) | Some(ERROR_NO_DATA) | Some(ERROR_PIPE_NOT_CONNECTED)
                ) =>
            {
                break;
            }
            Err(error) => return Err(error).context("read daemon query named pipe"),
        };
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
        if response.len() > response_limit {
            return Err(DaemonQueryResponseTooLarge::new(response_limit as u64).into());
        }
    }
    Ok(response)
}

#[cfg(windows)]
pub(in crate::semantic) fn windows_overlapped_io<F>(
    pipe: &WindowsQueryHandle,
    deadline: &WindowsIoDeadline,
    operation: &str,
    start: F,
) -> std::io::Result<u32>
where
    F: FnOnce(*mut u32, *mut windows_sys::Win32::System::IO::OVERLAPPED) -> windows_sys::core::BOOL,
{
    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_IO_PENDING, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};

    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let event = WindowsQueryHandle(event);
    let mut overlapped = OVERLAPPED {
        hEvent: event.0,
        ..OVERLAPPED::default()
    };
    let mut transferred = 0u32;
    let ok = start(&mut transferred, &mut overlapped);
    if ok != 0 {
        return Ok(transferred);
    }
    let error = unsafe { GetLastError() };
    if error != ERROR_IO_PENDING {
        return Err(std::io::Error::from_raw_os_error(error as i32));
    }

    let wait_ms = match deadline.remaining_ms(operation) {
        Ok(wait_ms) => wait_ms,
        Err(error) => {
            cancel_and_drain_windows_io(pipe, &overlapped);
            return Err(error);
        }
    };
    match unsafe { WaitForSingleObject(event.0, wait_ms) } {
        WAIT_OBJECT_0 => {}
        WAIT_TIMEOUT => {
            cancel_and_drain_windows_io(pipe, &overlapped);
            return Err(windows_daemon_query_timeout(operation));
        }
        WAIT_FAILED => {
            let error = std::io::Error::last_os_error();
            cancel_and_drain_windows_io(pipe, &overlapped);
            return Err(error);
        }
        status => {
            cancel_and_drain_windows_io(pipe, &overlapped);
            return Err(std::io::Error::other(format!(
                "unexpected Windows wait status {status}"
            )));
        }
    }

    let ok = unsafe { GetOverlappedResult(pipe.0, &overlapped, &mut transferred, 0) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(transferred)
}

#[cfg(windows)]
pub(in crate::semantic) fn cancel_and_drain_windows_io(
    pipe: &WindowsQueryHandle,
    overlapped: &windows_sys::Win32::System::IO::OVERLAPPED,
) {
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult};

    unsafe {
        let _ = CancelIoEx(pipe.0, overlapped);
        let mut transferred = 0u32;
        let _ = GetOverlappedResult(pipe.0, overlapped, &mut transferred, 1);
    }
}

#[cfg(windows)]
pub(in crate::semantic) fn windows_wide_null(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(all(test, windows))]
mod windows_query_transport_tests {
    use std::io::Write;

    use super::super::server::{
        connect_windows_daemon_query_pipe, create_windows_daemon_query_pipe,
        read_daemon_query_request_windows,
    };
    use super::*;

    #[test]
    pub(in crate::semantic) fn byte_pipe_roundtrip_uses_stream_protocol() {
        let pipe_name = daemon_query_pipe_name();
        let server = create_windows_daemon_query_pipe(&pipe_name, true).expect("create test pipe");
        let server_thread = std::thread::spawn(move || {
            let mut server = server;
            connect_windows_daemon_query_pipe(&server).expect("connect test pipe");
            assert_eq!(
                read_daemon_query_request_windows(&server, 1024, StdDuration::from_secs(2))
                    .expect("read request"),
                r#"{"ping":true}"#
            );
            server
                .write_all(b"{\"ok\":true}\n")
                .expect("write response");
        });

        let response = daemon_query_roundtrip_windows(
            &pipe_name,
            b"{\"ping\":true}\n",
            StdDuration::from_secs(2),
            1024,
        )
        .expect("roundtrip");
        assert_eq!(response, "{\"ok\":true}\n");
        server_thread.join().expect("server thread");
    }

    #[test]
    pub(in crate::semantic) fn stalled_byte_pipe_read_obeys_end_to_end_deadline() {
        let pipe_name = daemon_query_pipe_name();
        let server = create_windows_daemon_query_pipe(&pipe_name, true).expect("create test pipe");
        let server_thread = std::thread::spawn(move || {
            connect_windows_daemon_query_pipe(&server).expect("connect test pipe");
            read_daemon_query_request_windows(&server, 1024, StdDuration::from_secs(2))
                .expect("read request");
            std::thread::sleep(StdDuration::from_millis(500));
        });

        let started = std::time::Instant::now();
        let error = daemon_query_roundtrip_windows(
            &pipe_name,
            b"{\"ping\":true}\n",
            StdDuration::from_millis(50),
            1024,
        )
        .expect_err("stalled response must time out");
        assert!(format!("{error:#}").contains("timed out"));
        assert!(started.elapsed() < StdDuration::from_millis(450));
        server_thread.join().expect("server thread");
    }
}
use std::{
    fmt, fs,
    io::Read,
    path::{Path, PathBuf},
    process,
    time::Duration as StdDuration,
};

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(unix)]
use std::{
    io::Write, net::Shutdown, os::unix::fs::OpenOptionsExt, os::unix::net::UnixStream,
    time::Instant,
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::platform_security::verify_private_directory;
#[cfg(not(windows))]
use ctx_history_core::platform_security::verify_private_file;
#[cfg(windows)]
use ctx_history_core::platform_security::verify_private_file_handle;
use serde_json::{json, Value};
#[cfg(windows)]
use uuid::Uuid;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

use crate::compact_json;

use super::super::{
    health_search::secure_private_file_permissions,
    paths_status::{
        daemon_lock_path, daemon_root_path, open_or_create_pid_lock_file, pid_lock_guard_path,
        try_lock_pid_file, write_private_json_file,
    },
    runtime_limits::DAEMON_QUERY_ENDPOINT_FILE,
};
