mod support;

use support::*;

use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command as StdCommand, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    thread::JoinHandle,
};

struct OfflineSourceRefreshDaemon {
    child: Option<Child>,
}

struct IsolatedProcessEnvironment {
    home: PathBuf,
    xdg_config: PathBuf,
    xdg_data: PathBuf,
    xdg_state: PathBuf,
    local_app_data: PathBuf,
}

impl IsolatedProcessEnvironment {
    fn new(root: &Path) -> Self {
        let environment = Self {
            home: root.join("home"),
            xdg_config: root.join("xdg-config"),
            xdg_data: root.join("xdg-data"),
            xdg_state: root.join("xdg-state"),
            local_app_data: root.join("local-app-data"),
        };
        for (_, path) in environment.variables() {
            fs::create_dir_all(path).unwrap();
        }
        environment
    }

    fn variables(&self) -> [(&'static str, &Path); 5] {
        [
            ("HOME", &self.home),
            ("XDG_CONFIG_HOME", &self.xdg_config),
            ("XDG_DATA_HOME", &self.xdg_data),
            ("XDG_STATE_HOME", &self.xdg_state),
            ("LOCALAPPDATA", &self.local_app_data),
        ]
    }
}

struct ScriptedAnalyticsServer {
    requests: Receiver<Result<Value, String>>,
    stop: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl ScriptedAnalyticsServer {
    fn start(listener: TcpListener) -> Self {
        let (request_tx, requests) = mpsc::channel();
        let (stop, stop_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || loop {
            match stop_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_http_json_request(&mut stream);
                    if request_tx.send(request).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    match stop_rx.recv_timeout(Duration::from_millis(10)) {
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
                Err(error) => {
                    let _ = request_tx.send(Err(format!("accept analytics request: {error}")));
                    break;
                }
            }
        });
        Self {
            requests,
            stop: Some(stop),
            worker: Some(worker),
        }
    }

    fn wait_for_operation(
        &self,
        daemon: &mut OfflineSourceRefreshDaemon,
        operation: &str,
        expected_event_id: uuid::Uuid,
    ) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for daemon analytics event {expected_event_id}"
            );
            match self
                .requests
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(Ok(payload)) => {
                    if analytics_operation_event_id(std::slice::from_ref(&payload), operation)
                        == Some(expected_event_id)
                    {
                        return payload;
                    }
                }
                Ok(Err(error)) => panic!("scripted analytics server failed: {error}"),
                Err(RecvTimeoutError::Timeout) => daemon.assert_running(),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("scripted analytics server stopped before delivery")
                }
            }
        }
    }
}

impl Drop for ScriptedAnalyticsServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                if std::thread::panicking() {
                    eprintln!("scripted analytics server also panicked during teardown");
                } else {
                    panic!("scripted analytics server panicked");
                }
            }
        }
    }
}

impl Drop for OfflineSourceRefreshDaemon {
    fn drop(&mut self) {
        if let Err(error) =
            terminate_and_reap_test_child(&mut self.child, "offline source-refresh daemon")
        {
            if std::thread::panicking() {
                eprintln!("offline daemon teardown also failed: {error}");
            } else {
                panic!("offline daemon teardown failed: {error}");
            }
        }
    }
}

impl OfflineSourceRefreshDaemon {
    fn assert_running(&mut self) {
        let child = self.child.as_mut().unwrap();
        let Some(exit) = child.try_wait().unwrap() else {
            return;
        };
        let mut stderr = String::new();
        child
            .stderr
            .as_mut()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        panic!("source-refresh daemon exited before analytics delivery ({exit}): {stderr}");
    }
}

fn read_http_json_request(stream: &mut TcpStream) -> Result<Value, String> {
    let payload = read_http_json_request_body(stream)?;
    stream
        .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .map_err(|error| format!("write analytics response: {error}"))?;
    Ok(payload)
}

fn read_http_json_request_body(stream: &mut TcpStream) -> Result<Value, String> {
    const MAX_REQUEST_BYTES: usize = 1024 * 1024;

    stream
        .set_nonblocking(false)
        .map_err(|error| format!("make accepted analytics stream blocking: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("set analytics request timeout: {error}"))?;
    let mut request = Vec::new();
    let (body_start, content_length) = loop {
        let mut chunk = [0_u8; 4096];
        let size = stream
            .read(&mut chunk)
            .map_err(|error| format!("read analytics request: {error}"))?;
        if size == 0 {
            return Err("analytics request ended before its headers".to_owned());
        }
        request.extend_from_slice(&chunk[..size]);
        if request.len() > MAX_REQUEST_BYTES {
            return Err("analytics request exceeded test bound".to_owned());
        }
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let body_start = header_end + 4;
        let headers = std::str::from_utf8(&request[..header_end])
            .map_err(|error| format!("analytics request headers are not UTF-8: {error}"))?;
        let content_length = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .ok_or_else(|| "analytics request omitted Content-Length".to_owned())?
            .1
            .trim()
            .parse::<usize>()
            .map_err(|error| format!("invalid analytics Content-Length: {error}"))?;
        if body_start.saturating_add(content_length) > MAX_REQUEST_BYTES {
            return Err("analytics request body exceeded test bound".to_owned());
        }
        break (body_start, content_length);
    };
    while request.len() < body_start + content_length {
        let mut chunk = [0_u8; 4096];
        let size = stream
            .read(&mut chunk)
            .map_err(|error| format!("read analytics request body: {error}"))?;
        if size == 0 {
            return Err("analytics request ended before its body".to_owned());
        }
        request.extend_from_slice(&chunk[..size]);
        if request.len() > MAX_REQUEST_BYTES {
            return Err("analytics request exceeded test bound".to_owned());
        }
    }
    serde_json::from_slice(&request[body_start..body_start + content_length])
        .map_err(|error| format!("parse analytics request body: {error}"))
}

fn write_network_endpoints(data_root: &Path, endpoint: &str, analytics_enabled: Option<bool>) {
    fs::create_dir_all(data_root).unwrap();
    let enabled = analytics_enabled
        .map(|enabled| format!("enabled = {enabled}\n"))
        .unwrap_or_default();
    fs::write(
        data_root.join("config.toml"),
        format!(
            "[analytics]\n{enabled}endpoint = \"{endpoint}\"\n\
             [daemon]\nenabled = true\nmode = \"source-refresh-only\"\n\
             [search]\nsemantic = false\n\
             [upgrade]\nauto = \"off\"\n"
        ),
    )
    .unwrap();
}

fn local_command(temp: &TempDir, data_root: &Path) -> Command {
    let mut command = ctx(temp);
    command
        .env("CTX_DATA_ROOT", data_root)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env_remove("CTX_ANALYTICS_ENDPOINT")
        .env_remove("CTX_UPGRADE_AUTO")
        // Stale variables from the deleted history-upload prototype must be
        // inert and must not make a local command network-capable.
        .env("CTX_CLOUD_MODE", "local_and_cloud")
        .env("CTX_CLOUD_TOKEN", "stale-token")
        .env("CTX_CLOUD_API_BASE", "http://127.0.0.1:9");
    command
}

fn isolated_local_command(
    temp: &TempDir,
    data_root: &Path,
    environment: &IsolatedProcessEnvironment,
) -> Command {
    let mut command = local_command(temp, data_root);
    for (name, path) in environment.variables() {
        command.env(name, path);
    }
    command
}

fn spawn_isolated_source_refresh_daemon(
    temp: &TempDir,
    data_root: &Path,
    environment: &IsolatedProcessEnvironment,
) -> OfflineSourceRefreshDaemon {
    bind_test_ctx_binary(temp);
    let prepared = isolated_local_command(temp, data_root, environment);
    let mut command = StdCommand::new(prepared.get_program());
    for (name, value) in prepared.get_envs() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    command
        .current_dir(temp.path())
        .args(["daemon", "run", "--force", "--loop-interval-seconds", "600"])
        .env("CTX_DAEMON_MODE", "source-refresh-only")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .unwrap_or_else(|error| panic!("start isolated analytics daemon: {error}"));
    OfflineSourceRefreshDaemon { child: Some(child) }
}

fn assert_listener_received_no_connection(listener: &TcpListener, operation: &str) {
    match listener.accept() {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
        Ok((stream, peer)) => {
            drop(stream);
            panic!("foreground {operation} connected to analytics endpoint from {peer}");
        }
        Err(error) => panic!("inspect analytics listener after {operation}: {error}"),
    }
}

fn assert_owner_private_outbox(path: &Path) {
    assert!(path.is_file(), "analytics outbox is not a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "analytics outbox must be owner-private");
    }
}

fn start_offline_source_refresh_daemon(
    temp: &TempDir,
    data_root: &Path,
) -> OfflineSourceRefreshDaemon {
    bind_test_ctx_binary(temp);
    let prepared = local_command(temp, data_root);
    let mut command = StdCommand::new(prepared.get_program());
    for (name, value) in prepared.get_envs() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    command
        .current_dir(temp.path())
        .args(["daemon", "run", "--force", "--loop-interval-seconds", "600"])
        .env("CTX_DAEMON_MODE", "source-refresh-only")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .unwrap_or_else(|error| panic!("start isolated offline daemon: {error}"));
    let mut daemon = OfflineSourceRefreshDaemon { child: Some(child) };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(exit) = daemon.child.as_mut().unwrap().try_wait().unwrap() {
            let mut stderr = String::new();
            daemon
                .child
                .as_mut()
                .unwrap()
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("offline daemon exited before becoming ready ({exit}): {stderr}");
        }
        let status = local_command(temp, data_root)
            .args(["daemon", "status", "--format=json"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok());
        if status.as_ref().is_some_and(|status| {
            status["daemon"]["running"] == true
                && status["daemon"]["core_refresh_endpoint"]["available"] == true
        }) {
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for offline daemon readiness: {status:#?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn local_import_status_and_daemon_are_network_inert_when_analytics_are_disabled() {
    let temp = tempdir();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let fixture_source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider-history/codex-history.jsonl");
    let fixture = temp.path().join("codex-history.jsonl");
    fs::copy(fixture_source, &fixture).unwrap();

    let import_root = temp.path().join("import");
    write_network_endpoints(&import_root, &endpoint, Some(false));
    let _daemon = start_offline_source_refresh_daemon(&temp, &import_root);
    local_command(&temp, &import_root)
        .env("CTX_CLOUD_API_BASE", &endpoint)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            fixture.to_str().unwrap(),
            "--no-daemon",
            "--progress",
            "none",
        ])
        .assert()
        .success();
    assert!(
        listener.accept().is_err(),
        "ctx import attempted a connection"
    );

    let pro_root = temp.path().join("pro");
    write_network_endpoints(&pro_root, &endpoint, Some(false));
    local_command(&temp, &pro_root)
        .env("CTX_CLOUD_API_BASE", &endpoint)
        .args(["status", "--format=json"])
        .assert()
        .success();
    assert!(
        listener.accept().is_err(),
        "ctx status Pro inspection attempted a connection"
    );

    let daemon_root = temp.path().join("daemon");
    write_network_endpoints(&daemon_root, &endpoint, Some(false));
    local_command(&temp, &daemon_root)
        .env("CTX_CLOUD_API_BASE", &endpoint)
        .args(["daemon", "disable", "--format=json"])
        .assert()
        .success();
    assert!(
        listener.accept().is_err(),
        "ctx daemon attempted a connection"
    );
}

#[test]
fn analytics_opt_in_queues_offline_and_only_the_daemon_uploads_the_same_event_id() {
    let temp = tempdir();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let data_root = temp.path().join("opted-in");
    let environment = IsolatedProcessEnvironment::new(temp.path());
    write_network_endpoints(&data_root, &endpoint, Some(true));

    isolated_local_command(&temp, &data_root, &environment)
        .args(["doctor", "--format=json"])
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();
    assert_listener_received_no_connection(&listener, "doctor");

    let outboxes = analytics_outbox_paths(temp.path());
    assert_eq!(outboxes.len(), 1, "expected one hermetic analytics outbox");
    assert_owner_private_outbox(&outboxes[0]);
    let initially_queued = read_queued_analytics_events(temp.path());
    let doctor_event_id = analytics_operation_event_id(&initially_queued, "doctor")
        .expect("queued doctor event must have an event ID");
    assert_eq!(doctor_event_id.get_version_num(), 4);

    isolated_local_command(&temp, &data_root, &environment)
        .args(["daemon", "status", "--format=json"])
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();
    assert_listener_received_no_connection(&listener, "daemon status");
    assert_eq!(
        analytics_operation_event_id(&read_queued_analytics_events(temp.path()), "doctor"),
        Some(doctor_event_id),
        "a later foreground process must retain the exact queued event ID"
    );
    assert_owner_private_outbox(&outboxes[0]);

    let server = ScriptedAnalyticsServer::start(listener);
    let mut daemon = spawn_isolated_source_refresh_daemon(&temp, &data_root, &environment);
    let delivered = server.wait_for_operation(&mut daemon, "doctor", doctor_event_id);
    assert_eq!(
        analytics_operation_event_id(std::slice::from_ref(&delivered), "doctor"),
        Some(doctor_event_id),
        "daemon delivery must preserve the queued UUIDv4 event ID"
    );
}

#[test]
fn daemon_forced_shutdown_leaves_an_in_flight_event_safely_queued() {
    let temp = tempdir();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let data_root = temp.path().join("shutdown");
    let environment = IsolatedProcessEnvironment::new(temp.path());
    write_network_endpoints(&data_root, &endpoint, Some(true));

    isolated_local_command(&temp, &data_root, &environment)
        .args(["doctor", "--format=json"])
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();
    let queued = read_queued_analytics_events(temp.path());
    let event_id = analytics_operation_event_id(&queued, "doctor").unwrap();

    let mut daemon = spawn_isolated_source_refresh_daemon(&temp, &data_root, &environment);
    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut in_flight, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                daemon.assert_running();
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for daemon uploader"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept daemon analytics request: {error}"),
        }
    };
    let attempted = read_http_json_request_body(&mut in_flight)
        .unwrap_or_else(|error| panic!("read in-flight daemon request: {error}"));
    assert_eq!(
        analytics_operation_event_id(std::slice::from_ref(&attempted), "doctor"),
        Some(event_id)
    );

    terminate_and_reap_test_child(&mut daemon.child, "in-flight analytics daemon").unwrap();
    drop(in_flight);

    assert_eq!(
        analytics_operation_event_id(&read_queued_analytics_events(temp.path()), "doctor"),
        Some(event_id),
        "an event without a complete 2xx response must survive daemon shutdown"
    );
}
