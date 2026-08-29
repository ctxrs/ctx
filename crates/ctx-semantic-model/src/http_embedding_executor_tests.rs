use std::{
    env,
    ffi::OsString,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{mpsc, Arc, Barrier, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{json, Value};

use super::*;
use crate::{
    http_embedding_canary::{
        DOCUMENT_DAEMON_RECOVERY_REFERENCE, DOCUMENT_PROBES, QUERY_DAEMON_RECOVERY_REFERENCE,
        QUERY_PROBES,
    },
    SemanticEmbeddingExecutorConfig, SemanticEmbeddingExecutorHandle,
    SemanticEmbeddingExecutorKind, SemanticModelPaths, SemanticOnnxRuntimePaths,
    SharedSemanticRuntime,
};

#[cfg(ctx_semantic_fastembed)]
use crate::{
    http_embedding_canary::validate_conformance_canary, BuiltinSemanticEmbeddingExecutor,
    SemanticModelConfig,
};

static AUTH_ENV_LOCK: Mutex<()> = Mutex::new(());

// Test-only final-host adapter. Production callers resolve process authority
// outside ctx-semantic-model and pass this redacted value explicitly.
struct HttpSemanticEmbeddingExecutor;

impl HttpSemanticEmbeddingExecutor {
    fn build(endpoint: impl AsRef<str>) -> Result<super::HttpSemanticEmbeddingExecutor> {
        let auth = match env::var(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV) {
            Ok(token) => {
                let binding = env::var(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).map_err(
                    |error| match error {
                        env::VarError::NotPresent => anyhow!(
                            "semantic embedding authentication endpoint binding is required"
                        ),
                        env::VarError::NotUnicode(_) => anyhow!(
                            "semantic embedding authentication endpoint binding must be valid Unicode"
                        ),
                    },
                )?;
                SemanticEmbeddingExecutorAuth::bearer(token, binding)
            }
            Err(env::VarError::NotPresent) => SemanticEmbeddingExecutorAuth::none(),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(anyhow!(
                    "semantic embedding authentication token must be valid Unicode"
                ));
            }
        };
        if env::var_os(HTTPS_CHILD_ENDPOINT_ENV).is_some() {
            let certificate =
                ureq_semantic::tls::Certificate::from_pem(TEST_CA_CERTIFICATE_PEM.as_bytes())?;
            return super::HttpSemanticEmbeddingExecutor::new_with_auth_and_root_certs(
                endpoint,
                auth,
                ureq_semantic::tls::RootCerts::new_with_certs(&[certificate]),
            );
        }
        super::HttpSemanticEmbeddingExecutor::new_with_auth(endpoint, auth)
    }
}

const HTTPS_CHILD_ENDPOINT_ENV: &str = "CTX_SEMANTIC_TEST_HTTPS_ENDPOINT";
const HTTPS_TEST_NAME: &str =
    "http_embedding_executor::tests::https_protocol_uses_injected_test_trust_without_redirects_or_ambient_proxy";
const TEST_CA_CERTIFICATE_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIBsDCCAVWgAwIBAgIUHywT58QgxCtA49UkihSkVicDmhIwCgYIKoZIzj0EAwIw
JTEjMCEGA1UEAwwaY3R4IHNlbWFudGljIEhUVFBTIHRlc3QgQ0EwHhcNMjYwODI4
MjEzNzE4WhcNNDYwODIzMjEzNzE4WjAlMSMwIQYDVQQDDBpjdHggc2VtYW50aWMg
SFRUUFMgdGVzdCBDQTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABL26teDul7ES
mX3Ux+xpcG3fWsxQiloSZeeVWL3Nh3fWOBNBQA9/KlW3Ve1Fcr5/nljJV3YER/3u
dIbC4Ef6PtejYzBhMB0GA1UdDgQWBBRWt0JWVXR/VZfL15v6do5y7MPTVTAfBgNV
HSMEGDAWgBRWt0JWVXR/VZfL15v6do5y7MPTVTAPBgNVHRMBAf8EBTADAQH/MA4G
A1UdDwEB/wQEAwIBBjAKBggqhkjOPQQDAgNJADBGAiEA3rSzYl+SrsuNPMfWULRW
v5sw+0YEuV7QjyumaRcIGIkCIQDWbptvRKqrLY2+VVfBx5nZe3T7vRcTkJ/KWHV/
a+1pGw==
-----END CERTIFICATE-----
"#;
const TEST_SERVER_CERTIFICATE_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIBwjCCAWmgAwIBAgIUfsPBoPSG75cCErJUIJUMLYqL6V8wCgYIKoZIzj0EAwIw
JTEjMCEGA1UEAwwaY3R4IHNlbWFudGljIEhUVFBTIHRlc3QgQ0EwHhcNMjYwODI4
MjEzNzE4WhcNNDYwODIzMjEzNzE4WjAUMRIwEAYDVQQDDAkxMjcuMC4wLjEwWTAT
BgcqhkjOPQIBBggqhkjOPQMBBwNCAAQaUGxhXreBHm4vqzsfsfUtjaK1YEirQVGO
IjtL7EOY4HOo7S507VrN25Y6N16Orqa/XNvCzwHkyrzXH9ASv2Lyo4GHMIGEMA8G
A1UdEQQIMAaHBH8AAAEwDAYDVR0TAQH/BAIwADAOBgNVHQ8BAf8EBAMCB4AwEwYD
VR0lBAwwCgYIKwYBBQUHAwEwHQYDVR0OBBYEFFbJFkKtJzDu30a5KWBuM+xbUh60
MB8GA1UdIwQYMBaAFFa3QlZVdH9Vl8vXm/p2jnLsw9NVMAoGCCqGSM49BAMCA0cA
MEQCIHYNYTrKwJ+Hoy9bqhwMBcuaKQkE72+2QYgpvJF7g2bPAiAdMaNc9kvHh+N6
3rkKmYsjiYNPws79dYlVq3dS7ZtwfA==
-----END CERTIFICATE-----
"#;
const TEST_SERVER_PRIVATE_KEY_DER_HEX: &str = concat!(
    "308187020100301306072a8648ce3d020106082a8648ce3d030107046d306b",
    "0201010420ce452ee4067b3765371eda1f0514cbecbc4f4dba5a8da641fd89",
    "c127a2b2f431a144034200041a506c615eb7811e6e2fab3b1fb1f52d8da2b5",
    "6048ab41518e223b4bec4398e073a8ed2e74ed5acddb963a375e8eaea6bf5c",
    "dbc2cf01e4cabcd71fd012bf62f2",
);

struct AuthEnvGuard {
    _lock: MutexGuard<'static, ()>,
    original_token: Option<OsString>,
    original_endpoint: Option<OsString>,
}

impl AuthEnvGuard {
    fn unset() -> Self {
        Self::set_os(None, None)
    }

    fn token_without_binding(value: &str) -> Self {
        Self::set_os(Some(OsString::from(value)), None)
    }

    fn bound(value: &str, endpoint: &str) -> Self {
        Self::set_os(Some(OsString::from(value)), Some(OsString::from(endpoint)))
    }

    fn set_os(token: Option<OsString>, endpoint: Option<OsString>) -> Self {
        let lock = AUTH_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original_token = std::env::var_os(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV);
        let original_endpoint = std::env::var_os(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV);
        match token {
            Some(value) => std::env::set_var(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV, value),
            None => std::env::remove_var(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV),
        }
        match endpoint {
            Some(value) => std::env::set_var(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV, value),
            None => std::env::remove_var(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV),
        }
        Self {
            _lock: lock,
            original_token,
            original_endpoint,
        }
    }
}

impl Drop for AuthEnvGuard {
    fn drop(&mut self) {
        match self.original_token.take() {
            Some(value) => std::env::set_var(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV, value),
            None => std::env::remove_var(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV),
        }
        match self.original_endpoint.take() {
            Some(value) => std::env::set_var(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV, value),
            None => std::env::remove_var(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV),
        }
    }
}

#[derive(Clone)]
struct RecordedRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl RecordedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("request body is JSON")
    }

    fn header_count(&self, name: &str) -> usize {
        self.headers
            .iter()
            .filter(|(header, _)| header.eq_ignore_ascii_case(name))
            .count()
    }
}

enum WireResponse {
    Close,
    Http {
        status: u16,
        body: Vec<u8>,
        declared_length: Option<usize>,
    },
}

type Responder = Box<dyn Fn(&RecordedRequest) -> WireResponse + Send>;

struct FakeServer {
    base_url: String,
    requests: mpsc::Receiver<RecordedRequest>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeServer {
    fn start(responders: Vec<Responder>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            for responder in responders {
                let mut stream = accept_with_deadline(&listener);
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let request = read_request(&mut stream);
                request_tx.send(request.clone()).unwrap();
                match responder(&request) {
                    WireResponse::Close => {}
                    WireResponse::Http {
                        status,
                        body,
                        declared_length,
                    } => write_response(&mut stream, status, &body, declared_length),
                }
            }
        });
        Self {
            base_url: format!("http://{address}/semantic-base"),
            requests,
            thread: Some(thread),
        }
    }

    fn start_https(responders: Vec<Responder>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let tls_config = test_tls_server_config();
        let (request_tx, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            for responder in responders {
                let stream = accept_with_deadline(&listener);
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let connection = rustls::ServerConnection::new(Arc::clone(&tls_config)).unwrap();
                let mut stream = rustls::StreamOwned::new(connection, stream);
                let request = read_request(&mut stream);
                request_tx.send(request.clone()).unwrap();
                match responder(&request) {
                    WireResponse::Close => {}
                    WireResponse::Http {
                        status,
                        body,
                        declared_length,
                    } => write_response(&mut stream, status, &body, declared_length),
                }
                stream.conn.send_close_notify();
                let _ = stream.flush();
            }
        });
        Self {
            base_url: format!("https://{address}/semantic-base"),
            requests,
            thread: Some(thread),
        }
    }

    fn finish(mut self) -> Vec<RecordedRequest> {
        self.thread.take().unwrap().join().unwrap();
        self.requests.try_iter().collect()
    }
}

fn accept_with_deadline(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // Accepted sockets can inherit the listener's nonblocking mode
                // on some platforms. The fake server performs blocking HTTP
                // and TLS reads after this bounded accept loop.
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for HTTP request"
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept fake HTTP request: {error}"),
        }
    }
}

fn read_request(stream: &mut impl Read) -> RecordedRequest {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "HTTP request ended before its headers");
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() <= MAX_REQUEST_BODY_BYTES + 64 * 1024);
    };
    let header_text = std::str::from_utf8(&bytes[..header_end]).unwrap();
    let mut lines = header_text.trim_end().split("\r\n");
    let mut request_line = lines.next().unwrap().split_whitespace();
    let method = request_line.next().unwrap().to_owned();
    let path = request_line.next().unwrap().to_owned();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect::<Vec<_>>();
    let content_length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .map(|(_, value)| value.parse::<usize>().unwrap())
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "HTTP request body was truncated");
        bytes.extend_from_slice(&chunk[..count]);
    }
    RecordedRequest {
        method,
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    }
}

fn write_response(
    stream: &mut impl Write,
    status: u16,
    body: &[u8],
    declared_length: Option<usize>,
) {
    let reason = match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Response",
    };
    let redirect = if status == 302 {
        "location: http://127.0.0.1:1/must-not-follow\r\n"
    } else {
        ""
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n{redirect}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        declared_length.unwrap_or(body.len())
    );
    if stream.write_all(header.as_bytes()).is_ok() {
        let _ = stream.write_all(body);
        let _ = stream.flush();
    }
}

fn test_tls_server_config() -> Arc<rustls::ServerConfig> {
    let certificate = CertificateDer::from_pem_slice(TEST_SERVER_CERTIFICATE_PEM.as_bytes())
        .expect("parse static HTTPS test certificate");
    let private_key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(decode_hex(
        TEST_SERVER_PRIVATE_KEY_DER_HEX,
    )));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .expect("configure HTTPS test protocol versions")
            .with_no_client_auth()
            .with_single_cert(vec![certificate], private_key)
            .expect("configure static HTTPS test identity"),
    )
}

fn decode_hex(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0);
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digits = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(digits, 16).unwrap()
        })
        .collect()
}

fn http(status: u16, body: Vec<u8>) -> WireResponse {
    WireResponse::Http {
        status,
        body,
        declared_length: None,
    }
}

fn json_http(value: Value) -> WireResponse {
    http(200, serde_json::to_vec(&value).unwrap())
}

fn contract_value() -> Value {
    json!({
        "schema_version": PROTOCOL_SCHEMA_VERSION,
        "model_key": semantic_model_contract().model_key(),
        "model_contract_fingerprint": semantic_model_contract().fingerprint(),
    })
}

fn contract_reply() -> Responder {
    Box::new(|_: &RecordedRequest| json_http(contract_value()))
}

fn unit_embedding(dimension: usize) -> Vec<f32> {
    let mut embedding = vec![0.0; semantic_model_contract().dimensions()];
    embedding[dimension] = 1.0;
    embedding
}

fn embedding_outputs(request: &RecordedRequest, embeddings: Vec<Vec<f32>>) -> Vec<Value> {
    let request = request.json();
    request["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .zip(embeddings)
        .map(|(input, embedding)| {
            json!({
                "id": input["id"],
                "embedding": embedding,
            })
        })
        .collect()
}

fn embedding_value(request: &RecordedRequest, embeddings: Vec<Vec<f32>>) -> Value {
    embedding_value_with_outputs(request, embedding_outputs(request, embeddings))
}

fn embedding_value_with_outputs(request: &RecordedRequest, embeddings: Vec<Value>) -> Value {
    json!({
        "schema_version": PROTOCOL_SCHEMA_VERSION,
        "model_key": semantic_model_contract().model_key(),
        "model_contract_fingerprint": semantic_model_contract().fingerprint(),
        "request_id": request.json()["request_id"],
        "embeddings": embeddings,
    })
}

fn embedding_reply(embeddings: Vec<Vec<f32>>) -> Responder {
    Box::new(move |request| json_http(embedding_value(request, embeddings.clone())))
}

fn normalized_reference(reference: &[i8]) -> Vec<f32> {
    let norm = reference
        .iter()
        .map(|value| f32::from(*value).powi(2))
        .sum::<f32>()
        .sqrt();
    reference
        .iter()
        .map(|value| f32::from(*value) / norm)
        .collect()
}

fn canary_reply() -> Responder {
    Box::new(|request| {
        let request_json = request.json();
        let count = request_json["inputs"].as_array().unwrap().len();
        let embeddings = match request_json["input_kind"].as_str() {
            Some("query") => vec![normalized_reference(QUERY_DAEMON_RECOVERY_REFERENCE); count],
            Some("documents") => {
                vec![normalized_reference(DOCUMENT_DAEMON_RECOVERY_REFERENCE); count]
            }
            input_kind => panic!("unexpected canary input kind: {input_kind:?}"),
        };
        json_http(embedding_value(request, embeddings))
    })
}

fn successful_contract_prefix() -> Vec<Responder> {
    vec![contract_reply(), canary_reply(), canary_reply()]
}

fn with_successful_contract(mut responders: Vec<Responder>) -> Vec<Responder> {
    let mut all = successful_contract_prefix();
    all.append(&mut responders);
    all
}

#[test]
fn https_protocol_uses_injected_test_trust_without_redirects_or_ambient_proxy() {
    if let Ok(endpoint) = env::var(HTTPS_CHILD_ENDPOINT_ENV) {
        let executor = HttpSemanticEmbeddingExecutor::build(&endpoint).unwrap();
        let embedding = executor
            .embed_query(
                executor
                    .contract()
                    .prepare_query("HTTPS contract probe".to_owned()),
            )
            .unwrap();
        assert_eq!(embedding, unit_embedding(10));
        assert!(executor.contract_verified());
        return;
    }

    let _environment = AuthEnvGuard::unset();
    let server = FakeServer::start_https(with_successful_contract(vec![embedding_reply(vec![
        unit_embedding(10),
    ])]));
    let output = std::process::Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            HTTPS_TEST_NAME,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HTTPS_CHILD_ENDPOINT_ENV, &server.base_url)
        .env(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV, "https-test-token")
        .env(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV, &server.base_url)
        .env("HTTPS_PROXY", "http://127.0.0.1:1/must-not-use")
        .env("ALL_PROXY", "http://127.0.0.1:1/must-not-use")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child stdout:\n{}\nchild stderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert!(requests
        .iter()
        .all(|request| { request.header("authorization") == Some("Bearer https-test-token") }));
}

#[test]
fn transport_configuration_is_direct_and_bounded_by_the_operation_budget() {
    let _environment = AuthEnvGuard::unset();
    let executor = HttpSemanticEmbeddingExecutor::build("http://127.0.0.1:9").unwrap();
    let config = executor.agent.config();
    let timeouts = config.timeouts();

    assert!(!config.http_status_as_error());
    assert_eq!(config.max_redirects(), 0);
    assert!(config.proxy().is_none());
    assert!(matches!(
        config.tls_config().root_certs(),
        ureq_semantic::tls::RootCerts::PlatformVerifier
    ));
    assert_eq!(timeouts.global, Some(EXECUTION_BUDGET));
    assert_eq!(timeouts.resolve, Some(DNS_RESOLVE_TIMEOUT));
    assert_eq!(timeouts.connect, Some(CONNECT_TIMEOUT));
    assert!(DNS_RESOLVE_TIMEOUT < EXECUTION_BUDGET);
    assert!(CONNECT_TIMEOUT < EXECUTION_BUDGET);
}

fn model_config() -> crate::SemanticModelConfig {
    crate::SemanticModelConfig::new(SemanticModelPaths::new(
        PathBuf::from("test-model-cache"),
        SemanticOnnxRuntimePaths::new(PathBuf::from("test-runtime-cache")),
    ))
}

fn query_error(responder: Responder, input: &str) -> String {
    query_failure(responder, input).to_string()
}

fn query_failure(responder: Responder, input: &str) -> anyhow::Error {
    query_failure_from_responders(with_successful_contract(vec![responder]), input)
}

fn query_failure_from_responders(responders: Vec<Responder>, input: &str) -> anyhow::Error {
    let _environment = AuthEnvGuard::unset();
    let expected_requests = responders.len();
    let server = FakeServer::start(responders);
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
    let error = executor
        .embed_query(executor.contract().prepare_query(input.to_owned()))
        .unwrap_err();
    assert_eq!(server.finish().len(), expected_requests);
    error
}

fn documents_error(responder: Responder) -> String {
    let _environment = AuthEnvGuard::unset();
    let server = FakeServer::start(with_successful_contract(vec![responder]));
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
    let error = executor
        .embed_documents(
            executor
                .contract()
                .prepare_documents(vec!["first".to_owned(), "second".to_owned()]),
            None,
        )
        .unwrap_err()
        .to_string();
    assert_eq!(server.finish().len(), 4);
    error
}

#[test]
fn config_enforces_exact_url_policy_and_exposes_selection_metadata() {
    let builtin = SemanticEmbeddingExecutorConfig::default();
    assert_eq!(builtin.kind(), SemanticEmbeddingExecutorKind::Builtin);
    assert!(builtin.is_builtin());
    assert_eq!(builtin.endpoint(), None);

    for endpoint in [
        "http://127.0.0.1:8080",
        "http://127.0.0.2/base",
        "http://[::1]:8080",
        "https://embedding.example.test",
        "https://127.0.0.1/prefix/",
    ] {
        let config = SemanticEmbeddingExecutorConfig::http(endpoint).unwrap();
        assert_eq!(config.kind(), SemanticEmbeddingExecutorKind::Http);
        assert!(!config.is_builtin());
        assert!(config.endpoint().unwrap().ends_with('/'));
        assert_eq!(config.endpoint(), config.http_endpoint());
    }
    assert_eq!(
        SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:8080/base%2Fsegment")
            .unwrap()
            .endpoint(),
        Some("http://127.0.0.1:8080/base%2Fsegment/")
    );

    for endpoint in [
        "",
        " http://127.0.0.1",
        "http://localhost:8080",
        "http://192.168.1.5:8080",
        "http://127.1:8080",
        "http://2130706433:8080",
        "http://0x7f000001:8080",
        "http://[::ffff:127.0.0.1]:8080",
        "http://user:secret@127.0.0.1:8080",
        "http://@127.0.0.1:8080",
        "http://127.0.0.1:8080?key=value",
        "http://127.0.0.1:8080#fragment",
        "ftp://127.0.0.1:8080",
    ] {
        assert!(
            SemanticEmbeddingExecutorConfig::http(endpoint).is_err(),
            "endpoint should be rejected: {endpoint}"
        );
    }
}

#[test]
fn remote_auth_is_required_bounded_unicode_and_redacted() {
    {
        let _environment = AuthEnvGuard::unset();
        let error = HttpSemanticEmbeddingExecutor::build("https://embedding.example.test")
            .unwrap_err()
            .to_string();
        assert!(error.contains("authentication token"));
        assert!(HttpSemanticEmbeddingExecutor::build("http://127.0.0.1:9").is_ok());
    }
    for invalid in ["", "has whitespace", "line\nbreak"] {
        let _environment = AuthEnvGuard::token_without_binding(invalid);
        let error = HttpSemanticEmbeddingExecutor::build("http://127.0.0.1:9")
            .unwrap_err()
            .to_string();
        if !invalid.is_empty() {
            assert!(!error.contains(invalid));
        }
    }
    {
        let oversized = "x".repeat(MAX_TOKEN_BYTES + 1);
        let _environment = AuthEnvGuard::token_without_binding(&oversized);
        assert!(HttpSemanticEmbeddingExecutor::build("https://embedding.example.test").is_err());
    }
    {
        let token = "debug-private-token";
        let _environment = AuthEnvGuard::bound(token, "http://127.0.0.1:9");
        let executor = HttpSemanticEmbeddingExecutor::build("http://127.0.0.1:9").unwrap();
        let debug = format!("{executor:?}");
        assert!(executor.authentication_configured());
        assert!(!debug.contains(token));
    }
}

#[test]
fn authentication_is_bound_to_the_exact_normalized_endpoint_before_requests() {
    let token = "binding-private-token";
    let endpoint_a = "https://endpoint-a.example.test/private-path";
    let endpoint_b = "https://endpoint-b.example.test/private-path";

    {
        let _environment = AuthEnvGuard::token_without_binding(token);
        let error = HttpSemanticEmbeddingExecutor::build(endpoint_a)
            .unwrap_err()
            .to_string();
        assert!(error.contains("endpoint binding is required"), "{error}");
        assert!(!error.contains(token));
    }
    {
        let _environment = AuthEnvGuard::bound(token, endpoint_a);
        let error = HttpSemanticEmbeddingExecutor::build(endpoint_b)
            .unwrap_err()
            .to_string();
        assert!(error.contains("binding does not match"), "{error}");
        for private in [token, endpoint_a, endpoint_b] {
            assert!(!error.contains(private));
        }
    }
    {
        let _environment = AuthEnvGuard::bound(token, endpoint_a);
        let executor = HttpSemanticEmbeddingExecutor::build(format!("{endpoint_a}/")).unwrap();
        assert!(executor.authentication_configured());
    }
    assert_eq!(
        SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
        "CTX_SEMANTIC_EMBEDDING_TOKEN_ENDPOINT"
    );
}

#[cfg(unix)]
#[test]
fn non_unicode_auth_is_rejected_without_disclosure() {
    use std::os::unix::ffi::OsStringExt;

    {
        let _environment = AuthEnvGuard::set_os(Some(OsString::from_vec(vec![0xff, 0xfe])), None);
        let error = HttpSemanticEmbeddingExecutor::build("http://127.0.0.1:9")
            .unwrap_err()
            .to_string();
        assert!(error.contains("token must be valid Unicode"), "{error}");
    }
    {
        let token = "binding-unicode-private-token";
        let _environment = AuthEnvGuard::set_os(
            Some(OsString::from(token)),
            Some(OsString::from_vec(vec![0xff, 0xfe])),
        );
        let error = HttpSemanticEmbeddingExecutor::build("http://127.0.0.1:9")
            .unwrap_err()
            .to_string();
        assert!(error.contains("binding must be valid Unicode"), "{error}");
        assert!(!error.contains(token));
    }
}

#[test]
fn handshake_is_cached_and_prepared_inputs_headers_and_token_are_exact() {
    let token = "token-private-value";
    let server = FakeServer::start(with_successful_contract(vec![
        embedding_reply(vec![unit_embedding(0)]),
        embedding_reply(vec![unit_embedding(1), unit_embedding(2)]),
    ]));
    let _environment = AuthEnvGuard::bound(token, &server.base_url);
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
    assert!(!executor.contract_verified());
    assert_eq!(
        executor
            .embed_query(executor.contract().prepare_query("needle 秘密".to_owned()))
            .unwrap(),
        unit_embedding(0)
    );
    assert_eq!(
        executor
            .embed_documents(
                executor
                    .contract()
                    .prepare_documents(vec!["one".to_owned(), "two 世界".to_owned()]),
                Some(Instant::now()),
            )
            .unwrap(),
        vec![unit_embedding(1), unit_embedding(2)]
    );
    assert!(executor.contract_verified());

    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        (&requests[0].method, &requests[0].path),
        (&"GET".to_owned(), &"/semantic-base/v1/contract".to_owned())
    );
    assert_eq!(
        (&requests[3].method, &requests[3].path),
        (
            &"POST".to_owned(),
            &"/semantic-base/v1/embeddings".to_owned()
        )
    );
    for request in &requests {
        assert_eq!(request.header("accept-encoding"), Some("identity"));
        assert_eq!(request.header_count("accept-encoding"), 1);
        assert_eq!(request.header(SCHEMA_HEADER), Some("1"));
        assert_eq!(
            request.header(MODEL_KEY_HEADER),
            Some(semantic_model_contract().model_key())
        );
        assert_eq!(
            request.header(CONTRACT_FINGERPRINT_HEADER),
            Some(semantic_model_contract().fingerprint())
        );
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {token}").as_str())
        );
    }
    let query_canary = requests[1].json();
    let document_canary = requests[2].json();
    assert_eq!(query_canary["input_kind"], "query");
    assert_eq!(
        query_canary["inputs"].as_array().unwrap().len(),
        QUERY_PROBES.len()
    );
    assert_eq!(document_canary["input_kind"], "documents");
    assert_eq!(
        document_canary["inputs"].as_array().unwrap().len(),
        DOCUMENT_PROBES.len()
    );
    let query = requests[3].json();
    assert_eq!(query["schema_version"], 1);
    assert_eq!(query["model_key"], semantic_model_contract().model_key());
    assert_eq!(
        query["model_contract_fingerprint"],
        semantic_model_contract().fingerprint()
    );
    assert_eq!(query["input_kind"], "query");
    assert_eq!(query["inputs"][0]["text"], "query: needle 秘密");
    assert!(query["inputs"][0]["id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    let documents = requests[4].json();
    assert_eq!(documents["input_kind"], "documents");
    assert_eq!(
        documents["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|input| input["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["passage: one", "passage: two 世界"]
    );
    assert_ne!(documents["inputs"][0]["id"], documents["inputs"][1]["id"]);
    assert!(documents.get("pacing_deadline").is_none());
}

#[test]
fn transport_and_retryable_status_retry_once_with_the_same_request_id() {
    for retry in [
        Box::new(|_: &RecordedRequest| WireResponse::Close) as Responder,
        Box::new(|_: &RecordedRequest| http(503, b"private retry body".to_vec())) as Responder,
    ] {
        let _environment = AuthEnvGuard::unset();
        let server = FakeServer::start(with_successful_contract(vec![
            retry,
            embedding_reply(vec![unit_embedding(0)]),
        ]));
        let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
        executor
            .embed_query(executor.contract().prepare_query("retry input".to_owned()))
            .unwrap();
        let requests = server.finish();
        assert_eq!(requests.len(), 5);
        assert_eq!(requests[3].body, requests[4].body);
        assert_eq!(
            requests[3].json()["request_id"],
            requests[4].json()["request_id"]
        );
    }
}

#[test]
fn transient_failure_remains_retryable_on_the_same_executor() {
    let _environment = AuthEnvGuard::unset();
    let server = FakeServer::start(with_successful_contract(vec![
        Box::new(|_: &RecordedRequest| WireResponse::Close),
        Box::new(|_: &RecordedRequest| WireResponse::Close),
        embedding_reply(vec![unit_embedding(12)]),
    ]));
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();

    let first = executor
        .embed_query(
            executor
                .contract()
                .prepare_query("transient first attempt".to_owned()),
        )
        .unwrap_err();
    assert!(!semantic_embedding_failure_is_permanent(&first));
    assert!(executor.contract_verified());
    assert_eq!(
        executor
            .embed_query(
                executor
                    .contract()
                    .prepare_query("transient second attempt".to_owned()),
            )
            .unwrap(),
        unit_embedding(12)
    );
    assert_eq!(server.finish().len(), 6);
}

#[cfg(ctx_semantic_fastembed)]
#[test]
#[ignore = "qualification requires a populated production semantic model cache"]
fn pinned_builtin_executor_qualifies_the_committed_frozen_pair() {
    let data_root = env::var_os("CTX_SEMANTIC_GOLDEN_DATA_ROOT")
        .map(PathBuf::from)
        .expect("set CTX_SEMANTIC_GOLDEN_DATA_ROOT to a populated ctx data root");
    let config = SemanticModelConfig::new(SemanticModelPaths::new(
        data_root.join("semantic-model-cache"),
        SemanticOnnxRuntimePaths::new(data_root.join("runtime")),
    ));
    let runtime = SharedSemanticRuntime::default();
    runtime.ensure_loaded_from_cache(&config).unwrap();
    let executor = BuiltinSemanticEmbeddingExecutor::new(runtime, config);
    let query = executor
        .embed_query(
            executor
                .contract()
                .prepare_query(QUERY_PROBES[0].text.to_owned()),
        )
        .unwrap();
    let documents = executor
        .embed_documents(
            executor
                .contract()
                .prepare_documents(vec![DOCUMENT_PROBES[0].text.to_owned()]),
            None,
        )
        .unwrap();

    validate_conformance_canary(&[query], &documents).unwrap();
}

#[test]
fn retry_is_bounded_and_never_falls_back() {
    let _environment = AuthEnvGuard::unset();
    let server = FakeServer::start(with_successful_contract(vec![
        Box::new(|_: &RecordedRequest| http(503, b"first private body".to_vec())),
        Box::new(|_: &RecordedRequest| http(503, b"second private body".to_vec())),
    ]));
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
    let error = executor
        .embed_query(
            executor
                .contract()
                .prepare_query("never fallback".to_owned()),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("HTTP status 503"));
    assert!(!error.contains("private body"));
    assert_eq!(server.finish().len(), 5);
}

#[test]
fn wrong_handshake_contract_fails_before_embedding() {
    let _environment = AuthEnvGuard::unset();
    let reply = Box::new(|_: &RecordedRequest| {
        let mut value = contract_value();
        value["model_contract_fingerprint"] = json!("wrong-contract");
        json_http(value)
    });
    let server = FakeServer::start(vec![reply]);
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
    let first = executor
        .embed_query(executor.contract().prepare_query("input".to_owned()))
        .unwrap_err();
    let second = executor
        .embed_query(executor.contract().prepare_query("retry input".to_owned()))
        .unwrap_err();
    assert!(semantic_embedding_failure_is_permanent(&first));
    assert!(semantic_embedding_failure_is_permanent(&second));
    assert_eq!(first.to_string(), second.to_string());
    assert!(first.to_string().contains("different model contract"));
    assert!(!executor.contract_verified());
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn permanent_protocol_failure_is_cached_until_executor_reconstruction() {
    let _environment = AuthEnvGuard::unset();
    let mut responders = with_successful_contract(vec![Box::new(|_: &RecordedRequest| {
        http(200, b"private malformed response".to_vec())
    })]);
    responders.extend(with_successful_contract(vec![embedding_reply(vec![
        unit_embedding(13),
    ])]));
    let server = FakeServer::start(responders);
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();

    let first = executor
        .embed_query(
            executor
                .contract()
                .prepare_query("private first input".to_owned()),
        )
        .unwrap_err();
    let second = executor
        .embed_query(
            executor
                .contract()
                .prepare_query("private retry input".to_owned()),
        )
        .unwrap_err();
    assert!(semantic_embedding_failure_is_permanent(&first));
    assert!(semantic_embedding_failure_is_permanent(&second));
    assert_eq!(first.to_string(), second.to_string());
    assert!(!first.to_string().contains("private"));
    let empty_documents = executor
        .embed_documents(executor.contract().prepare_documents(Vec::new()), None)
        .unwrap_err();
    assert!(semantic_embedding_failure_is_permanent(&empty_documents));
    assert_eq!(first.to_string(), empty_documents.to_string());
    assert!(!executor.contract_verified());
    drop(executor);

    let reconstructed = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
    assert_eq!(
        reconstructed
            .embed_query(
                reconstructed
                    .contract()
                    .prepare_query("reconstructed input".to_owned()),
            )
            .unwrap(),
        unit_embedding(13)
    );
    assert!(reconstructed.contract_verified());
    assert_eq!(server.finish().len(), 8);
}

#[test]
fn empty_document_batch_succeeds_without_handshake_or_request() {
    let _environment = AuthEnvGuard::unset();
    let server = FakeServer::start(Vec::new());
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();

    assert_eq!(
        executor
            .embed_documents(executor.contract().prepare_documents(Vec::new()), None)
            .unwrap(),
        Vec::<Vec<f32>>::new()
    );
    assert!(!executor.contract_verified());
    assert!(server.finish().is_empty());
}

#[test]
fn protocol_failures_are_permanent_while_retry_exhaustion_remains_retryable() {
    let wrong_contract = query_failure_from_responders(
        vec![Box::new(|_: &RecordedRequest| {
            let mut value = contract_value();
            value["model_key"] = json!("wrong-model");
            json_http(value)
        })],
        "input",
    );
    assert!(semantic_embedding_failure_is_permanent(&wrong_contract));

    let malformed = query_failure(
        Box::new(|_: &RecordedRequest| http(200, b"not-json".to_vec())),
        "input",
    );
    assert!(semantic_embedding_failure_is_permanent(&malformed));

    let nonretryable_http = query_failure(
        Box::new(|_: &RecordedRequest| http(400, Vec::new())),
        "input",
    );
    assert!(semantic_embedding_failure_is_permanent(&nonretryable_http));

    let count = QUERY_PROBES.len();
    let conformance = query_failure_from_responders(
        vec![
            contract_reply(),
            embedding_reply(vec![unit_embedding(0); count]),
            embedding_reply(vec![unit_embedding(0); count]),
        ],
        "input",
    );
    assert!(semantic_embedding_failure_is_permanent(&conformance));

    let retryable_http = query_failure_from_responders(
        vec![
            Box::new(|_: &RecordedRequest| http(503, Vec::new())),
            Box::new(|_: &RecordedRequest| http(503, Vec::new())),
        ],
        "input",
    );
    assert!(!semantic_embedding_failure_is_permanent(&retryable_http));

    let transport = query_failure_from_responders(
        vec![
            Box::new(|_: &RecordedRequest| WireResponse::Close),
            Box::new(|_: &RecordedRequest| WireResponse::Close),
        ],
        "input",
    );
    assert!(!semantic_embedding_failure_is_permanent(&transport));
}

#[test]
fn response_identity_request_id_cardinality_and_dimensions_are_exact() {
    let wrong_contract = query_error(
        Box::new(|request| {
            let mut value = embedding_value(request, vec![unit_embedding(0)]);
            value["model_key"] = json!("wrong-model");
            json_http(value)
        }),
        "input",
    );
    assert!(wrong_contract.contains("different model contract"));

    let wrong_id = query_error(
        Box::new(|request| {
            let mut value = embedding_value(request, vec![unit_embedding(0)]);
            value["request_id"] = json!("wrong-request-id");
            json_http(value)
        }),
        "input",
    );
    assert!(wrong_id.contains("request ID"));

    let cardinality = query_error(embedding_reply(Vec::new()), "input");
    assert!(cardinality.contains("missing an input ID"));
    let dimensions = query_error(embedding_reply(vec![vec![1.0; 383]]), "input");
    assert!(dimensions.contains("dimensions"));
}

#[test]
fn shuffled_embedding_outputs_are_restored_to_request_order() {
    let _environment = AuthEnvGuard::unset();
    let server = FakeServer::start(with_successful_contract(vec![Box::new(|request| {
        let mut outputs = embedding_outputs(
            request,
            vec![unit_embedding(7), unit_embedding(8), unit_embedding(9)],
        );
        outputs.reverse();
        json_http(embedding_value_with_outputs(request, outputs))
    })]));
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
    let embeddings = executor
        .embed_documents(
            executor.contract().prepare_documents(vec![
                "first".to_owned(),
                "second".to_owned(),
                "third".to_owned(),
            ]),
            None,
        )
        .unwrap();
    assert_eq!(
        embeddings,
        vec![unit_embedding(7), unit_embedding(8), unit_embedding(9)]
    );
    assert_eq!(server.finish().len(), 4);
}

#[test]
fn duplicate_missing_and_unknown_embedding_ids_fail_closed() {
    let duplicate = documents_error(Box::new(|request| {
        let mut outputs = embedding_outputs(request, vec![unit_embedding(0), unit_embedding(1)]);
        outputs[1]["id"] = outputs[0]["id"].clone();
        json_http(embedding_value_with_outputs(request, outputs))
    }));
    assert!(duplicate.contains("duplicate input ID"), "{duplicate}");

    let missing = documents_error(Box::new(|request| {
        let mut outputs = embedding_outputs(request, vec![unit_embedding(0), unit_embedding(1)]);
        outputs.pop();
        json_http(embedding_value_with_outputs(request, outputs))
    }));
    assert!(missing.contains("missing an input ID"), "{missing}");

    let unknown = documents_error(Box::new(|request| {
        let mut outputs = embedding_outputs(request, vec![unit_embedding(0), unit_embedding(1)]);
        outputs[0]["id"] = json!("unknown-opaque-input-id");
        json_http(embedding_value_with_outputs(request, outputs))
    }));
    assert!(unknown.contains("unknown input ID"), "{unknown}");
}

#[test]
fn constant_and_wrong_role_canary_vectors_are_rejected_before_user_input() {
    let count = QUERY_PROBES.len();
    let constant = vec![unit_embedding(0); count];
    let frozen_query = vec![normalized_reference(QUERY_DAEMON_RECOVERY_REFERENCE); count];
    let frozen_document = vec![normalized_reference(DOCUMENT_DAEMON_RECOVERY_REFERENCE); count];

    for (queries, documents) in [
        (constant.clone(), constant),
        (frozen_query.clone(), frozen_query),
        (frozen_document.clone(), frozen_document),
    ] {
        let _environment = AuthEnvGuard::unset();
        let server = FakeServer::start(vec![
            contract_reply(),
            embedding_reply(queries),
            embedding_reply(documents),
        ]);
        let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
        let error = executor
            .embed_query(
                executor
                    .contract()
                    .prepare_query("private user input".to_owned()),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("conformance canary"), "{error}");
        assert!(!error.contains("private user input"));
        assert!(!executor.contract_verified());
        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|request| {
            !String::from_utf8_lossy(&request.body).contains("private user input")
        }));
    }
}

#[test]
fn concurrent_first_use_shares_one_handshake_and_canary() {
    let _environment = AuthEnvGuard::unset();
    let delayed_contract = Box::new(|_: &RecordedRequest| {
        thread::sleep(Duration::from_millis(50));
        json_http(contract_value())
    });
    let server = FakeServer::start(vec![
        delayed_contract,
        canary_reply(),
        canary_reply(),
        embedding_reply(vec![unit_embedding(10)]),
        embedding_reply(vec![unit_embedding(10)]),
    ]);
    let executor = Arc::new(HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap());
    let barrier = Arc::new(Barrier::new(3));
    let workers = (0..2)
        .map(|index| {
            let executor = Arc::clone(&executor);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                executor.embed_query(
                    executor
                        .contract()
                        .prepare_query(format!("concurrent query {index}")),
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for worker in workers {
        assert_eq!(worker.join().unwrap().unwrap(), unit_embedding(10));
    }
    assert!(executor.contract_verified());
    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.path.ends_with(CONTRACT_ROUTE))
            .count(),
        1
    );
}

#[test]
fn vectors_must_be_finite_nonzero_and_normalized_with_existing_tolerance() {
    let zero = query_error(
        embedding_reply(vec![vec![0.0; semantic_model_contract().dimensions()]]),
        "input",
    );
    assert!(zero.contains("zero-norm"));

    let mut unnormalized = vec![0.0; semantic_model_contract().dimensions()];
    unnormalized[0] = 1.01;
    assert!(query_error(embedding_reply(vec![unnormalized]), "input").contains("L2-normalized"));

    for literal in ["NaN", "Infinity", "1e9999"] {
        let error = query_error(
            Box::new(move |request| {
                let request_json = request.json();
                let request_id = request_json["request_id"].as_str().unwrap();
                let body = format!(
                    "{{\"schema_version\":1,\"model_key\":\"{}\",\"model_contract_fingerprint\":\"{}\",\"request_id\":\"{request_id}\",\"embeddings\":[[{literal}]]}}",
                    semantic_model_contract().model_key(),
                    semantic_model_contract().fingerprint(),
                );
                http(200, body.into_bytes())
            }),
            "input",
        );
        assert!(error.contains("malformed"));
    }

    let _environment = AuthEnvGuard::unset();
    let mut tolerated = vec![0.0; semantic_model_contract().dimensions()];
    tolerated[0] = 1.0004;
    let server = FakeServer::start(with_successful_contract(vec![embedding_reply(vec![
        tolerated,
    ])]));
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
    executor
        .embed_query(executor.contract().prepare_query("input".to_owned()))
        .unwrap();
    assert_eq!(server.finish().len(), 4);
}

#[test]
fn oversized_truncated_malformed_and_error_responses_are_redacted() {
    {
        let _environment = AuthEnvGuard::unset();
        let server = FakeServer::start(vec![Box::new(|_: &RecordedRequest| {
            http(302, b"redirect-private-body".to_vec())
        })]);
        let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
        let error = executor
            .embed_query(executor.contract().prepare_query("input".to_owned()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("HTTP status 302"));
        assert!(!error.contains("redirect-private-body"));
        assert_eq!(server.finish().len(), 1);
    }
    {
        let _environment = AuthEnvGuard::unset();
        let server = FakeServer::start(vec![Box::new(|_: &RecordedRequest| WireResponse::Http {
            status: 200,
            body: Vec::new(),
            declared_length: Some(MAX_RESPONSE_BODY_BYTES + 1),
        })]);
        let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
        let error = executor
            .embed_query(executor.contract().prepare_query("input".to_owned()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("body size limit"));
        assert_eq!(server.finish().len(), 1);
    }
    {
        let _environment = AuthEnvGuard::unset();
        let body = serde_json::to_vec(&contract_value()).unwrap();
        let declared = body.len() + 10;
        let responders = (0..2)
            .map(|_| {
                let body = body.clone();
                Box::new(move |_: &RecordedRequest| WireResponse::Http {
                    status: 200,
                    body: body.clone(),
                    declared_length: Some(declared),
                }) as Responder
            })
            .collect();
        let server = FakeServer::start(responders);
        let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
        let error = executor
            .embed_query(executor.contract().prepare_query("input".to_owned()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("transport failed"));
        assert_eq!(server.finish().len(), 2);
    }
    {
        let token = "header-private-token";
        let input = "input-private-text";
        let body = "response-private-body";
        let server = FakeServer::start(with_successful_contract(vec![Box::new(
            move |_: &RecordedRequest| http(200, body.as_bytes().to_vec()),
        )]));
        let _environment = AuthEnvGuard::bound(token, &server.base_url);
        let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
        let error = executor
            .embed_query(executor.contract().prepare_query(input.to_owned()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("malformed"));
        for private in [token, input, body] {
            assert!(!error.contains(private));
        }
        assert_eq!(server.finish().len(), 4);
    }
    {
        let body = "private error response body";
        let error = query_error(
            Box::new(move |_: &RecordedRequest| http(400, body.as_bytes().to_vec())),
            "private request input",
        );
        assert!(error.contains("HTTP status 400"));
        assert!(!error.contains(body));
        assert!(!error.contains("private request input"));
    }
}

#[test]
fn oversized_request_fails_locally_before_any_handshake() {
    let _environment = AuthEnvGuard::unset();
    let executor = HttpSemanticEmbeddingExecutor::build("http://127.0.0.1:9").unwrap();
    let oversized = "private-large-input".repeat(MAX_REQUEST_BODY_BYTES / 10);
    let error = executor
        .embed_query(executor.contract().prepare_query(oversized))
        .unwrap_err()
        .to_string();
    assert!(error.contains("body size limit"), "{error}");
}

#[test]
fn document_pages_are_split_into_bounded_http_batches_without_partial_return() {
    let _token = AuthEnvGuard::unset();
    let dynamic_reply = || -> Responder {
        Box::new(|request: &RecordedRequest| {
            let count = request.json()["inputs"].as_array().unwrap().len();
            json_http(embedding_value(
                request,
                (0..count).map(|_| unit_embedding(0)).collect(),
            ))
        })
    };
    let server = FakeServer::start(with_successful_contract(vec![
        dynamic_reply(),
        dynamic_reply(),
    ]));
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
    let documents = (0..(MAX_INPUT_COUNT + 1))
        .map(|index| format!("document {index}"))
        .collect::<Vec<_>>();

    let embeddings = executor
        .embed_documents(executor.contract().prepare_documents(documents), None)
        .unwrap();
    assert_eq!(embeddings.len(), MAX_INPUT_COUNT + 1);

    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests[3].json()["inputs"].as_array().unwrap().len(),
        MAX_INPUT_COUNT
    );
    assert_eq!(requests[4].json()["inputs"].as_array().unwrap().len(), 1);
}

#[test]
fn second_document_batch_failure_is_atomic_and_never_falls_back() {
    let _environment = AuthEnvGuard::unset();
    let first_batch = Box::new(|request: &RecordedRequest| {
        let count = request.json()["inputs"].as_array().unwrap().len();
        json_http(embedding_value(
            request,
            (0..count).map(|_| unit_embedding(11)).collect(),
        ))
    });
    let server = FakeServer::start(with_successful_contract(vec![
        first_batch,
        Box::new(|_: &RecordedRequest| http(400, b"second-batch-private-body".to_vec())),
    ]));
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();
    let documents = (0..(MAX_INPUT_COUNT + 1))
        .map(|index| format!("atomic document {index}"))
        .collect::<Vec<_>>();

    let error = executor
        .embed_documents(executor.contract().prepare_documents(documents), None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("HTTP status 400"), "{error}");
    assert!(!error.contains("second-batch-private-body"));
    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests[3].json()["inputs"].as_array().unwrap().len(),
        MAX_INPUT_COUNT
    );
    assert_eq!(requests[4].json()["inputs"].as_array().unwrap().len(), 1);
}

#[path = "http_embedding_executor_tests/selection_and_budget_tests.rs"]
mod selection_and_budget_tests;
