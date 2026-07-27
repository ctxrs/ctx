use super::*;

#[cfg(any(unix, windows))]
const TEST_QUERY_REQUEST_READ_TIMEOUT: StdDuration = StdDuration::from_millis(100);

#[cfg(any(unix, windows))]
fn start_test_query_service(data_root: &Path) -> Result<DaemonQueryService> {
    start_daemon_query_service_with_request_timeout(
        data_root,
        SharedSemanticRuntime::default(),
        TEST_QUERY_REQUEST_READ_TIMEOUT,
    )
}

#[cfg(any(unix, windows))]
fn wait_for_active_query(service: &DaemonQueryService) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < StdDuration::from_secs(2) {
        if service.activity.snapshot().0 == 1 {
            return Ok(());
        }
        std::thread::sleep(StdDuration::from_millis(5));
    }
    Err(anyhow!(
        "daemon query service did not accept the test client"
    ))
}

#[cfg(unix)]
fn connect_stalled_query_client(data_root: &Path) -> Result<UnixStream> {
    let endpoint = read_daemon_query_endpoint(data_root)?.expect("query endpoint");
    let DaemonQueryEndpoint::Unix { path, .. } = endpoint;
    UnixStream::connect(&path)
        .with_context(|| format!("connect test query socket {}", path.display()))
}

#[cfg(unix)]
fn connect_valid_nonreading_query_client(data_root: &Path) -> Result<UnixStream> {
    let endpoint = read_daemon_query_endpoint(data_root)?.expect("query endpoint");
    let DaemonQueryEndpoint::Unix { path, token } = endpoint;
    let mut stream = UnixStream::connect(&path)
        .with_context(|| format!("connect test query socket {}", path.display()))?;
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&compact_json(json!({
            "schema_version": 1,
            "op": "ping",
            "token": token,
        })))?
    )?;
    Ok(stream)
}

#[cfg(unix)]
#[test]
fn configured_unix_query_stream_drains_response_larger_than_socket_buffer() -> Result<()> {
    use std::io::{Read, Write};

    let (mut server, mut client) = UnixStream::pair()?;
    server.set_nonblocking(true)?;
    configure_daemon_query_stream_unix(&server, StdDuration::from_secs(2))?;
    let response = vec![b'x'; 1024 * 1024];
    let expected = response.len();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        server.write_all(&response)?;
        server.shutdown(Shutdown::Write)
    });
    std::thread::sleep(StdDuration::from_millis(25));
    let mut received = Vec::new();
    client.read_to_end(&mut received)?;
    writer.join().expect("query response writer panicked")?;
    assert_eq!(received.len(), expected);
    Ok(())
}

#[cfg(windows)]
fn connect_stalled_query_client(data_root: &Path) -> Result<WindowsQueryHandle> {
    let endpoint = read_daemon_query_endpoint(data_root)?.expect("query endpoint");
    let DaemonQueryEndpoint::WindowsNamedPipe { pipe_name, .. } = endpoint;
    let deadline = WindowsIoDeadline::new(StdDuration::from_secs(2));
    open_windows_daemon_query_pipe(&windows_wide_null(&pipe_name), &deadline)
}

#[cfg(windows)]
fn connect_valid_nonreading_query_client(data_root: &Path) -> Result<WindowsQueryHandle> {
    let endpoint = read_daemon_query_endpoint(data_root)?.expect("query endpoint");
    let DaemonQueryEndpoint::WindowsNamedPipe { pipe_name, token } = endpoint;
    let deadline = WindowsIoDeadline::new(StdDuration::from_secs(2));
    let pipe = open_windows_daemon_query_pipe(&windows_wide_null(&pipe_name), &deadline)?;
    let request = format!(
        "{}\n",
        serde_json::to_string(&compact_json(json!({
            "schema_version": 1,
            "op": "ping",
            "token": token,
        })))?
    );
    write_all_windows_daemon_query_pipe(&pipe, request.as_bytes(), &deadline)?;
    Ok(pipe)
}

#[test]
fn daemon_query_activity_prevents_idle_shutdown_during_a_request() {
    let activity = Arc::new(DaemonQueryActivity::new());
    let request = activity.begin_request().expect("request accepted");
    let (active, generation) = activity.snapshot();

    assert_eq!(active, 1);
    assert!(!activity.try_stop_accepting_if_idle(generation));

    drop(request);
    let (active, completed_generation) = activity.snapshot();
    assert_eq!(active, 0);
    assert_ne!(completed_generation, generation);
    assert!(activity.try_stop_accepting_if_idle(completed_generation));
    assert!(activity.begin_request().is_none());
}

#[cfg(any(unix, windows))]
#[test]
fn stalled_query_client_is_discarded_and_next_query_is_served() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let service = start_test_query_service(temp.path())?;
    let stalled_client = connect_stalled_query_client(temp.path())?;
    wait_for_active_query(&service)?;

    let started = Instant::now();
    let response = daemon_query_request(
        temp.path(),
        compact_json(json!({
            "schema_version": 1,
            "op": "ping",
        })),
        StdDuration::from_secs(2),
        64 * 1024,
    )?
    .expect("query response");

    assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));
    assert!(started.elapsed() < StdDuration::from_secs(1));
    drop(stalled_client);
    drop(service);
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn query_service_ping_stays_healthy_while_embedder_is_busy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runtime = SharedSemanticRuntime::default();
    let service = start_daemon_query_service_with_request_timeout(
        temp.path(),
        runtime.clone(),
        TEST_QUERY_REQUEST_READ_TIMEOUT,
    )?;
    let _runtime_guard = runtime.lock_for_test()?;

    let started = Instant::now();
    let response = daemon_query_request(
        temp.path(),
        compact_json(json!({
            "schema_version": 1,
            "op": "ping",
        })),
        StdDuration::from_secs(1),
        64 * 1024,
    )?
    .expect("query response");

    assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(response.get("busy").and_then(Value::as_bool), Some(true));
    assert!(response["embedding_runtime"].is_null());
    assert!(started.elapsed() < StdDuration::from_millis(500));
    drop(service);
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn query_service_shutdown_is_bounded_with_stalled_client() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let service = start_test_query_service(temp.path())?;
    let stalled_client = connect_stalled_query_client(temp.path())?;
    wait_for_active_query(&service)?;

    let started = Instant::now();
    drop(service);

    assert!(started.elapsed() < StdDuration::from_secs(1));
    drop(stalled_client);
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn valid_nonreading_client_does_not_block_later_queries_or_shutdown() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let service = start_test_query_service(temp.path())?;
    let nonreader = connect_valid_nonreading_query_client(temp.path())?;
    std::thread::sleep(StdDuration::from_millis(25));

    let response = daemon_query_request(
        temp.path(),
        compact_json(json!({
            "schema_version": 1,
            "op": "ping",
        })),
        StdDuration::from_secs(2),
        64 * 1024,
    )?
    .expect("query response");
    assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));

    let started = Instant::now();
    drop(service);
    assert!(started.elapsed() < StdDuration::from_secs(1));
    drop(nonreader);
    Ok(())
}

#[test]
fn observing_query_activity_resets_an_expired_idle_window() {
    let activity = Arc::new(DaemonQueryActivity::new());
    let request = activity.begin_request().expect("request accepted");
    let mut idle_since = Some(Instant::now() - StdDuration::from_secs(5));
    let mut observed_generation = 0;

    observe_daemon_query_activity(
        Some(activity.as_ref()),
        &mut idle_since,
        &mut observed_generation,
    );

    assert!(idle_since.is_none());
    assert!(!daemon_can_begin_idle_shutdown(
        Some(activity.as_ref()),
        observed_generation
    ));
    drop(request);
    observe_daemon_query_activity(
        Some(activity.as_ref()),
        &mut idle_since,
        &mut observed_generation,
    );
    assert!(idle_since.is_none());
}

#[cfg(unix)]
#[test]
fn daemon_query_socket_uses_short_private_fallback_for_long_data_root() -> Result<()> {
    let data_root = PathBuf::from("/tmp").join("x".repeat(160));
    let (listener, path, runtime_dir) = bind_daemon_query_listener(&data_root)?;
    assert!(path.as_os_str().as_bytes().len() <= DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES);
    assert_ne!(path, daemon_query_socket_path(&data_root));
    let runtime_dir = runtime_dir.expect("long path should use a private runtime dir");
    assert_eq!(path.parent(), Some(runtime_dir.as_path()));

    drop(listener);
    fs::remove_file(&path)?;
    fs::remove_dir(&runtime_dir)?;
    Ok(())
}

#[test]
fn daemon_query_request_reader_stops_at_newline() -> Result<()> {
    let mut cursor = std::io::Cursor::new(b"{\"op\":\"ping\"}\nignored".to_vec());

    let body = read_daemon_query_request(&mut cursor, 256)?;

    assert_eq!(body, "{\"op\":\"ping\"}");
    Ok(())
}

#[test]
fn daemon_query_request_reader_rejects_oversized_request() {
    let mut cursor = std::io::Cursor::new(b"abcdef".to_vec());

    let error =
        read_daemon_query_request(&mut cursor, 3).expect_err("oversized request should fail");

    assert!(format!("{error:#}").contains("daemon query request is too large"));
}

#[cfg(unix)]
#[test]
fn daemon_query_endpoint_roundtrips_unix_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let endpoint = DaemonQueryEndpoint::Unix {
        path: daemon_query_socket_path(temp.path()),
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    };

    write_daemon_query_endpoint(temp.path(), &endpoint)?;
    let loaded = read_daemon_query_endpoint(temp.path())?.expect("endpoint");

    match loaded {
        DaemonQueryEndpoint::Unix { path, token } => {
            assert_eq!(path, daemon_query_socket_path(temp.path()));
            assert_eq!(token, "0123456789abcdef0123456789abcdef");
        }
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn daemon_query_endpoint_roundtrips_windows_named_pipe_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let pipe_name = daemon_query_pipe_name();
    assert!(windows_named_pipe_name_is_local(&pipe_name));
    let endpoint = DaemonQueryEndpoint::WindowsNamedPipe {
        pipe_name: pipe_name.clone(),
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    };

    write_daemon_query_endpoint(temp.path(), &endpoint)?;
    let loaded = read_daemon_query_endpoint(temp.path())?.expect("endpoint");

    match loaded {
        DaemonQueryEndpoint::WindowsNamedPipe {
            pipe_name: loaded_pipe_name,
            token,
        } => {
            assert_eq!(loaded_pipe_name, pipe_name);
            assert_eq!(token, "0123456789abcdef0123456789abcdef");
        }
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn daemon_query_endpoint_rejects_nonlocal_windows_pipe_name() -> Result<()> {
    let temp = tempfile::tempdir()?;
    create_private_dir_all(&daemon_root_path(temp.path()))?;
    let endpoint = compact_json(json!({
        "schema_version": 1,
        "transport": "windows_named_pipe",
        "pipe_name": r"\\server\pipe\ctx-daemon-query-0123456789abcdef0123456789abcdef",
        "token": "0123456789abcdef0123456789abcdef",
    }));
    write_private_json_file(&daemon_query_endpoint_path(temp.path()), &endpoint)?;

    assert!(read_daemon_query_endpoint(temp.path())?.is_none());
    Ok(())
}

#[test]
fn daemon_query_endpoint_rejects_short_tokens() -> Result<()> {
    let temp = tempfile::tempdir()?;
    create_private_dir_all(&daemon_root_path(temp.path()))?;
    let mut endpoint = compact_json(json!({
            "schema_version": 1,
            "transport": "unix",
            "token": "short",
    }));
    #[cfg(unix)]
    {
        endpoint["path"] = Value::String(
            daemon_query_socket_path(temp.path())
                .to_string_lossy()
                .into_owned(),
        );
    }
    #[cfg(windows)]
    {
        endpoint["transport"] = Value::String("windows_named_pipe".to_owned());
        endpoint["pipe_name"] = Value::String(daemon_query_pipe_name());
    }
    write_private_json_file(&daemon_query_endpoint_path(temp.path()), &endpoint)?;

    assert!(read_daemon_query_endpoint(temp.path())?.is_none());
    Ok(())
}
