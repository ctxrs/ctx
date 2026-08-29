use super::*;

struct TestHttpEndpoint {
    endpoint: String,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl TestHttpEndpoint {
    fn start(space_id: &str, dimensions: usize) -> Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let contract_body = json!({
            "schema_version": 1,
            "space_id": space_id,
            "dimensions": dimensions,
        })
        .to_string();
        let worker = std::thread::spawn(move || {
            while !worker_stop.load(std::sync::atomic::Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(StdDuration::from_secs(1)));
                        let request = read_test_http_request(&mut stream).unwrap_or_default();
                        worker_requests
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .push(request.clone());
                        let (status, body) = if request.starts_with("GET ") {
                            ("200 OK", contract_body.as_str())
                        } else {
                            ("500 Internal Server Error", "")
                        };
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(StdDuration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            endpoint,
            requests,
            stop,
            worker: Some(worker),
        })
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl Drop for TestHttpEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_test_http_request(stream: &mut std::net::TcpStream) -> std::io::Result<String> {
    let mut request = Vec::new();
    let mut content_length = None;
    loop {
        let mut buffer = [0_u8; 4096];
        let read = std::io::Read::read(stream, &mut buffer)?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if content_length.is_none() {
            if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                content_length = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                });
            }
        }
        if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            if request.len() >= header_end + 4 + content_length.unwrap_or(0) {
                break;
            }
        }
    }
    Ok(String::from_utf8_lossy(&request).into_owned())
}

#[test]
fn same_space_endpoint_move_rejects_stale_daemon_before_endpoint_access() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let endpoint_a = TestHttpEndpoint::start("ipc-space", 192)?;
    let endpoint_b_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let endpoint_b = format!("http://{}", endpoint_b_listener.local_addr()?);
    let space = ExternalSemanticSpace::new("ipc-space", 192)?;
    let active = SemanticEmbeddingExecutorConfig::http(endpoint_a.endpoint(), space.clone())?;
    let replacement = SemanticEmbeddingExecutorConfig::http(&endpoint_b, space)?;
    assert_eq!(
        active.contract().fingerprint(),
        replacement.contract().fingerprint()
    );
    assert_ne!(
        active.contract().executor_route_identity(),
        replacement.contract().executor_route_identity()
    );
    let service = start_test_query_service_with_executor(temp.path(), active.clone())?;

    let ping = daemon_query_request(
        temp.path(),
        compact_json(json!({
            "schema_version": DAEMON_SEMANTIC_QUERY_SCHEMA_VERSION,
            "op": "ping",
            "executor_route_identity": replacement.contract().executor_route_identity(),
        })),
        StdDuration::from_secs(1),
        64 * 1024,
    )?
    .expect("query ping response");
    assert_eq!(ping["ok"], false);
    assert_eq!(ping["model_key"], active.contract().model_key());
    assert_eq!(
        ping["model_contract_fingerprint"],
        active.contract().fingerprint()
    );
    assert_eq!(
        ping["executor_route_identity"],
        active.contract().executor_route_identity()
    );
    assert_eq!(
        ping["error"],
        "daemon query executor route identity mismatch"
    );

    let raw_query = "must never reach endpoint A";
    let stale = daemon_query_request(
        temp.path(),
        compact_json(json!({
            "schema_version": DAEMON_SEMANTIC_QUERY_SCHEMA_VERSION,
            "op": "embed_query",
            "model_key": replacement.contract().model_key(),
            "model_contract_fingerprint": replacement.contract().fingerprint(),
            "executor_route_identity": replacement.contract().executor_route_identity(),
            "text": raw_query,
        })),
        StdDuration::from_secs(1),
        64 * 1024,
    )?
    .expect("stale daemon response");
    assert_eq!(stale["ok"], false);
    assert_eq!(
        stale["error"],
        "daemon query executor route identity mismatch"
    );
    assert_eq!(
        stale["model_contract_fingerprint"],
        active.contract().fingerprint()
    );
    assert_eq!(
        stale["executor_route_identity"],
        active.contract().executor_route_identity()
    );
    assert!(!stale.to_string().contains(raw_query));
    assert_eq!(
        endpoint_a.requests(),
        Vec::<String>::new(),
        "stale endpoint A must receive neither contract nor embedding requests"
    );
    drop(service);
    Ok(())
}
