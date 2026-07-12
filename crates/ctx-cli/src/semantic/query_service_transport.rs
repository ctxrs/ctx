#[derive(Debug, Clone)]
enum DaemonQueryEndpoint {
    #[cfg(unix)]
    Unix { path: PathBuf, token: String },
    #[cfg(windows)]
    WindowsNamedPipe { pipe_name: String, token: String },
    #[cfg(not(any(unix, windows)))]
    #[allow(dead_code)]
    Unsupported,
}

impl DaemonQueryEndpoint {
    fn token(&self) -> &str {
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

fn daemon_query_endpoint_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_QUERY_ENDPOINT_FILE)
}

fn write_daemon_query_endpoint(data_root: &Path, endpoint: &DaemonQueryEndpoint) -> Result<()> {
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
    write_private_json_file(&daemon_query_endpoint_path(data_root), &value)
}

fn remove_daemon_query_endpoint(data_root: &Path) {
    let _ = fs::remove_file(daemon_query_endpoint_path(data_root));
}

fn read_daemon_query_endpoint(data_root: &Path) -> Result<Option<DaemonQueryEndpoint>> {
    let path = daemon_query_endpoint_path(data_root);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read daemon query endpoint {}", path.display()));
        }
    };
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("parse daemon query endpoint {}", path.display()))?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Ok(None);
    }
    read_daemon_query_endpoint_value(value)
}

fn read_daemon_query_endpoint_value(value: Value) -> Result<Option<DaemonQueryEndpoint>> {
    let Some(token) = value
        .get("token")
        .and_then(Value::as_str)
        .filter(|token| token.len() >= 32)
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    match value.get("transport").and_then(Value::as_str) {
        #[cfg(unix)]
        Some("unix") => {
            let path = value.get("path").and_then(Value::as_str).map(PathBuf::from);
            Ok(path.map(|path| DaemonQueryEndpoint::Unix { path, token }))
        }
        #[cfg(windows)]
        Some("windows_named_pipe") => {
            let pipe_name = value
                .get("pipe_name")
                .and_then(Value::as_str)
                .filter(|pipe_name| windows_named_pipe_name_is_local(pipe_name))
                .map(str::to_owned);
            Ok(pipe_name.map(|pipe_name| DaemonQueryEndpoint::WindowsNamedPipe {
                pipe_name,
                token,
            }))
        }
        _ => Ok(None),
    }
}

fn daemon_query_request(
    data_root: &Path,
    mut request: Value,
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<Option<Value>> {
    let Some(endpoint) = read_daemon_query_endpoint(data_root)? else {
        return Ok(None);
    };
    request["token"] = Value::String(endpoint.token().to_owned());
    let request = format!("{}\n", serde_json::to_string(&compact_json(request))?);
    let body = daemon_query_roundtrip(&endpoint, request.as_bytes(), timeout, max_response_bytes)?;
    let response: Value = serde_json::from_str(&body).context("parse daemon query response")?;
    Ok(Some(response))
}

fn daemon_query_roundtrip(
    endpoint: &DaemonQueryEndpoint,
    request: &[u8],
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<String> {
    match endpoint {
        #[cfg(unix)]
        DaemonQueryEndpoint::Unix { path, .. } => {
            if !path.exists() {
                return Err(anyhow!("daemon query socket does not exist"));
            }
            let mut stream = UnixStream::connect(path)
                .with_context(|| format!("connect daemon query socket {}", path.display()))?;
            stream
                .set_read_timeout(Some(timeout))
                .context("set daemon query read timeout")?;
            stream
                .set_write_timeout(Some(timeout))
                .context("set daemon query write timeout")?;
            stream
                .write_all(request)
                .context("write daemon query request")?;
            let _ = stream.shutdown(Shutdown::Write);
            let mut body = String::new();
            stream
                .take(max_response_bytes)
                .read_to_string(&mut body)
                .context("read daemon query response")?;
            Ok(body)
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

fn read_daemon_query_request<S: std::io::Read>(stream: &mut S, max_bytes: usize) -> Result<String> {
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
fn read_daemon_query_request_unix(
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
fn daemon_query_pipe_name() -> String {
    format!(
        r"\\.\pipe\ctx-daemon-query-{}",
        Uuid::new_v4().simple()
    )
}

#[cfg(windows)]
fn windows_named_pipe_name_is_local(pipe_name: &str) -> bool {
    pipe_name
        .strip_prefix(r"\\.\pipe\ctx-daemon-query-")
        .is_some_and(|suffix| {
            suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

#[cfg(windows)]
fn daemon_query_roundtrip_windows(
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
    let response_limit = usize::try_from(max_response_bytes)
        .ok()
        .ok_or_else(|| anyhow!("daemon query response limit is too large for Windows named pipe"))?;
    let deadline = WindowsIoDeadline::new(timeout);
    let pipe_name = windows_wide_null(pipe_name);
    let pipe = open_windows_daemon_query_pipe(&pipe_name, &deadline)?;
    write_all_windows_daemon_query_pipe(&pipe, request, &deadline)?;
    let response = read_windows_daemon_query_pipe(&pipe, response_limit, &deadline)?;
    String::from_utf8(response).context("daemon query response is not UTF-8")
}

#[cfg(windows)]
struct WindowsQueryHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for WindowsQueryHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
struct WindowsIoDeadline {
    started: std::time::Instant,
    timeout: StdDuration,
}

#[cfg(windows)]
impl WindowsIoDeadline {
    fn new(timeout: StdDuration) -> Self {
        Self {
            started: std::time::Instant::now(),
            timeout,
        }
    }

    fn remaining_ms(&self, operation: &str) -> std::io::Result<u32> {
        let remaining = self.timeout.saturating_sub(self.started.elapsed());
        if remaining.is_zero() {
            return Err(windows_daemon_query_timeout(operation));
        }
        let millis = remaining.as_millis().max(1).min(u128::from(u32::MAX - 1));
        Ok(millis as u32)
    }
}

#[cfg(windows)]
fn windows_daemon_query_timeout(operation: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("daemon query named pipe {operation} timed out"),
    )
}

#[cfg(windows)]
fn open_windows_daemon_query_pipe(
    pipe_name: &[u16],
    deadline: &WindowsIoDeadline,
) -> Result<WindowsQueryHandle> {
    use windows_sys::Win32::Foundation::{
        GetLastError, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, ERROR_PIPE_BUSY,
        ERROR_SEM_TIMEOUT,
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
fn write_all_windows_daemon_query_pipe(
    pipe: &WindowsQueryHandle,
    mut request: &[u8],
    deadline: &WindowsIoDeadline,
) -> Result<()> {
    use windows_sys::Win32::Storage::FileSystem::WriteFile;

    while !request.is_empty() {
        let write_len = request.len().min(u32::MAX as usize) as u32;
        let written = windows_overlapped_io(pipe, deadline, "write", |transferred, overlapped| unsafe {
            WriteFile(
                pipe.0,
                request.as_ptr(),
                write_len,
                transferred,
                overlapped,
            )
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
fn read_windows_daemon_query_pipe(
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
        let read = windows_overlapped_io(pipe, deadline, "read", |transferred, overlapped| unsafe {
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
            return Err(anyhow!("daemon query response is too large"));
        }
    }
    Ok(response)
}

#[cfg(windows)]
fn windows_overlapped_io<F>(
    pipe: &WindowsQueryHandle,
    deadline: &WindowsIoDeadline,
    operation: &str,
    start: F,
) -> std::io::Result<u32>
where
    F: FnOnce(
        *mut u32,
        *mut windows_sys::Win32::System::IO::OVERLAPPED,
    ) -> windows_sys::core::BOOL,
{
    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_IO_PENDING, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

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
fn cancel_and_drain_windows_io(
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
fn windows_wide_null(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(all(test, windows))]
mod windows_query_transport_tests {
    use super::*;

    #[test]
    fn byte_pipe_roundtrip_uses_stream_protocol() {
        let pipe_name = daemon_query_pipe_name();
        let server =
            create_windows_daemon_query_pipe(&pipe_name, true).expect("create test pipe");
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
    fn stalled_byte_pipe_read_obeys_end_to_end_deadline() {
        let pipe_name = daemon_query_pipe_name();
        let server =
            create_windows_daemon_query_pipe(&pipe_name, true).expect("create test pipe");
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
