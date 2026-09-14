#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DaemonQueryEndpoint {
    #[cfg(unix)]
    Unix { path: PathBuf, token: String },
    #[cfg(windows)]
    WindowsNamedPipe { pipe_name: String, token: String },
    #[cfg(not(any(unix, windows)))]
    #[allow(dead_code)]
    Unsupported,
}

mod submission;
#[cfg(unix)]
mod unix_response;
#[cfg(windows)]
use submission::mark_windows_pending_submission;
use submission::{mark_request_may_have_been_submitted, request_may_have_been_submitted};
#[cfg(unix)]
pub use unix_response::daemon_query_roundtrip_unix;
#[cfg(unix)]
use unix_response::daemon_query_roundtrip_unix_with_control;
#[cfg(unix)]
pub use unix_response::read_daemon_query_response_unix;

#[derive(Debug)]
pub struct IpcServiceUnavailable;

impl fmt::Display for IpcServiceUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("daemon IPC service is unavailable")
    }
}

impl std::error::Error for IpcServiceUnavailable {}

/// Operation-local control for bounded IPC waits. The runtime owns no signal
/// handler; final composition may inject typed cancellation and test clocks.
pub trait DaemonIpcWaitControl {
    fn checkpoint(&mut self) -> Result<()>;

    fn pause(&mut self, duration: StdDuration) -> Result<()> {
        self.checkpoint()?;
        std::thread::sleep(duration);
        self.checkpoint()
    }

    /// `None` preserves the ordinary one-shot native wait. Foreground callers
    /// provide a small slice so cancellation can be observed between waits.
    fn blocking_quantum(&self) -> Option<StdDuration> {
        None
    }
}

struct UninterruptedIpcWait;

impl DaemonIpcWaitControl for UninterruptedIpcWait {
    fn checkpoint(&mut self) -> Result<()> {
        Ok(())
    }
}

impl IpcServiceUnavailable {
    pub fn request_may_have_been_submitted(error: &anyhow::Error) -> bool {
        request_may_have_been_submitted(error)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DaemonQueryEndpointIdentity {
    pub endpoint: DaemonQueryEndpoint,
    pub owner_pid: u32,
}

#[derive(Debug)]
pub struct DaemonQueryResponseTooLarge {
    limit: u64,
}

impl DaemonQueryResponseTooLarge {
    pub fn new(limit: u64) -> Self {
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
    pub fn token(&self) -> &str {
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

pub fn write_daemon_service_endpoint_at(path: &Path, endpoint: &DaemonQueryEndpoint) -> Result<()> {
    let value = match endpoint {
        #[cfg(unix)]
        DaemonQueryEndpoint::Unix { path, token } => json!({
            "schema_version": 1,
            "transport": "unix",
            "path": path,
            "token": token,
            "pid": process::id(),
        }),
        #[cfg(windows)]
        DaemonQueryEndpoint::WindowsNamedPipe { pipe_name, token } => json!({
            "schema_version": 1,
            "transport": "windows_named_pipe",
            "pipe_name": pipe_name,
            "token": token,
            "pid": process::id(),
        }),
        #[cfg(not(any(unix, windows)))]
        DaemonQueryEndpoint::Unsupported => {
            return Err(anyhow!(
                "daemon query service is not supported on this platform"
            ));
        }
    };
    write_private_json_file(path, &value)
}

pub fn remove_daemon_service_endpoint_at(path: &Path) {
    let _ = fs::remove_file(path);
}

pub fn remove_released_daemon_service_endpoints(
    data_root: &Path,
    endpoint_paths: &[PathBuf],
) -> Result<()> {
    let _quiescence = crate::DaemonQuiescenceGuard::acquire(data_root)?.ok_or_else(|| {
        anyhow!(
            "refusing to remove daemon service artifacts while lifecycle ownership remains active"
        )
    })?;
    for endpoint_path in endpoint_paths {
        let identity = read_daemon_service_endpoint_identity_at(endpoint_path)
            .context("inspect released daemon service endpoint")?;
        #[cfg(unix)]
        if let Some(DaemonQueryEndpointIdentity {
            endpoint: DaemonQueryEndpoint::Unix { path, .. },
            ..
        }) = identity
        {
            remove_file_if_present(&path)
                .with_context(|| format!("remove released daemon socket {}", path.display()))?;
        }
        #[cfg(not(unix))]
        let _ = identity;
        remove_file_if_present(endpoint_path).with_context(|| {
            format!(
                "remove released daemon endpoint identity {}",
                endpoint_path.display()
            )
        })?;
    }
    Ok(())
}

fn remove_file_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn read_daemon_service_endpoint_identity_at(
    path: &Path,
) -> Result<Option<DaemonQueryEndpointIdentity>> {
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
    let mut file = match options.open(path) {
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
    verify_private_file(path)
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

pub fn read_daemon_query_endpoint_identity_value(
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

pub fn remove_daemon_service_endpoint_if_matches(
    daemon_lock_path: &Path,
    endpoint_path: &Path,
    expected: &DaemonQueryEndpointIdentity,
) {
    // The daemon owns this guard for the endpoint lifetime. Acquiring it makes
    // the identity re-read and unlink atomic with a replacement daemon start.
    let guard_path = pid_lock_guard_path(daemon_lock_path);
    let Ok((guard, _)) = open_or_create_pid_lock_file(&guard_path) else {
        return;
    };
    if secure_private_file_permissions(&guard_path).is_err() {
        return;
    }
    let Ok(true) = try_lock_pid_file(&guard) else {
        return;
    };
    let current = read_daemon_service_endpoint_identity_at(endpoint_path)
        .ok()
        .flatten();
    if current.as_ref() == Some(expected) {
        let _ = fs::remove_file(endpoint_path);
    }
    let _ = fs2::FileExt::unlock(&guard);
}

fn compact_json(mut value: Value) -> Value {
    fn prune(value: &mut Value) {
        match value {
            Value::Object(map) => map.retain(|_, nested| {
                prune(nested);
                !nested.is_null()
            }),
            Value::Array(items) => items.iter_mut().for_each(prune),
            _ => {}
        }
    }
    prune(&mut value);
    value
}

pub fn daemon_service_request(
    daemon_lock_path: &Path,
    endpoint_path: &Path,
    request: Value,
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<Option<Value>> {
    daemon_service_request_with_control(
        daemon_lock_path,
        endpoint_path,
        request,
        timeout,
        max_response_bytes,
        &mut UninterruptedIpcWait,
    )
}

pub fn daemon_service_request_with_control(
    daemon_lock_path: &Path,
    endpoint_path: &Path,
    mut request: Value,
    timeout: StdDuration,
    max_response_bytes: u64,
    control: &mut dyn DaemonIpcWaitControl,
) -> Result<Option<Value>> {
    control.checkpoint()?;
    let Some(identity) = read_daemon_service_endpoint_identity_at(endpoint_path)? else {
        return Ok(None);
    };
    let endpoint = &identity.endpoint;
    request["token"] = Value::String(endpoint.token().to_owned());
    let request = format!("{}\n", serde_json::to_string(&compact_json(request))?);
    let body = match daemon_query_roundtrip_with_control(
        endpoint,
        request.as_bytes(),
        timeout,
        max_response_bytes,
        control,
    ) {
        Ok(body) => body,
        Err(error) if daemon_query_roundtrip_error_is_unavailable(endpoint, &error) => {
            remove_daemon_service_endpoint_if_matches(daemon_lock_path, endpoint_path, &identity);
            return Err(IpcServiceUnavailable.into());
        }
        Err(error) => return Err(error),
    };
    let response: Value = serde_json::from_str(&body)
        .context("parse daemon query response")
        .map_err(mark_request_may_have_been_submitted)?;
    Ok(Some(response))
}

pub fn daemon_query_roundtrip(
    endpoint: &DaemonQueryEndpoint,
    request: &[u8],
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<String> {
    daemon_query_roundtrip_with_control(
        endpoint,
        request,
        timeout,
        max_response_bytes,
        &mut UninterruptedIpcWait,
    )
}

fn daemon_query_roundtrip_with_control(
    endpoint: &DaemonQueryEndpoint,
    request: &[u8],
    timeout: StdDuration,
    max_response_bytes: u64,
    control: &mut dyn DaemonIpcWaitControl,
) -> Result<String> {
    control.checkpoint()?;
    match endpoint {
        #[cfg(unix)]
        DaemonQueryEndpoint::Unix { path, .. } => {
            if max_response_bytes == 0 {
                return Err(DaemonQueryResponseTooLarge::new(0).into());
            }
            let body = daemon_query_roundtrip_unix_with_control(
                path,
                request,
                timeout,
                max_response_bytes,
                control,
            )?;
            String::from_utf8(body)
                .context("daemon query response is not UTF-8")
                .map_err(mark_request_may_have_been_submitted)
        }
        #[cfg(windows)]
        DaemonQueryEndpoint::WindowsNamedPipe { pipe_name, .. } => {
            daemon_query_roundtrip_windows_with_control(
                pipe_name,
                request,
                timeout,
                max_response_bytes,
                control,
            )
        }
        #[cfg(not(any(unix, windows)))]
        DaemonQueryEndpoint::Unsupported => Err(anyhow!(
            "daemon query service is not supported on this platform"
        )),
    }
}

pub fn daemon_query_roundtrip_error_is_unavailable(
    endpoint: &DaemonQueryEndpoint,
    error: &anyhow::Error,
) -> bool {
    if request_may_have_been_submitted(error) {
        return false;
    }
    let Some(io_error) = error.downcast_ref::<std::io::Error>() else {
        return false;
    };
    match endpoint {
        #[cfg(unix)]
        DaemonQueryEndpoint::Unix { .. } => {
            daemon_query_unix_io_error_is_pre_submission_unavailable(io_error.kind())
        }
        #[cfg(windows)]
        DaemonQueryEndpoint::WindowsNamedPipe { .. } => {
            daemon_query_windows_io_error_is_pre_submission_unavailable(
                io_error.kind(),
                io_error.raw_os_error(),
            )
        }
        #[cfg(not(any(unix, windows)))]
        DaemonQueryEndpoint::Unsupported => false,
    }
}

pub fn daemon_query_unix_io_error_is_pre_submission_unavailable(kind: std::io::ErrorKind) -> bool {
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

pub fn daemon_query_windows_io_error_is_pre_submission_unavailable(
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

#[cfg(windows)]
pub fn daemon_query_pipe_name() -> String {
    format!(r"\\.\pipe\ctx-daemon-query-{}", Uuid::new_v4().simple())
}

#[cfg(windows)]
pub fn windows_named_pipe_name_is_local(pipe_name: &str) -> bool {
    pipe_name
        .strip_prefix(r"\\.\pipe\ctx-daemon-query-")
        .is_some_and(|suffix| {
            suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

#[cfg(windows)]
pub fn daemon_query_roundtrip_windows(
    pipe_name: &str,
    request: &[u8],
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<String> {
    daemon_query_roundtrip_windows_with_control(
        pipe_name,
        request,
        timeout,
        max_response_bytes,
        &mut UninterruptedIpcWait,
    )
}

#[cfg(windows)]
fn daemon_query_roundtrip_windows_with_control(
    pipe_name: &str,
    request: &[u8],
    timeout: StdDuration,
    max_response_bytes: u64,
    control: &mut dyn DaemonIpcWaitControl,
) -> Result<String> {
    control.checkpoint()?;
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
    let pipe = open_windows_daemon_query_pipe_with_control(&pipe_name, &deadline, control)?;
    let mut request_may_have_been_submitted = false;
    if let Err(error) = write_all_windows_daemon_query_pipe_with_submission(
        &pipe,
        request,
        &deadline,
        &mut request_may_have_been_submitted,
        control,
    ) {
        return Err(if request_may_have_been_submitted {
            mark_request_may_have_been_submitted(error)
        } else {
            error
        });
    }
    let response =
        read_windows_daemon_query_pipe_with_control(&pipe, response_limit, &deadline, control)
            .map_err(mark_request_may_have_been_submitted)?;
    String::from_utf8(response)
        .context("daemon query response is not UTF-8")
        .map_err(mark_request_may_have_been_submitted)
}

#[cfg(windows)]
pub struct WindowsQueryHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for WindowsQueryHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
pub struct WindowsIoDeadline {
    started: std::time::Instant,
    timeout: StdDuration,
}

#[cfg(windows)]
impl WindowsIoDeadline {
    pub fn new(timeout: StdDuration) -> Self {
        Self {
            started: std::time::Instant::now(),
            timeout,
        }
    }

    pub fn remaining_ms(&self, operation: &str) -> std::io::Result<u32> {
        let remaining = self.timeout.saturating_sub(self.started.elapsed());
        if remaining.is_zero() {
            return Err(windows_daemon_query_timeout(operation));
        }
        let millis = remaining.as_millis().max(1).min(u128::from(u32::MAX - 1));
        Ok(millis as u32)
    }

    fn remaining_ms_capped(
        &self,
        operation: &str,
        quantum: Option<StdDuration>,
    ) -> std::io::Result<u32> {
        let remaining = self.remaining_ms(operation)?;
        let Some(quantum) = quantum else {
            return Ok(remaining);
        };
        let quantum = quantum.as_millis().max(1).min(u128::from(u32::MAX - 1)) as u32;
        Ok(remaining.min(quantum))
    }
}

#[cfg(windows)]
pub fn windows_daemon_query_timeout(operation: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("daemon query named pipe {operation} timed out"),
    )
}

#[cfg(windows)]
pub fn open_windows_daemon_query_pipe(
    pipe_name: &[u16],
    deadline: &WindowsIoDeadline,
) -> Result<WindowsQueryHandle> {
    open_windows_daemon_query_pipe_with_control(pipe_name, deadline, &mut UninterruptedIpcWait)
}

#[cfg(windows)]
fn open_windows_daemon_query_pipe_with_control(
    pipe_name: &[u16],
    deadline: &WindowsIoDeadline,
    control: &mut dyn DaemonIpcWaitControl,
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
        control.checkpoint()?;
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
            .remaining_ms_capped("connect", control.blocking_quantum())
            .context("wait for daemon query named pipe")?;
        let ok = unsafe { WaitNamedPipeW(pipe_name.as_ptr(), wait_ms) };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_SEM_TIMEOUT {
                control.checkpoint()?;
                if deadline.remaining_ms("connect").is_ok() {
                    continue;
                }
                return Err(windows_daemon_query_timeout("connect"))
                    .context("wait for daemon query named pipe");
            }
            return Err(std::io::Error::from_raw_os_error(error as i32))
                .context("wait for daemon query named pipe");
        }
    }
}

#[cfg(windows)]
pub fn write_all_windows_daemon_query_pipe(
    pipe: &WindowsQueryHandle,
    request: &[u8],
    deadline: &WindowsIoDeadline,
) -> Result<()> {
    let mut request_may_have_been_submitted = false;
    write_all_windows_daemon_query_pipe_with_submission(
        pipe,
        request,
        deadline,
        &mut request_may_have_been_submitted,
        &mut UninterruptedIpcWait,
    )
}

#[cfg(windows)]
fn write_all_windows_daemon_query_pipe_with_submission(
    pipe: &WindowsQueryHandle,
    mut request: &[u8],
    deadline: &WindowsIoDeadline,
    request_may_have_been_submitted: &mut bool,
    control: &mut dyn DaemonIpcWaitControl,
) -> Result<()> {
    use windows_sys::Win32::Storage::FileSystem::WriteFile;

    while !request.is_empty() {
        control.checkpoint()?;
        let write_len = request.len().min(u32::MAX as usize) as u32;
        let written = windows_overlapped_io_with_control(
            pipe,
            deadline,
            "write",
            Some(request_may_have_been_submitted),
            control,
            |transferred, overlapped| unsafe {
                WriteFile(pipe.0, request.as_ptr(), write_len, transferred, overlapped)
            },
        )
        .context("write daemon query named pipe")?;
        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "daemon query named pipe wrote zero bytes",
            ))
            .context("write daemon query named pipe");
        }
        *request_may_have_been_submitted = true;
        request = &request[written as usize..];
    }
    Ok(())
}

#[cfg(windows)]
pub fn read_windows_daemon_query_pipe(
    pipe: &WindowsQueryHandle,
    response_limit: usize,
    deadline: &WindowsIoDeadline,
) -> Result<Vec<u8>> {
    read_windows_daemon_query_pipe_with_control(
        pipe,
        response_limit,
        deadline,
        &mut UninterruptedIpcWait,
    )
}

#[cfg(windows)]
fn read_windows_daemon_query_pipe_with_control(
    pipe: &WindowsQueryHandle,
    response_limit: usize,
    deadline: &WindowsIoDeadline,
    control: &mut dyn DaemonIpcWaitControl,
) -> Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::{
        ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::ReadFile;

    const READ_CHUNK_BYTES: usize = 64 * 1024;
    let mut response = Vec::with_capacity(response_limit.min(READ_CHUNK_BYTES));
    let mut chunk = vec![0u8; READ_CHUNK_BYTES];
    loop {
        control.checkpoint()?;
        let read_limit = (response_limit - response.len())
            .saturating_add(1)
            .min(chunk.len());
        let read = windows_overlapped_io_with_control(
            pipe,
            deadline,
            "read",
            None,
            control,
            |transferred, overlapped| unsafe {
                ReadFile(
                    pipe.0,
                    chunk.as_mut_ptr(),
                    read_limit as u32,
                    transferred,
                    overlapped,
                )
            },
        );
        let read = match read {
            Ok(read) => read as usize,
            Err(error)
                if matches!(
                    error
                        .downcast_ref::<std::io::Error>()
                        .and_then(std::io::Error::raw_os_error)
                        .map(|code| code as u32),
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
pub fn windows_overlapped_io<F>(
    pipe: &WindowsQueryHandle,
    deadline: &WindowsIoDeadline,
    operation: &str,
    pending_submission: Option<&mut bool>,
    start: F,
) -> std::io::Result<u32>
where
    F: FnOnce(*mut u32, *mut windows_sys::Win32::System::IO::OVERLAPPED) -> windows_sys::core::BOOL,
{
    windows_overlapped_io_with_control(
        pipe,
        deadline,
        operation,
        pending_submission,
        &mut UninterruptedIpcWait,
        start,
    )
    .map_err(|error| match error.downcast::<std::io::Error>() {
        Ok(error) => error,
        Err(error) => std::io::Error::other(error.to_string()),
    })
}

#[cfg(windows)]
fn windows_overlapped_io_with_control<F>(
    pipe: &WindowsQueryHandle,
    deadline: &WindowsIoDeadline,
    operation: &str,
    pending_submission: Option<&mut bool>,
    control: &mut dyn DaemonIpcWaitControl,
    start: F,
) -> Result<u32>
where
    F: FnOnce(*mut u32, *mut windows_sys::Win32::System::IO::OVERLAPPED) -> windows_sys::core::BOOL,
{
    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_IO_PENDING, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};

    control.checkpoint()?;
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        return Err(std::io::Error::last_os_error().into());
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
        return Err(std::io::Error::from_raw_os_error(error as i32).into());
    }
    mark_windows_pending_submission(pending_submission);

    loop {
        if let Err(error) = control.checkpoint() {
            cancel_and_drain_windows_io(pipe, &overlapped);
            return Err(error);
        }
        let wait_ms = match deadline.remaining_ms_capped(operation, control.blocking_quantum()) {
            Ok(wait_ms) => wait_ms,
            Err(error) => {
                cancel_and_drain_windows_io(pipe, &overlapped);
                return Err(error.into());
            }
        };
        match unsafe { WaitForSingleObject(event.0, wait_ms) } {
            WAIT_OBJECT_0 => {
                if let Err(error) = control.checkpoint() {
                    cancel_and_drain_windows_io(pipe, &overlapped);
                    return Err(error);
                }
                break;
            }
            WAIT_TIMEOUT => {
                if let Err(error) = control.checkpoint() {
                    cancel_and_drain_windows_io(pipe, &overlapped);
                    return Err(error);
                }
                if deadline.remaining_ms(operation).is_ok() {
                    continue;
                }
                cancel_and_drain_windows_io(pipe, &overlapped);
                return Err(windows_daemon_query_timeout(operation).into());
            }
            WAIT_FAILED => {
                let error = std::io::Error::last_os_error();
                cancel_and_drain_windows_io(pipe, &overlapped);
                return Err(error.into());
            }
            status => {
                cancel_and_drain_windows_io(pipe, &overlapped);
                return Err(std::io::Error::other(format!(
                    "unexpected Windows wait status {status}"
                ))
                .into());
            }
        }
    }

    let ok = unsafe { GetOverlappedResult(pipe.0, &overlapped, &mut transferred, 0) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(transferred)
}

#[cfg(windows)]
pub fn cancel_and_drain_windows_io(
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
pub fn windows_wide_null(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

use std::{
    fmt, fs,
    io::Read,
    path::{Path, PathBuf},
    process,
    time::Duration as StdDuration,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(test)]
pub fn read_daemon_query_request<S: std::io::Read>(
    stream: &mut S,
    max_bytes: usize,
) -> Result<String> {
    super::server::read_bounded_daemon_request(stream, max_bytes)
}

use anyhow::{anyhow, Context, Result};
use ctx_history_platform::platform_security::verify_private_directory;
#[cfg(not(windows))]
use ctx_history_platform::platform_security::verify_private_file;
#[cfg(windows)]
use ctx_history_platform::platform_security::verify_private_file_handle;
use serde_json::{json, Value};
#[cfg(windows)]
use uuid::Uuid;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

use crate::{
    open_or_create_pid_lock_file, pid_lock_guard_path, secure_private_file_permissions,
    try_lock_pid_file, write_private_json_file,
};
