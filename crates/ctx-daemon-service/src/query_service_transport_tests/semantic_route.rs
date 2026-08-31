use super::*;

#[derive(Clone)]
enum TestHttpProtocol {
    V2 { space_id: String, dimensions: usize },
    LegacyFixedV1,
}

struct TestHttpEndpoint {
    endpoint: String,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl TestHttpEndpoint {
    fn start(space_id: &str, dimensions: usize) -> Result<Self> {
        Self::start_with_protocol(
            TestHttpProtocol::V2 {
                space_id: space_id.to_owned(),
                dimensions,
            },
            None,
        )
    }

    fn start_authenticated(space_id: &str, dimensions: usize, token: &str) -> Result<Self> {
        Self::start_with_protocol(
            TestHttpProtocol::V2 {
                space_id: space_id.to_owned(),
                dimensions,
            },
            Some(token.to_owned()),
        )
    }

    fn start_legacy_authenticated(token: &str) -> Result<Self> {
        Self::start_with_protocol(TestHttpProtocol::LegacyFixedV1, Some(token.to_owned()))
    }

    fn start_with_protocol(
        protocol: TestHttpProtocol,
        expected_token: Option<String>,
    ) -> Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            while !worker_stop.load(std::sync::atomic::Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // macOS may inherit the listener's nonblocking mode on
                        // accepted streams. The fake server reads synchronously.
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(StdDuration::from_secs(1)));
                        let request = read_test_http_request(&mut stream).unwrap_or_default();
                        worker_requests
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .push(request.clone());
                        let authorized = expected_token.as_ref().is_none_or(|token| {
                            request.lines().any(|line| {
                                line.split_once(':').is_some_and(|(name, value)| {
                                    name.eq_ignore_ascii_case("authorization")
                                        && value.trim() == format!("Bearer {token}")
                                })
                            })
                        });
                        let (status, body) = if authorized {
                            test_http_response(&protocol, &request)
                        } else {
                            ("401 Unauthorized", String::new())
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

fn test_http_response(protocol: &TestHttpProtocol, request: &str) -> (&'static str, String) {
    let request_line = request.lines().next().unwrap_or_default();
    match (protocol, request_line) {
        (
            TestHttpProtocol::V2 {
                space_id,
                dimensions,
            },
            line,
        ) if line.starts_with("GET /v2/contract ") => (
            "200 OK",
            json!({
                "schema_version": 2,
                "space_id": space_id,
                "dimensions": dimensions,
            })
            .to_string(),
        ),
        (
            TestHttpProtocol::V2 {
                space_id,
                dimensions,
            },
            line,
        ) if line.starts_with("POST /v2/embeddings ") => {
            let body = test_http_request_body(request);
            let mut embedding = vec![0.0_f32; *dimensions];
            embedding[0] = 1.0;
            let embeddings = body["inputs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|input| json!({"id": input["id"], "embedding": embedding}))
                .collect::<Vec<_>>();
            (
                "200 OK",
                json!({
                    "schema_version": 2,
                    "space_id": space_id,
                    "dimensions": dimensions,
                    "request_id": body["request_id"],
                    "embeddings": embeddings,
                })
                .to_string(),
            )
        }
        (TestHttpProtocol::LegacyFixedV1, line) if line.starts_with("GET /v1/contract ") => {
            let Some(model_key) = test_http_header(request, "x-ctx-semantic-model-key") else {
                return ("400 Bad Request", String::new());
            };
            let Some(fingerprint) =
                test_http_header(request, "x-ctx-semantic-model-contract-fingerprint")
            else {
                return ("400 Bad Request", String::new());
            };
            (
                "200 OK",
                json!({
                    "schema_version": 1,
                    "model_key": model_key,
                    "model_contract_fingerprint": fingerprint,
                })
                .to_string(),
            )
        }
        (TestHttpProtocol::LegacyFixedV1, line) if line.starts_with("POST /v1/embeddings ") => {
            let body = test_http_request_body(request);
            let Some(embedding) = body["input_kind"]
                .as_str()
                .and_then(legacy_fixed_http_canary_embedding)
            else {
                return ("400 Bad Request", String::new());
            };
            let embeddings = body["inputs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|input| json!({"id": input["id"], "embedding": embedding}))
                .collect::<Vec<_>>();
            (
                "200 OK",
                json!({
                    "schema_version": 1,
                    "model_key": body["model_key"],
                    "model_contract_fingerprint": body["model_contract_fingerprint"],
                    "request_id": body["request_id"],
                    "embeddings": embeddings,
                })
                .to_string(),
            )
        }
        _ => ("404 Not Found", String::new()),
    }
}

fn test_http_request_body(request: &str) -> serde_json::Value {
    request
        .split_once("\r\n\r\n")
        .and_then(|(_, body)| serde_json::from_str(body).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn test_http_header<'a>(request: &'a str, expected_name: &str) -> Option<&'a str> {
    request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(expected_name)
            .then_some(value.trim())
    })
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

fn daemon_embedding_request(config: &SemanticEmbeddingExecutorConfig, text: &str) -> Value {
    compact_json(json!({
        "schema_version": DAEMON_SEMANTIC_QUERY_SCHEMA_VERSION,
        "op": "embed_query",
        "model_key": config.contract().model_key(),
        "model_contract_fingerprint": config.contract().fingerprint(),
        "executor_route_identity": config.contract().executor_route_identity(),
        "text": text,
    }))
}

#[test]
fn v2_daemon_ipc_routes_authenticated_query_to_selected_http_executor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let endpoint = TestHttpEndpoint::start_authenticated("ipc-positive-v2", 3, "v2-secret")?;
    let config = SemanticEmbeddingExecutorConfig::http(
        endpoint.endpoint(),
        ExternalSemanticSpace::new("ipc-positive-v2", 3)?,
    )?;
    let auth = SemanticEmbeddingExecutorAuth::bearer(
        "v2-secret".to_owned(),
        endpoint.endpoint().to_owned(),
    );
    let service =
        start_test_query_service_with_executor_and_auth(temp.path(), config.clone(), auth)?;

    let response = daemon_query_request(
        temp.path(),
        daemon_embedding_request(&config, "daemon IPC V2 query"),
        StdDuration::from_secs(2),
        64 * 1024,
    )?
    .expect("daemon query response");

    let requests = endpoint.requests();
    assert_eq!(response["ok"], true, "{response:#}\nrequests={requests:#?}");
    assert_eq!(response["embedding"], json!([1.0, 0.0, 0.0]));
    assert_eq!(requests.len(), 2, "{requests:#?}");
    assert!(requests[0].starts_with("GET /v2/contract "));
    assert!(requests[1].starts_with("POST /v2/embeddings "));
    assert!(requests.iter().all(|request| request
        .to_ascii_lowercase()
        .contains("authorization: bearer v2-secret")));
    assert_eq!(
        test_http_request_body(&requests[1])["inputs"][0]["text"],
        "daemon IPC V2 query"
    );
    drop(service);
    Ok(())
}

#[test]
fn legacy_v1_daemon_ipc_preserves_fixed_e5_wire_auth_and_route_fence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let endpoint = TestHttpEndpoint::start_legacy_authenticated("v1-secret")?;
    let config = SemanticEmbeddingExecutorConfig::legacy_fixed_http(endpoint.endpoint())?;
    let auth = SemanticEmbeddingExecutorAuth::bearer(
        "v1-secret".to_owned(),
        endpoint.endpoint().to_owned(),
    );
    let service =
        start_test_query_service_with_executor_and_auth(temp.path(), config.clone(), auth)?;

    let response = daemon_query_request(
        temp.path(),
        daemon_embedding_request(&config, "daemon IPC V1 query"),
        StdDuration::from_secs(2),
        256 * 1024,
    )?
    .expect("daemon query response");

    assert_eq!(response["ok"], true, "{response:#}");
    assert_eq!(response["model_key"], config.contract().model_key());
    assert_eq!(
        response["executor_route_identity"],
        config.contract().executor_route_identity()
    );
    assert_eq!(
        response["embedding"].as_array().map(Vec::len),
        Some(config.contract().dimensions())
    );
    let requests = endpoint.requests();
    assert_eq!(requests.len(), 4, "{requests:#?}");
    assert!(requests[0].starts_with("GET /v1/contract "));
    assert!(requests[1..]
        .iter()
        .all(|request| request.starts_with("POST /v1/embeddings ")));
    assert!(requests.iter().all(|request| request
        .to_ascii_lowercase()
        .contains("authorization: bearer v1-secret")));
    assert_eq!(
        test_http_request_body(&requests[3])["inputs"][0]["text"],
        "query: daemon IPC V1 query"
    );
    drop(service);
    Ok(())
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
