#[derive(Debug)]
struct DaemonIteration {
    did_work: bool,
    failed: bool,
}

#[derive(Default)]
struct DaemonRuntime {
    semantic_embedder: Arc<Mutex<Option<SemanticEmbedder>>>,
    semantic_bootstrap_passes_since_refresh: usize,
}

fn daemon_runtime_embedder_loaded(runtime: &DaemonRuntime) -> bool {
    runtime
        .semantic_embedder
        .lock()
        .map(|embedder| embedder.is_some())
        .unwrap_or(false)
}

fn lock_daemon_runtime_embedder(
    runtime: &DaemonRuntime,
) -> Result<std::sync::MutexGuard<'_, Option<SemanticEmbedder>>> {
    lock_shared_semantic_embedder(&runtime.semantic_embedder)
}

fn lock_shared_semantic_embedder(
    embedder: &Arc<Mutex<Option<SemanticEmbedder>>>,
) -> Result<std::sync::MutexGuard<'_, Option<SemanticEmbedder>>> {
    embedder
        .lock()
        .map_err(|_| anyhow!("semantic embedder lock is poisoned"))
}

fn shared_semantic_embedder_policy_status_json(
    embedder: &Arc<Mutex<Option<SemanticEmbedder>>>,
) -> Result<Value> {
    let guard = lock_shared_semantic_embedder(embedder)?;
    Ok(semantic_embedder_policy_status_json(&guard))
}

fn shared_semantic_embedder_runtime_status_json(
    embedder: &Arc<Mutex<Option<SemanticEmbedder>>>,
) -> Result<Option<Value>> {
    let guard = lock_shared_semantic_embedder(embedder)?;
    Ok(semantic_embedder_runtime_status_json(&guard))
}

#[cfg(ctx_semantic_fastembed)]
fn embed_documents_with_shared_runtime(
    shared: &Arc<Mutex<Option<SemanticEmbedder>>>,
    cache_dir: &Path,
    texts: Vec<String>,
    deadline: Option<Instant>,
) -> Result<(Vec<Vec<f32>>, SemanticQuietPolicy)> {
    let mut guard = lock_shared_semantic_embedder(shared)?;
    let started = Instant::now();
    let first = guard
        .as_mut()
        .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
        .embed_documents(texts.clone());
    let embeddings = match first {
        Ok(embeddings) => embeddings,
        Err(first_error) => {
            let runtime = guard
                .as_ref()
                .ok_or_else(|| anyhow!("semantic embedder disappeared after inference failure"))?
                .runtime_info();
            *guard = None;
            let mut replacement = reacquire_semantic_embedder(cache_dir, &runtime)
                .context("reinitialize semantic embedder after document inference failure")?;
            let retry = replacement.embed_documents(texts).with_context(|| {
                format!("semantic document inference failed twice; first failure: {first_error:#}")
            })?;
            *guard = Some(replacement);
            retry
        }
    };
    let quiet_policy = guard
        .as_ref()
        .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
        .quiet_policy();
    drop(guard);
    let active = started.elapsed();
    let remaining = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
    throttle_semantic_batch(active, quiet_policy, remaining);
    Ok((embeddings, quiet_policy))
}

#[cfg(ctx_semantic_fastembed)]
fn embed_query_with_shared_runtime(
    shared: &Arc<Mutex<Option<SemanticEmbedder>>>,
    cache_dir: &Path,
    query: String,
) -> Result<(Vec<f32>, SemanticEmbeddingRuntimeInfo)> {
    let mut guard = lock_shared_semantic_embedder(shared)?;
    let first = guard
        .as_mut()
        .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
        .embed_query(query.clone());
    let embedding = match first {
        Ok(embedding) => embedding,
        Err(first_error) => {
            let runtime = guard
                .as_ref()
                .ok_or_else(|| anyhow!("semantic embedder disappeared after inference failure"))?
                .runtime_info();
            *guard = None;
            let mut replacement = reacquire_semantic_embedder(cache_dir, &runtime)
                .context("reinitialize semantic embedder after query inference failure")?;
            let retry = replacement.embed_query(query).with_context(|| {
                format!("semantic query inference failed twice; first failure: {first_error:#}")
            })?;
            *guard = Some(replacement);
            retry
        }
    };
    let runtime = guard
        .as_ref()
        .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
        .runtime_info();
    Ok((embedding, runtime))
}

struct DaemonQueryService {
    data_root: PathBuf,
    activity: Arc<DaemonQueryActivity>,
    thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(unix)]
    socket_path: PathBuf,
    #[cfg(unix)]
    socket_runtime_dir: Option<PathBuf>,
    #[cfg(windows)]
    pipe_name: String,
}

const DAEMON_QUERY_REQUEST_MAX_BYTES: usize = 256 * 1024;
const DAEMON_QUERY_REQUEST_READ_TIMEOUT: StdDuration = StdDuration::from_secs(2);

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
struct DaemonQueryActivity {
    state: Mutex<DaemonQueryActivityState>,
}

#[derive(Default)]
struct DaemonQueryActivityState {
    accepting: bool,
    stopping: bool,
    active_requests: usize,
    generation: u64,
}

struct DaemonQueryRequestGuard {
    activity: Arc<DaemonQueryActivity>,
}

impl DaemonQueryActivity {
    fn new() -> Self {
        Self {
            state: Mutex::new(DaemonQueryActivityState {
                accepting: true,
                ..DaemonQueryActivityState::default()
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, DaemonQueryActivityState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn begin_request(self: &Arc<Self>) -> Option<DaemonQueryRequestGuard> {
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

    fn snapshot(&self) -> (usize, u64) {
        let state = self.state();
        (state.active_requests, state.generation)
    }

    fn try_stop_accepting_if_idle(&self, observed_generation: u64) -> bool {
        let mut state = self.state();
        if state.active_requests != 0 || state.generation != observed_generation {
            return false;
        }
        state.accepting = false;
        true
    }

    fn stop(&self) {
        let mut state = self.state();
        state.accepting = false;
        state.stopping = true;
    }

    fn stopping(&self) -> bool {
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

fn observe_daemon_query_activity(
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

fn daemon_can_begin_idle_shutdown(
    activity: Option<&DaemonQueryActivity>,
    observed_generation: u64,
) -> bool {
    activity.is_none_or(|activity| activity.try_stop_accepting_if_idle(observed_generation))
}

#[cfg(unix)]
const DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES: usize = 90;

#[cfg(unix)]
fn bind_daemon_query_listener(
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
            if let Err(error) = fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700)) {
                let _ = fs::remove_dir(&runtime_dir);
                failures.push(format!("secure {}: {error}", runtime_dir.display()));
                continue;
            }
            let path = runtime_dir.join("q.sock");
            if path.as_os_str().as_bytes().len() > DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES {
                let _ = fs::remove_dir(&runtime_dir);
                failures.push(format!("fallback socket path is still too long: {}", path.display()));
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
fn start_daemon_query_service(
    data_root: &Path,
    embedder: Arc<Mutex<Option<SemanticEmbedder>>>,
) -> Result<DaemonQueryService> {
    start_daemon_query_service_with_request_timeout(
        data_root,
        embedder,
        DAEMON_QUERY_REQUEST_READ_TIMEOUT,
    )
}

#[cfg(unix)]
fn start_daemon_query_service_with_request_timeout(
    data_root: &Path,
    embedder: Arc<Mutex<Option<SemanticEmbedder>>>,
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
                        if configure_daemon_query_stream_unix(
                            &stream,
                            request_read_timeout,
                        )
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
                            &embedder,
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
fn configure_daemon_query_stream_unix(
    stream: &UnixStream,
    write_timeout: StdDuration,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(write_timeout))
}

#[cfg(windows)]
fn start_daemon_query_service(
    data_root: &Path,
    embedder: Arc<Mutex<Option<SemanticEmbedder>>>,
) -> Result<DaemonQueryService> {
    start_daemon_query_service_with_request_timeout(
        data_root,
        embedder,
        DAEMON_QUERY_REQUEST_READ_TIMEOUT,
    )
}

#[cfg(windows)]
fn start_daemon_query_service_with_request_timeout(
    data_root: &Path,
    embedder: Arc<Mutex<Option<SemanticEmbedder>>>,
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
                    &embedder,
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
struct WindowsDaemonQueryPipe {
    handle: windows_sys::Win32::Foundation::HANDLE,
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
struct WindowsDaemonQueryRequestReader<'a> {
    pipe: &'a WindowsDaemonQueryPipe,
    deadline: WindowsIoDeadline,
}

#[cfg(windows)]
impl WindowsDaemonQueryRequestReader<'_> {
    fn new(
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
fn read_daemon_query_request_windows(
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
fn create_windows_daemon_query_pipe(
    pipe_name: &str,
    first_instance: bool,
) -> Result<WindowsDaemonQueryPipe> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        PIPE_WAIT,
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
fn connect_windows_daemon_query_pipe(stream: &WindowsDaemonQueryPipe) -> Result<()> {
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
fn wake_windows_daemon_query_pipe(pipe_name: &str) {
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
fn start_daemon_query_service(
    _data_root: &Path,
    _embedder: Arc<Mutex<Option<SemanticEmbedder>>>,
) -> Result<DaemonQueryService> {
    Err(anyhow!("daemon query service is not supported on this platform"))
}

fn handle_daemon_query_stream<S: std::io::Write>(
    data_root: &Path,
    embedder: &Arc<Mutex<Option<SemanticEmbedder>>>,
    token: &str,
    mut stream: S,
    request: Result<String>,
) {
    let result = request.and_then(|body| {
        handle_daemon_query_stream_inner(data_root, embedder, token, &mut stream, &body)
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

fn handle_daemon_query_stream_inner<S: std::io::Write>(
    data_root: &Path,
    embedder: &Arc<Mutex<Option<SemanticEmbedder>>>,
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
        let (runtime, busy) = match embedder.try_lock() {
            Ok(guard) => (
                guard
                    .as_ref()
                    .map(|embedder| embedder.runtime_info().to_json()),
                false,
            ),
            Err(std::sync::TryLockError::WouldBlock) => (None, true),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(anyhow!("semantic embedder lock is poisoned"));
            }
        };
        writeln!(
            stream,
            "{}",
            serde_json::to_string(&compact_json(json!({
                "ok": true,
                "schema_version": 1,
                "model_key": semantic_model_key(),
                "embedding_runtime": runtime,
                "busy": busy,
            })))?
        )?;
        return Ok(());
    }
    if op != "embed_query" {
        return Err(anyhow!("unknown daemon query operation `{op}`"));
    }
    let model_key = request.get("model_key").and_then(Value::as_str).unwrap_or("");
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
    {
        let mut guard = lock_shared_semantic_embedder(embedder)?;
        if guard.is_none() {
        let cache_dir = semantic_worker_cache_dir(data_root);
        if !semantic_model_cache_available(&cache_dir) {
            return Err(anyhow!("semantic model cache is not available to daemon query service"));
        }
            *guard = Some(new_semantic_embedder(&cache_dir)?);
        }
    }
    let cache_dir = semantic_worker_cache_dir(data_root);
    let (embedding, runtime) =
        embed_query_with_shared_runtime(embedder, &cache_dir, text.to_owned())?;
    let query_embed_ms = started.elapsed().as_millis() as u64;
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&compact_json(json!({
            "ok": true,
            "model_key": semantic_model_key(),
            "embedding_runtime": runtime.to_json(),
            "query_embed_ms": query_embed_ms,
            "embedding": embedding,
        })))?
    )?;
    Ok(())
}

#[cfg(all(test, ctx_sqlite_vec))]
#[derive(Clone)]
struct DaemonTestJobHooks {
    calls: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    history_refresh: Option<Value>,
    semantic_index: Option<Value>,
}

#[cfg(all(test, ctx_sqlite_vec))]
thread_local! {
    static DAEMON_TEST_JOB_HOOKS: std::cell::RefCell<Option<DaemonTestJobHooks>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, ctx_sqlite_vec))]
struct DaemonTestJobHookGuard;

#[cfg(all(test, ctx_sqlite_vec))]
impl Drop for DaemonTestJobHookGuard {
    fn drop(&mut self) {
        DAEMON_TEST_JOB_HOOKS.with(|hooks| {
            *hooks.borrow_mut() = None;
        });
    }
}

#[cfg(all(test, ctx_sqlite_vec))]
fn install_daemon_test_job_hooks(hooks: DaemonTestJobHooks) -> DaemonTestJobHookGuard {
    DAEMON_TEST_JOB_HOOKS.with(|slot| {
        assert!(slot.borrow().is_none(), "daemon test job hook already installed");
        *slot.borrow_mut() = Some(hooks);
    });
    DaemonTestJobHookGuard
}

#[cfg(all(test, ctx_sqlite_vec))]
fn daemon_test_job(job: &'static str) -> Option<Value> {
    DAEMON_TEST_JOB_HOOKS.with(|slot| {
        let hooks = slot.borrow();
        let hooks = hooks.as_ref()?;
        hooks.calls.borrow_mut().push(job);
        match job {
            "history_refresh" => hooks.history_refresh.clone(),
            "semantic_index" => hooks.semantic_index.clone(),
            _ => None,
        }
    })
}

#[derive(Debug, Clone)]
struct SemanticWorkerArgs {
    max_chunks: Option<usize>,
    max_seconds: Option<u64>,
}

pub(crate) fn run_daemon_command(
    args: DaemonArgs,
    data_root: PathBuf,
    config: &AppConfig,
) -> Result<()> {
    match args.command {
        DaemonCommand::Run(args) => run_daemon(args, data_root, config),
        DaemonCommand::Status(args) => run_daemon_status(args, data_root),
        DaemonCommand::Enable(args) => run_daemon_enabled_update(args, data_root, true),
        DaemonCommand::Disable(args) => run_daemon_enabled_update(args, data_root, false),
    }
}

fn run_daemon_status(args: JsonArgs, data_root: PathBuf) -> Result<()> {
    let semantic_report = semantic_worker_report_for_daemon(&data_root);
    let daemon = daemon_report(&data_root, &semantic_report);
    if args.json {
        print_json(json!({
            "schema_version": 1,
            "daemon": daemon,
            "local_only": true,
        }))?;
    } else {
        print_daemon_status_human(&daemon);
    }
    Ok(())
}

fn run_daemon_enabled_update(args: JsonArgs, data_root: PathBuf, enabled: bool) -> Result<()> {
    config::set_daemon_enabled(&data_root, enabled)?;
    if args.json {
        print_json(json!({
            "schema_version": 1,
            "daemon_enabled": enabled,
            "config_path": data_root.join(CONFIG_FILE),
            "local_only": true,
        }))?;
    } else {
        println!("daemon_enabled: {enabled}");
        println!("config_path: {}", data_root.join(CONFIG_FILE).display());
    }
    Ok(())
}

fn print_daemon_status_human(daemon: &Value) {
    println!(
        "daemon_enabled: {}",
        daemon
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    );
    println!(
        "daemon_status: {}",
        daemon
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "daemon_running: {}",
        daemon
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    if let Some(reason) = daemon.get("reason").and_then(Value::as_str) {
        println!("daemon_reason: {reason}");
    }
    if daemon
        .get("recoverable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        println!("daemon_recoverable: true");
    }
    println!(
        "history_refresh_status: {}",
        daemon
            .get("jobs")
            .and_then(|jobs| jobs.get("history_refresh"))
            .and_then(|job| job.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "semantic_index_status: {}",
        daemon
            .get("jobs")
            .and_then(|jobs| jobs.get("semantic_index"))
            .and_then(|job| job.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    let embedding_runtime = daemon
        .get("jobs")
        .and_then(|jobs| jobs.get("semantic_index"))
        .and_then(|job| job.get("embedding_runtime"));
    if let Some(backend) = embedding_runtime
        .and_then(|runtime| runtime.get("backend"))
        .and_then(Value::as_str)
    {
        println!("semantic_embedding_backend: {backend}");
    }
    if let Some(compute_mode) = embedding_runtime
        .and_then(|runtime| runtime.get("compute_mode"))
        .and_then(Value::as_str)
    {
        println!("semantic_embedding_compute_mode: {compute_mode}");
    }
    if let Some(fallback) = embedding_runtime
        .and_then(|runtime| runtime.get("acquisition_fallback"))
        .and_then(Value::as_str)
    {
        println!("semantic_embedding_fallback: {fallback}");
    }
    println!(
        "cloud_sync_status: {}",
        daemon
            .get("jobs")
            .and_then(|jobs| jobs.get("cloud_sync"))
            .and_then(|cloud| cloud.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("disabled")
    );
}

fn run_daemon(args: DaemonRunArgs, data_root: PathBuf, config: &AppConfig) -> Result<()> {
    if (args.start_mode.is_some() || args.trigger_command.is_some())
        && !semantic_env_flag(DAEMON_BACKGROUND_CHILD_ENV)
    {
        return Err(anyhow!(
            "daemon autostart metadata flags are internal; run `ctx daemon run` without --start-mode or --trigger-command"
        ));
    }
    let semantic_enabled = config.semantic_search_enabled() && semantic_query_service_supported();
    if semantic_enabled {
        lower_semantic_worker_priority();
    }
    let report = match run_daemon_inner(
        args.clone(),
        &data_root,
        config.daemon.enabled,
        semantic_enabled,
    ) {
        Ok(report) => report,
        Err(error) => {
            let message = format!("{error:#}");
            let now = utc_now().timestamp_millis();
            let _ = write_daemon_status(
                &data_root,
                &json!({
                    "schema_version": 1,
                    "status": "failed",
                    "pid": process::id(),
                    "heartbeat_at_ms": now,
                    "finished_at_ms": now,
                    "start_mode": daemon_run_start_mode(&args).as_str(),
                    "trigger_command": args.trigger_command.map(DaemonTriggerCommandArg::as_str),
                    "last_error": message,
                }),
            );
            return Err(error);
        }
    };
    if args.json {
        print_json(report)?;
    } else {
        print_daemon_status_human(&report);
    }
    Ok(())
}

fn run_daemon_inner(
    args: DaemonRunArgs,
    data_root: &Path,
    daemon_enabled: bool,
    semantic_enabled: bool,
) -> Result<Value> {
    if !daemon_enabled && !args.force {
        let semantic_report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_report(data_root, &semantic_report));
    }
    let Some(_lock) = DaemonLock::acquire(data_root)? else {
        let semantic_report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_report(data_root, &semantic_report));
    };

    let run_once = args.once;
    let idle_exit = StdDuration::from_secs(
        args.idle_exit_seconds
            .unwrap_or(DAEMON_IDLE_EXIT_SECONDS_DEFAULT),
    );
    let loop_interval = StdDuration::from_secs(
        args.loop_interval_seconds
            .unwrap_or(DAEMON_LOOP_INTERVAL_SECONDS_DEFAULT),
    );
    let started_at_ms = utc_now().timestamp_millis();
    let mut failed = false;
    write_daemon_lifecycle_status(data_root, &args, "running", started_at_ms, None, None)?;

    let mut runtime = DaemonRuntime::default();
    let query_service = if semantic_enabled {
        Some(start_daemon_query_service(data_root, runtime.semantic_embedder.clone())?)
    } else {
        None
    };
    let mut idle_since: Option<Instant> = None;
    let mut observed_query_generation = 0;
    loop {
        observe_daemon_query_activity(
            query_service
                .as_ref()
                .map(|service| service.activity.as_ref()),
            &mut idle_since,
            &mut observed_query_generation,
        );
        if idle_since.is_some_and(|idle| idle.elapsed() >= idle_exit) {
            if daemon_can_begin_idle_shutdown(
                query_service
                    .as_ref()
                    .map(|service| service.activity.as_ref()),
                observed_query_generation,
            ) {
                break;
            }
            observe_daemon_query_activity(
                query_service
                    .as_ref()
                    .map(|service| service.activity.as_ref()),
                &mut idle_since,
                &mut observed_query_generation,
            );
            continue;
        }
        let iteration = run_daemon_once(&args, data_root, &mut runtime, None, semantic_enabled)?;
        write_daemon_lifecycle_status(data_root, &args, "running", started_at_ms, None, None)?;
        if iteration.failed {
            failed = true;
            break;
        }
        if run_once {
            break;
        }
        observe_daemon_query_activity(
            query_service
                .as_ref()
                .map(|service| service.activity.as_ref()),
            &mut idle_since,
            &mut observed_query_generation,
        );
        if iteration.did_work {
            idle_since = None;
        } else if idle_since.is_none() {
            idle_since = Some(Instant::now());
        }
        std::thread::sleep(loop_interval);
    }

    write_daemon_lifecycle_status(
        data_root,
        &args,
        if failed { "failed" } else { "completed" },
        started_at_ms,
        Some(utc_now().timestamp_millis()),
        failed.then_some("one or more daemon jobs failed".to_owned()),
    )?;
    // Keep daemon ownership until the query service has removed its endpoint
    // and joined its listener thread. Otherwise a replacement can publish a
    // new endpoint that this service's destructor then removes.
    drop(query_service);
    drop(_lock);
    let semantic_report = semantic_worker_report_for_daemon(data_root);
    Ok(daemon_report_with_disabled_status(
        data_root,
        &semantic_report,
        !args.force,
    ))
}

fn run_daemon_once(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
) -> Result<DaemonIteration> {
    if semantic_enabled && semantic_bootstrap_should_run_first(data_root, runtime)? {
        let history_refresh_job =
            daemon_history_refresh_skipped_job("semantic_bootstrap_in_progress");
        write_daemon_job_status(&daemon_history_refresh_job_path(data_root), &history_refresh_job)?;
        let semantic_job = run_daemon_semantic_job(args, data_root, runtime, deadline, semantic_enabled)
            .unwrap_or_else(|error| daemon_semantic_failed_job(data_root, format!("{error:#}")));
        let semantic_did_work = daemon_semantic_job_did_work(&semantic_job);
        runtime.semantic_bootstrap_passes_since_refresh =
            runtime.semantic_bootstrap_passes_since_refresh.saturating_add(1);
        write_daemon_job_status_unless_deadline_skip(
            &daemon_semantic_job_path(data_root),
            &semantic_job,
        )?;
        let cloud_sync_job = daemon_cloud_sync_disabled_job(Some(utc_now().timestamp_millis()));
        write_daemon_job_status(&daemon_cloud_sync_job_path(data_root), &cloud_sync_job)?;
        return Ok(DaemonIteration {
            did_work: semantic_did_work,
            failed: daemon_job_failed(&semantic_job),
        });
    }

    let history_refresh_job =
        if daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
            run_daemon_history_refresh_job(data_root)
        } else {
            Ok(daemon_history_refresh_skipped_job("daemon_deadline"))
        };
    let history_refresh_job = match history_refresh_job {
        Ok(value) => value,
        Err(error) => daemon_history_refresh_failed_job(format!("{error:#}")),
    };
    let history_refresh_did_work = daemon_history_refresh_job_did_work(&history_refresh_job);
    runtime.semantic_bootstrap_passes_since_refresh = 0;
    write_daemon_job_status_unless_deadline_skip(
        &daemon_history_refresh_job_path(data_root),
        &history_refresh_job,
    )?;

    let semantic_job = if daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
        run_daemon_semantic_job(args, data_root, runtime, deadline, semantic_enabled)
    } else {
        Ok(daemon_semantic_deadline_skipped_job(data_root))
    };
    let semantic_job = match semantic_job {
        Ok(value) => value,
        Err(error) => daemon_semantic_failed_job(data_root, format!("{error:#}")),
    };
    let semantic_did_work = daemon_semantic_job_did_work(&semantic_job);
    write_daemon_job_status_unless_deadline_skip(
        &daemon_semantic_job_path(data_root),
        &semantic_job,
    )?;

    let cloud_sync_job = daemon_cloud_sync_disabled_job(Some(utc_now().timestamp_millis()));
    write_daemon_job_status(&daemon_cloud_sync_job_path(data_root), &cloud_sync_job)?;

    Ok(DaemonIteration {
        did_work: history_refresh_did_work || semantic_did_work,
        failed: daemon_job_failed(&history_refresh_job) || daemon_job_failed(&semantic_job),
    })
}

fn semantic_bootstrap_should_run_first(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
) -> Result<bool> {
    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        return Ok(false);
    }
    if runtime.semantic_bootstrap_passes_since_refresh
        >= DAEMON_SEMANTIC_BOOTSTRAP_PASSES_BEFORE_REFRESH
    {
        return Ok(false);
    }
    let store = Store::open(&db_path).context("open ctx store for daemon semantic bootstrap")?;
    refresh_semantic_document_count_cache(&store)?;
    let report = semantic_worker_report(data_root, Some(&store))?;
    Ok(report.searchable_items > 0
        && report.queued_items_estimate > 0
        && report.model_cache_available)
}

fn semantic_report_should_queue_recent_work(report: &SemanticWorkerReport) -> bool {
    report.searchable_items > 0
        && report.embedded_items >= report.searchable_items
        && report.dirty_items == 0
}

fn refresh_semantic_document_count_cache(store: &Store) -> Result<()> {
    store.refresh_event_embedding_document_count_cache()?;
    Ok(())
}

fn daemon_semantic_job_did_work(value: &Value) -> bool {
    value
        .get("indexed_chunks")
        .and_then(Value::as_u64)
        .is_some_and(|chunks| chunks > 0)
}

fn daemon_run_start_mode(args: &DaemonRunArgs) -> DaemonStartModeArg {
    args.start_mode.unwrap_or(DaemonStartModeArg::Manual)
}

fn daemon_job_failed(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("failed")
}

fn write_daemon_job_status_unless_deadline_skip(path: &Path, value: &Value) -> Result<()> {
    if daemon_job_skipped_for_deadline(value) && path.exists() {
        return Ok(());
    }
    write_daemon_job_status(path, value)
}

fn daemon_job_skipped_for_deadline(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("skipped")
        && value.get("reason").and_then(Value::as_str) == Some("daemon_deadline")
}

fn daemon_deadline_remaining(deadline: Option<Instant>) -> Option<StdDuration> {
    deadline.and_then(|deadline| deadline.checked_duration_since(Instant::now()))
}

fn daemon_deadline_has_min_budget(deadline: Option<Instant>, min_secs: u64) -> bool {
    let Some(remaining) = daemon_deadline_remaining(deadline) else {
        return deadline.is_none();
    };
    remaining >= StdDuration::from_secs(min_secs)
}

fn run_daemon_history_refresh_job(data_root: &Path) -> Result<Value> {
    #[cfg(all(test, ctx_sqlite_vec))]
    if let Some(value) = daemon_test_job("history_refresh") {
        return Ok(value);
    }

    let last_run_at_ms = utc_now().timestamp_millis();
    let sources = search_refresh_sources(None);
    let plugin_sources = search_refresh_plugin_sources(
        data_root,
        None,
        &crate::search_filters::SourceIdentityFilters::default(),
    )?;
    let source_count = sources.len().saturating_add(plugin_sources.len());
    if source_count == 0 {
        return Ok(daemon_history_refresh_job_json(
            "skipped",
            0,
            ImportTotals::default(),
            last_run_at_ms,
            Some("no_sources"),
            None,
        ));
    }
    let source_fingerprint = search_refresh_source_fingerprint(&sources);
    let mut job = match refresh_sources_for_search(
        data_root,
        sources,
        plugin_sources,
        RefreshArg::Background,
        true,
    ) {
        Ok(totals) => daemon_history_refresh_job_json(
            "completed",
            source_count,
            totals,
            last_run_at_ms,
            None,
            None,
        ),
        Err(error) => daemon_history_refresh_job_json(
            "failed",
            source_count,
            ImportTotals::default(),
            last_run_at_ms,
            None,
            Some(error_summary(&error)),
        ),
    };
    if let Some(map) = job.as_object_mut() {
        map.insert("source_fingerprint".to_owned(), json!(source_fingerprint));
        map.insert("passes".to_owned(), json!(1));
    }
    Ok(job)
}

fn daemon_history_refresh_skipped_job(reason: &str) -> Value {
    daemon_history_refresh_job_json(
        "skipped",
        0,
        ImportTotals::default(),
        utc_now().timestamp_millis(),
        Some(reason),
        None,
    )
}

fn daemon_history_refresh_failed_job(message: String) -> Value {
    daemon_history_refresh_job_json(
        "failed",
        0,
        ImportTotals::default(),
        utc_now().timestamp_millis(),
        None,
        Some(message),
    )
}

fn daemon_history_refresh_job_json(
    status: &str,
    source_count: usize,
    totals: ImportTotals,
    last_run_at_ms: i64,
    reason: Option<&str>,
    last_error: Option<String>,
) -> Value {
    compact_json(json!({
        "mode": RefreshArg::Background.as_str(),
        "status": status,
        "source_count": source_count,
        "totals": import_totals_json(&totals),
        "reason": reason,
        "last_run_at_ms": last_run_at_ms,
        "last_error": last_error,
    }))
}

fn daemon_history_refresh_job_did_work(value: &Value) -> bool {
    let Some(totals) = value.get("totals") else {
        return false;
    };
    ["imported_sessions", "imported_events", "imported_edges"]
        .into_iter()
        .any(|key| totals.get(key).and_then(Value::as_u64).unwrap_or(0) > 0)
}

fn search_refresh_source_fingerprint(sources: &[crate::provider_sources::SourceInfo]) -> String {
    let mut items = sources
        .iter()
        .map(|source| {
            format!(
                "{}|{}|{}",
                source.provider.as_str(),
                source.source_format,
                source.path.display()
            )
        })
        .collect::<Vec<_>>();
    items.sort();
    semantic_text_hash(&items.join("\n"))
}

fn run_daemon_semantic_job(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
) -> Result<Value> {
    let last_run_at_ms = utc_now().timestamp_millis();
    if !semantic_enabled {
        let report = semantic_worker_report_best_effort(data_root);
        return Ok(daemon_semantic_job_json(
            "disabled",
            Some("semantic_disabled"),
            last_run_at_ms,
            &report,
            None,
            None,
        ));
    }

    #[cfg(all(test, ctx_sqlite_vec))]
    if let Some(value) = daemon_test_job("semantic_index") {
        return Ok(value);
    }

    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        let report = semantic_worker_report_best_effort(data_root);
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("store_missing"),
            last_run_at_ms,
            &report,
            None,
            None,
        ));
    }

    let store = Store::open(&db_path).context("open ctx store for daemon semantic job")?;
    refresh_semantic_document_count_cache(&store)?;
    let mut before = semantic_worker_report(data_root, Some(&store))?;
    if semantic_report_should_queue_recent_work(&before)
        && queue_recent_semantic_work(data_root, &store, "daemon_recent").unwrap_or(0) > 0
    {
        before = semantic_worker_report(data_root, Some(&store))?;
    }
    if before.searchable_items == 0 {
        return Ok(daemon_semantic_job_json(
            "empty",
            Some("no_searchable_items"),
            last_run_at_ms,
            &before,
            None,
            None,
        ));
    }
    let min_remaining_secs = if daemon_runtime_embedder_loaded(runtime) {
        DAEMON_MIN_REMAINING_FOR_JOB_SECS
    } else {
        SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS
    }
    .saturating_add(DAEMON_SEMANTIC_RESERVE_GRACE_SECS);
    if !daemon_deadline_has_min_budget(deadline, min_remaining_secs) {
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("daemon_deadline"),
            last_run_at_ms,
            &before,
            None,
            None,
        ));
    }
    if semantic_daemon_model_load_needed(&before, daemon_runtime_embedder_loaded(runtime)) {
        let cache_dir = semantic_worker_cache_dir(data_root);
        let _ = write_semantic_model_acquisition_status(data_root, "acquiring_model", None);
        match acquire_semantic_embedder(&cache_dir) {
            Ok(embedder) => {
                *lock_daemon_runtime_embedder(runtime)? = Some(embedder);
                let embedding_runtime =
                    shared_semantic_embedder_runtime_status_json(&runtime.semantic_embedder)?;
                let embed_policy =
                    shared_semantic_embedder_policy_status_json(&runtime.semantic_embedder)?;
                let _ = write_semantic_model_acquired_status(
                    data_root,
                    embedding_runtime,
                    embed_policy,
                );
                before = semantic_worker_report(data_root, Some(&store))?;
            }
            Err(error) if error.downcast_ref::<SemanticModelLoadDeferred>().is_some() => {
                let deferred = error
                    .downcast_ref::<SemanticModelLoadDeferred>()
                    .expect("matched semantic model load deferral");
                let _ = write_semantic_model_load_deferred_status(data_root, deferred);
                let report = semantic_worker_report(data_root, Some(&store))?;
                return Ok(daemon_semantic_model_load_deferred_job(
                    last_run_at_ms,
                    &report,
                    deferred,
                ));
            }
            Err(error) => {
                let message = format!("{error:#}");
                let integrity_failure = semantic_model_acquisition_integrity_error(&error);
                let failure_code = if integrity_failure {
                    "model_integrity_failed"
                } else {
                    "model_acquisition_failed"
                };
                let _ = write_semantic_model_acquisition_status(
                    data_root,
                    failure_code,
                    Some(message.clone()),
                );
                return Ok(daemon_semantic_job_json(
                    "skipped",
                    Some(failure_code),
                    last_run_at_ms,
                    &before,
                    None,
                    Some(message),
                ));
            }
        }
    }
    if before.queued_items_estimate == 0 {
        return Ok(daemon_semantic_job_json(
            "ready",
            None,
            last_run_at_ms,
            &before,
            None,
            None,
        ));
    }
    drop(store);

    let worker_max_seconds = daemon_semantic_worker_seconds_budget(args, deadline);
    if worker_max_seconds == 0 {
        let report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("daemon_deadline"),
            last_run_at_ms,
            &report,
            None,
            None,
        ));
    }
    let worker_args = SemanticWorkerArgs {
        max_chunks: args.max_chunks,
        max_seconds: Some(worker_max_seconds),
    };
    let worker_result = run_semantic_worker_inner_with_embedder(
        worker_args,
        data_root,
        None,
        &runtime.semantic_embedder,
    );
    if let Err(error) = worker_result {
        if let Some(deferred) = error.downcast_ref::<SemanticModelLoadDeferred>() {
            let _ = write_semantic_model_load_deferred_status(data_root, deferred);
            let report = semantic_worker_report_for_daemon(data_root);
            return Ok(daemon_semantic_model_load_deferred_job(
                last_run_at_ms,
                &report,
                deferred,
            ));
        }
        let message = format!("{error:#}");
        let _ = write_semantic_worker_failure_status(data_root, message.clone());
        let report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_semantic_job_json(
            "failed",
            None,
            last_run_at_ms,
            &report,
            None,
            Some(message),
        ));
    }
    let report = semantic_worker_report_for_daemon(data_root);
    let indexed_chunks_now = report
        .embedded_chunks
        .saturating_sub(before.embedded_chunks);
    let indexed_chunks = (indexed_chunks_now > 0).then_some(indexed_chunks_now);
    let status = if report.running {
        "running"
    } else if report.queued_items_estimate == 0 {
        "ready"
    } else if indexed_chunks_now > 0 {
        "budget_exhausted"
    } else {
        report.status.as_str()
    };
    Ok(daemon_semantic_job_json(
        status,
        None,
        last_run_at_ms,
        &report,
        indexed_chunks,
        None,
    ))
}

fn daemon_semantic_requested_seconds(args: &DaemonRunArgs) -> u64 {
    semantic_worker_seconds_budget(&SemanticWorkerArgs {
        max_chunks: args.max_chunks,
        max_seconds: args.max_seconds,
    })
}

fn semantic_daemon_model_load_needed(
    report: &SemanticWorkerReport,
    runtime_loaded: bool,
) -> bool {
    report.searchable_items > 0 && !runtime_loaded
}

fn daemon_semantic_worker_seconds_budget(args: &DaemonRunArgs, deadline: Option<Instant>) -> u64 {
    let requested = daemon_semantic_requested_seconds(args);
    let Some(remaining) = daemon_deadline_remaining(deadline) else {
        return if deadline.is_none() { requested } else { 0 };
    };
    let remaining_secs = remaining
        .as_secs()
        .saturating_sub(DAEMON_SEMANTIC_RESERVE_GRACE_SECS);
    requested.min(remaining_secs)
}

fn daemon_semantic_deadline_skipped_job(data_root: &Path) -> Value {
    let report = semantic_worker_report_for_daemon(data_root);
    daemon_semantic_job_json(
        "skipped",
        Some("daemon_deadline"),
        utc_now().timestamp_millis(),
        &report,
        None,
        None,
    )
}

fn daemon_semantic_failed_job(data_root: &Path, message: String) -> Value {
    let report = semantic_worker_report_for_daemon(data_root);
    daemon_semantic_job_json(
        "failed",
        None,
        utc_now().timestamp_millis(),
        &report,
        None,
        Some(message),
    )
}

fn daemon_semantic_job_json(
    status: &str,
    reason: Option<&str>,
    last_run_at_ms: i64,
    report: &SemanticWorkerReport,
    indexed_chunks: Option<usize>,
    last_error: Option<String>,
) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "status": status,
        "model_key": semantic_model_key(),
        "enabled": true,
        "reason": reason,
        "last_run_at_ms": last_run_at_ms,
        "last_error": last_error,
        "indexed_chunks": indexed_chunks,
        "model_cache_available": report.model_cache_available,
        "model_acquisition": report.model_acquisition.clone(),
        "embed_policy": report.embed_policy.clone(),
        "embedding_runtime": report.embedding_runtime.clone(),
        "worker_status": report.status,
        "coverage": {
            "searchable_items": report.searchable_items,
            "completed_items": report.embedded_items,
            "embedded_items": report.embedded_items,
            "embedded_chunks": report.embedded_chunks,
            "dirty_items": report.dirty_items,
            "queued_items_estimate": report.queued_items_estimate,
        },
    }))
}

fn daemon_semantic_model_load_deferred_job(
    last_run_at_ms: i64,
    report: &SemanticWorkerReport,
    deferred: &SemanticModelLoadDeferred,
) -> Value {
    let mut value = daemon_semantic_job_json(
        "skipped",
        Some("memory_pressure"),
        last_run_at_ms,
        report,
        None,
        None,
    );
    value["retryable"] = Value::Bool(true);
    value["available_memory_bytes"] = json!(deferred.available_memory_bytes);
    value["required_available_memory_bytes"] =
        json!(deferred.required_available_memory_bytes);
    compact_json(value)
}

fn daemon_cloud_sync_disabled_job(last_run_at_ms: Option<i64>) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "status": "disabled",
        "enabled": false,
        "reason": "not_configured",
        "network_allowed": false,
        "last_run_at_ms": last_run_at_ms,
        "last_upload_at_ms": Value::Null,
        "queued_items_estimate": 0,
        "last_error": Value::Null,
    }))
}

fn write_daemon_lifecycle_status(
    data_root: &Path,
    args: &DaemonRunArgs,
    status: &str,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    last_error: Option<String>,
) -> Result<()> {
    write_daemon_status(
        data_root,
        &compact_json(json!({
            "schema_version": 1,
            "status": status,
            "pid": process::id(),
            "started_at_ms": started_at_ms,
            "heartbeat_at_ms": utc_now().timestamp_millis(),
            "finished_at_ms": finished_at_ms,
            "start_mode": daemon_run_start_mode(args).as_str(),
            "trigger_command": args.trigger_command.map(DaemonTriggerCommandArg::as_str),
            "last_error": last_error,
        })),
    )
}

fn semantic_worker_report_for_daemon(data_root: &Path) -> SemanticWorkerReport {
    let db_path = database_path(data_root.to_path_buf());
    if db_path.exists() {
        match open_existing_store_read_only(&db_path, "ctx daemon status") {
            Ok(store) => {
                return semantic_worker_report_cached(data_root, Some(&store)).unwrap_or_else(|error| {
                    SemanticWorkerReport::unavailable(data_root, format!("{error:#}"))
                });
            }
            Err(error) => {
                return SemanticWorkerReport::unavailable(data_root, format!("{error:#}"));
            }
        }
    }
    semantic_worker_report_best_effort(data_root)
}

fn write_semantic_worker_failure_status(data_root: &Path, message: String) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": "failed",
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "last_error": message,
            "model_acquisition": semantic_model_acquisition_status_json(
                &semantic_worker_cache_dir(data_root),
            ),
            "embed_policy": semantic_embed_policy_status_json(),
        }),
    )
}

fn write_semantic_model_acquisition_status(
    data_root: &Path,
    status: &str,
    message: Option<String>,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": status,
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": matches!(
                status,
                "model_acquisition_failed" | "model_integrity_failed"
            )
            .then_some(now),
            "last_error": message,
            "model_acquisition": semantic_model_acquisition_status_json(
                &semantic_worker_cache_dir(data_root),
            ),
            "embed_policy": semantic_embed_policy_status_json(),
        }),
    )
}

fn write_semantic_model_load_deferred_status(
    data_root: &Path,
    deferred: &SemanticModelLoadDeferred,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &compact_json(json!({
            "schema_version": 1,
            "status": "model_load_deferred",
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "last_error": null,
            "retryable": true,
            "available_memory_bytes": deferred.available_memory_bytes,
            "required_available_memory_bytes": deferred.required_available_memory_bytes,
            "model_acquisition": semantic_model_acquisition_status_json(
                &semantic_worker_cache_dir(data_root),
            ),
            "embed_policy": semantic_embed_policy_status_json(),
        })),
    )
}

fn write_semantic_model_acquired_status(
    data_root: &Path,
    embedding_runtime: Option<Value>,
    embed_policy: Value,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": "model_acquired",
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "model_acquisition": semantic_model_acquisition_status_json(
                &semantic_worker_cache_dir(data_root),
            ),
            "embedding_runtime": embedding_runtime,
            "embed_policy": embed_policy,
        }),
    )
}

fn run_semantic_worker_inner_with_embedder(
    args: SemanticWorkerArgs,
    data_root: &Path,
    query_hint: Option<String>,
    embedder: &Arc<Mutex<Option<SemanticEmbedder>>>,
) -> Result<()> {
    let Some(_lock) = SemanticWorkerLock::acquire(data_root)? else {
        return Ok(());
    };

    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        return Err(anyhow!(
            "ctx index does not exist yet; run `ctx import --all` or `ctx setup` first"
        ));
    }
    let cache_dir = semantic_worker_cache_dir(data_root);
    if lock_shared_semantic_embedder(embedder)?.is_none()
        && !semantic_model_cache_available(&cache_dir)
    {
        return Err(anyhow!(
            "semantic model is not available in the local cache; background indexing will not initialize or download {SEMANTIC_MODEL_ID}"
        ));
    }
    let store = Store::open(&db_path).context("open ctx store for semantic worker")?;
    refresh_semantic_document_count_cache(&store)?;
    let vector_path = semantic_vector_path(data_root);
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    let prune_outcome = vector_store.prune_ineligible_events(&store)?;
    let started_at_ms = utc_now().timestamp_millis();
    let initial_stats = vector_store
        .cached_stats()?
        .unwrap_or_else(SemanticSidecarStats::default);
    let initial_dirty_items = vector_store.dirty_event_count()?;
    let searchable_items = store.event_embedding_document_count_cached_or_exact()?;
    let initial_queued_items_estimate = searchable_items
        .saturating_sub(initial_stats.embedded_items)
        .max(initial_dirty_items);
    let was_ready_before_worker =
        semantic_worker_status_was_ready_for_stats(data_root, initial_stats);
    let continue_past_indexed_pages = !was_ready_before_worker
        || initial_queued_items_estimate > SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT;
    let starting_embed_policy = shared_semantic_embedder_policy_status_json(embedder)?;
    let starting_embedding_runtime = shared_semantic_embedder_runtime_status_json(embedder)?;
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": "running",
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "started_at_ms": started_at_ms,
            "heartbeat_at_ms": started_at_ms,
            "indexed_chunks": 0,
            "pruned_chunks": prune_outcome.deleted_chunks,
            "stale_events_queued": prune_outcome.queued_stale_events,
            "searchable_items": searchable_items,
            "embedded_items": initial_stats.embedded_items,
            "embedded_chunks": initial_stats.embedded_chunks,
            "dirty_items": initial_dirty_items,
            "embed_policy": starting_embed_policy,
            "embedding_runtime": starting_embedding_runtime,
            "last_error": null,
        }),
    )?;
    let max_chunks = semantic_worker_chunk_budget(&args);
    let max_seconds = semantic_worker_seconds_budget(&args);
    let started = Instant::now();
    let deadline = started + StdDuration::from_secs(max_seconds);
    let mut model_init_ms = None;
    let indexed_chunks = if Instant::now() >= deadline {
        0
    } else {
        backfill_semantic_embeddings(
            &store,
            &mut vector_store,
            embedder,
            &mut model_init_ms,
            &cache_dir,
            query_hint.as_deref(),
            max_chunks,
            true,
            continue_past_indexed_pages,
            Some(deadline),
        )?
    };
    let elapsed = started.elapsed();
    let finished_embed_policy = shared_semantic_embedder_policy_status_json(embedder)?;
    let finished_embedding_runtime = shared_semantic_embedder_runtime_status_json(embedder)?;
    let elapsed_ms = elapsed.as_millis() as u64;
    let final_stats = vector_store
        .cached_stats()?
        .unwrap_or_else(SemanticSidecarStats::default);
    let final_dirty_items = vector_store.dirty_event_count()?;
    refresh_semantic_document_count_cache(&store)?;
    let searchable_items = store.event_embedding_document_count_cached_or_exact()?;
    let status = if searchable_items > 0
        && final_stats.embedded_items >= searchable_items
        && final_dirty_items == 0
    {
        vector_store.set_backfill_cursor(None)?;
        "ready"
    } else if elapsed >= StdDuration::from_secs(max_seconds) {
        "budget_exhausted"
    } else {
        "completed"
    };
    let finished_at_ms = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": status,
            "model_key": semantic_model_key(),
            "pid": process::id(),
            "started_at_ms": started_at_ms,
            "heartbeat_at_ms": finished_at_ms,
            "finished_at_ms": finished_at_ms,
            "indexed_chunks": indexed_chunks,
            "pruned_chunks": prune_outcome.deleted_chunks,
            "stale_events_queued": prune_outcome.queued_stale_events,
            "elapsed_ms": elapsed_ms,
            "model_init_ms": model_init_ms,
            "searchable_items": searchable_items,
            "embedded_items": final_stats.embedded_items,
            "embedded_chunks": final_stats.embedded_chunks,
            "dirty_items": final_dirty_items,
            "embed_policy": finished_embed_policy,
            "embedding_runtime": finished_embedding_runtime,
            "last_error": null,
        }),
    )?;
    drop(_lock);
    Ok(())
}

fn semantic_worker_chunk_budget(args: &SemanticWorkerArgs) -> usize {
    args.max_chunks
        .or_else(|| env_usize("CTX_SEMANTIC_WORKER_MAX_CHUNKS"))
        .map(|value| value.min(SEMANTIC_WORKER_BATCH_MAX))
        .unwrap_or(SEMANTIC_WORKER_BATCH_DEFAULT)
}

fn semantic_worker_seconds_budget(args: &SemanticWorkerArgs) -> u64 {
    args.max_seconds
        .or_else(|| {
            env::var("CTX_SEMANTIC_WORKER_MAX_SECONDS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
        })
        .map(|value| value.min(SEMANTIC_WORKER_MAX_SECONDS_CAP))
        .unwrap_or(SEMANTIC_WORKER_MAX_SECONDS_DEFAULT)
}

fn semantic_worker_status_was_ready_for_stats(
    data_root: &Path,
    stats: SemanticSidecarStats,
) -> bool {
    let Some(value) = read_semantic_worker_status(data_root) else {
        return false;
    };
    if !semantic_status_file_model_matches(Some(&value)) {
        return false;
    }
    let status_ready = json_string(&value, "status").is_some_and(|status| status == "ready");
    let dirty_items = json_usize(&value, "dirty_items").unwrap_or(usize::MAX);
    let embedded_items = json_usize(&value, "embedded_items").unwrap_or(0);
    let searchable_items = json_usize(&value, "searchable_items").unwrap_or(usize::MAX);
    status_ready
        && dirty_items == 0
        && embedded_items == stats.embedded_items
        && embedded_items >= searchable_items
}

fn queue_recent_semantic_work(data_root: &Path, store: &Store, reason: &str) -> Result<usize> {
    let vector_path = semantic_vector_path(data_root);
    if !vector_path.exists()
        && !semantic_model_cache_available(&semantic_worker_cache_dir(data_root))
    {
        return Ok(0);
    }
    let docs = store.recent_event_embedding_documents(None, SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT)?;
    if docs.is_empty() {
        return Ok(0);
    }
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    let existing_hashes = vector_store
        .existing_hashes_for_event_ids(&docs.iter().map(|doc| doc.event_id).collect::<Vec<_>>())?;
    let docs = docs
        .into_iter()
        .filter(|doc| {
            let source_text = semantic_source_text(&doc.text);
            let hash = semantic_document_hash(doc, &source_text);
            existing_hashes
                .get(&doc.event_id)
                .map(|existing| existing != &hash)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    vector_store.enqueue_dirty_documents(&docs, reason)
}

pub(crate) fn maybe_autostart_daemon(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
    json_output: bool,
) {
    maybe_autostart_daemon_inner(data_root, config, trigger, json_output, false);
}

pub(crate) fn maybe_autostart_daemon_for_search(data_root: &Path, config: &AppConfig) {
    maybe_autostart_daemon_inner(
        data_root,
        config,
        DaemonTriggerCommandArg::Search,
        false,
        true,
    );
}

fn maybe_autostart_daemon_inner(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
    json_output: bool,
    allow_json_output: bool,
) {
    if semantic_env_flag(DAEMON_BACKGROUND_CHILD_ENV) {
        return;
    }
    if !database_path(data_root.to_path_buf()).exists() {
        return;
    }
    if !config.daemon.enabled {
        return;
    }
    if semantic_env_flag(DAEMON_AUTOSTART_OFF_ENV) {
        return;
    }
    if json_output && !allow_json_output {
        return;
    }
    if semantic_env_flag("CI") {
        return;
    }
    let lock_path = daemon_lock_path(data_root);
    if lock_path.exists() && !daemon_lock_is_stale(&lock_path) {
        return;
    }
    let exe = match daemon_autostart_exe() {
        Ok(exe) => exe,
        Err(error) => {
            let _ = write_daemon_autostart_status(
                data_root,
                trigger,
                "failed",
                Some("current_exe"),
                Some(format!("{error:#}")),
                None,
            );
            return;
        }
    };
    let idle_exit = daemon_autostart_u64_env(
        "CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS",
        DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT,
        DAEMON_IDLE_EXIT_SECONDS_CAP,
    );
    let loop_interval = daemon_autostart_u64_env(
        "CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS",
        DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT,
        3_600,
    );
    match Command::new(exe)
        .arg("--data-root")
        .arg(data_root)
        .arg("daemon")
        .arg("run")
        .arg("--idle-exit-seconds")
        .arg(idle_exit.to_string())
        .arg("--loop-interval-seconds")
        .arg(loop_interval.to_string())
        .arg("--start-mode")
        .arg(DaemonStartModeArg::Auto.as_str())
        .arg("--trigger-command")
        .arg(trigger.as_str())
        .arg("--json")
        .env(DAEMON_BACKGROUND_CHILD_ENV, "1")
        .env("CTX_ANALYTICS_OFF", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_child) => {}
        Err(error) => {
            let _ = write_daemon_autostart_status(
                data_root,
                trigger,
                "failed",
                Some("spawn_failed"),
                Some(error.to_string()),
                None,
            );
        }
    }
}

pub(crate) fn semantic_query_service_supported() -> bool {
    cfg!(ctx_semantic_fastembed)
}

pub(crate) fn daemon_query_service_available(data_root: &Path) -> bool {
    let response = daemon_query_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "ping",
        })),
        StdDuration::from_secs(1),
        1024,
    );
    response
        .ok()
        .flatten()
        .and_then(|value| value.get("ok").and_then(Value::as_bool))
        == Some(true)
}

pub(crate) fn wait_for_daemon_query_service(data_root: &Path, timeout: StdDuration) -> bool {
    if !semantic_query_service_supported() {
        return false;
    }
    let started = Instant::now();
    loop {
        if daemon_query_service_available(data_root) {
            return true;
        }
        if started.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(StdDuration::from_millis(100));
    }
}

fn daemon_autostart_exe() -> Result<PathBuf> {
    env::var("CTX_DAEMON_AUTOSTART_EXE")
        .ok()
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| env::current_exe().context("resolve ctx daemon autostart executable"))
}

fn write_daemon_autostart_status(
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    status: &str,
    reason: Option<&str>,
    last_error: Option<String>,
    pid: Option<u32>,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_daemon_status(
        data_root,
        &compact_json(json!({
            "schema_version": 1,
            "status": status,
            "reason": reason,
            "pid": pid,
            "started_at_ms": Value::Null,
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "start_mode": DaemonStartModeArg::Auto.as_str(),
            "trigger_command": trigger.as_str(),
            "last_error": last_error,
        })),
    )
}

fn daemon_autostart_u64_env(name: &str, default: u64, max: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(max))
        .unwrap_or(default)
}
