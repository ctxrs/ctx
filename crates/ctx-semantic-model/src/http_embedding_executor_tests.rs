use std::{
    env,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{mpsc, Arc, Barrier, Mutex},
    thread,
    time::{Duration, Instant},
};

use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{json, Value};

use super::*;
use crate::{
    semantic_model_contract, ExternalSemanticSpace, SemanticEmbeddingExecutorConfig,
    SemanticEmbeddingExecutorHandle, SemanticEmbeddingExecutorKind, SemanticModelPaths,
    SemanticOnnxRuntimePaths, SharedSemanticRuntime, MAX_EXTERNAL_SEMANTIC_DIMENSIONS,
};

const HTTPS_CHILD_ENDPOINT_ENV: &str = "CTX_SEMANTIC_TEST_HTTPS_ENDPOINT";
const HTTPS_TEST_NAME: &str =
    "http_embedding_executor::tests::https_protocol_uses_injected_trust_and_ignores_proxy";
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

    fn header_count(&self, name: &str) -> usize {
        self.headers
            .iter()
            .filter(|(header, _)| header.eq_ignore_ascii_case(name))
            .count()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("request body is JSON")
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
        Self::start_with_listener(responders, false)
    }

    fn start_https(responders: Vec<Responder>) -> Self {
        Self::start_with_listener(responders, true)
    }

    fn start_with_listener(responders: Vec<Responder>, https: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let tls_config = https.then(test_tls_server_config);
            for responder in responders {
                let stream = accept_with_deadline(&listener);
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                if let Some(tls_config) = &tls_config {
                    let connection = rustls::ServerConnection::new(Arc::clone(tls_config)).unwrap();
                    let mut stream = rustls::StreamOwned::new(connection, stream);
                    serve_one(&mut stream, responder, &request_tx);
                    stream.conn.send_close_notify();
                    let _ = stream.flush();
                } else {
                    let mut stream = stream;
                    serve_one(&mut stream, responder, &request_tx);
                }
            }
        });
        Self {
            base_url: format!(
                "{}://{address}/semantic-base",
                if https { "https" } else { "http" }
            ),
            requests,
            thread: Some(thread),
        }
    }

    fn finish(mut self) -> Vec<RecordedRequest> {
        self.thread.take().unwrap().join().unwrap();
        self.requests.try_iter().collect()
    }
}

fn serve_one<Stream: Read + Write>(
    stream: &mut Stream,
    responder: Responder,
    request_tx: &mpsc::Sender<RecordedRequest>,
) {
    let request = read_request(stream);
    request_tx.send(request.clone()).unwrap();
    match responder(&request) {
        WireResponse::Close => {}
        WireResponse::Http {
            status,
            body,
            declared_length,
        } => write_response(stream, status, &body, declared_length),
    }
}

fn accept_with_deadline(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "timed out waiting for request");
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
        assert!(count > 0, "request ended before its headers");
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
        assert!(count > 0, "request body was truncated");
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
        500 => "Internal Server Error",
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

fn space(space_id: &str, dimensions: usize) -> ExternalSemanticSpace {
    ExternalSemanticSpace::new(space_id, dimensions).unwrap()
}

fn contract_value(space: &ExternalSemanticSpace) -> Value {
    json!({
        "schema_version": PROTOCOL_SCHEMA_VERSION,
        "space_id": space.space_id(),
        "dimensions": space.dimensions(),
    })
}

fn contract_reply(space: ExternalSemanticSpace) -> Responder {
    Box::new(move |_| json_http(contract_value(&space)))
}

fn unit_embedding(dimensions: usize, position: usize) -> Vec<f32> {
    let mut embedding = vec![0.0; dimensions];
    embedding[position] = 1.0;
    embedding
}

fn embedding_outputs(request: &RecordedRequest, embeddings: Vec<Vec<f32>>) -> Vec<Value> {
    request.json()["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .zip(embeddings)
        .map(|(input, embedding)| json!({"id": input["id"], "embedding": embedding}))
        .collect()
}

fn embedding_value_with_outputs(request: &RecordedRequest, embeddings: Vec<Value>) -> Value {
    let request_json = request.json();
    json!({
        "schema_version": PROTOCOL_SCHEMA_VERSION,
        "space_id": request_json["space_id"],
        "dimensions": request_json["dimensions"],
        "request_id": request_json["request_id"],
        "embeddings": embeddings,
    })
}

fn embedding_reply(position: usize) -> Responder {
    Box::new(move |request| {
        let request_json = request.json();
        let count = request_json["inputs"].as_array().unwrap().len();
        let dimensions = request_json["dimensions"].as_u64().unwrap() as usize;
        let embeddings = vec![unit_embedding(dimensions, position); count];
        json_http(embedding_value_with_outputs(
            request,
            embedding_outputs(request, embeddings),
        ))
    })
}

fn model_config() -> crate::SemanticModelConfig {
    crate::SemanticModelConfig::new(SemanticModelPaths::new(
        PathBuf::from("test-model-cache"),
        SemanticOnnxRuntimePaths::new(PathBuf::from("test-runtime-cache")),
    ))
}

fn query_failure(
    space: ExternalSemanticSpace,
    responders: Vec<Responder>,
    input: &str,
) -> anyhow::Error {
    let expected_requests = responders.len();
    let server = FakeServer::start(responders);
    let executor = super::HttpSemanticEmbeddingExecutor::new(&server.base_url, space).unwrap();
    let error = executor
        .embed_query(executor.contract().prepare_query(input.to_owned()))
        .unwrap_err();
    assert_eq!(server.finish().len(), expected_requests);
    error
}

#[test]
fn config_enforces_url_policy_and_exposes_endpoint_scoped_contracts() {
    let first = space("alpha-v1", 3);
    let second = space("beta.v2", 1_536);
    for endpoint in [
        "http://127.0.0.1:8080/base",
        "http://[::1]:8080/base",
        "https://embed.example.test/base",
    ] {
        let config = SemanticEmbeddingExecutorConfig::http(endpoint, first.clone()).unwrap();
        assert_eq!(config.kind(), SemanticEmbeddingExecutorKind::Http);
        assert_eq!(config.external_space(), Some(&first));
        assert_eq!(config.contract().external_space(), Some(&first));
        assert_eq!(
            config.contract().external_http_endpoint(),
            config.endpoint()
        );
    }
    let first_endpoint =
        SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:8080/a", first.clone()).unwrap();
    let second_endpoint =
        SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:8080/b", first.clone()).unwrap();
    let second_space =
        SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:8080/a", second).unwrap();
    assert_ne!(first_endpoint.contract(), second_endpoint.contract());
    assert_ne!(first_endpoint.contract(), second_space.contract());

    for endpoint in [
        "http://example.test",
        "http://localhost:8080",
        "ftp://127.0.0.1:8080",
        "https://user@example.test",
        "https://example.test?query=1",
        "https://example.test#fragment",
        " https://example.test",
    ] {
        assert!(SemanticEmbeddingExecutorConfig::http(endpoint, first.clone()).is_err());
    }
}

#[test]
fn discovery_gets_authenticated_contract_and_freezes_the_accepted_space() {
    let accepted = space("discovered-v1", 768);
    let server = FakeServer::start(vec![
        contract_reply(accepted.clone()),
        contract_reply(accepted.clone()),
    ]);
    let auth =
        SemanticEmbeddingExecutorAuth::bearer("secret-token".to_owned(), server.base_url.clone());
    let config =
        SemanticEmbeddingExecutorConfig::discover_http(&server.base_url, auth.clone()).unwrap();
    assert_eq!(config.external_space(), Some(&accepted));
    let handle = SemanticEmbeddingExecutorHandle::build_with_auth(
        config,
        auth,
        SharedSemanticRuntime::default(),
        model_config(),
    )
    .unwrap();
    handle.verify_contract().unwrap();

    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.method == "GET"));
    assert!(requests
        .iter()
        .all(|request| request.path == "/semantic-base/v1/contract"));
    assert!(requests
        .iter()
        .all(|request| request.header("authorization") == Some("Bearer secret-token")));
}

#[test]
fn public_verification_is_content_free_cached_and_drift_fails_permanently() {
    let accepted = space("accepted-v1", 4);
    let drifted = space("drifted-v2", 4);
    let server = FakeServer::start(vec![contract_reply(drifted)]);
    let executor = super::HttpSemanticEmbeddingExecutor::new(&server.base_url, accepted).unwrap();
    let first = executor.verify_contract().unwrap_err();
    assert!(semantic_embedding_failure_is_permanent(&first));
    let second = executor.verify_contract().unwrap_err();
    assert!(semantic_embedding_failure_is_permanent(&second));
    assert!(!executor.contract_verified());
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert!(requests[0].body.is_empty());
}

#[test]
fn query_and_documents_send_exact_raw_text_and_protocol_assertions() {
    let accepted = space("raw-text-v1", 5);
    let server = FakeServer::start(vec![
        contract_reply(accepted.clone()),
        embedding_reply(1),
        embedding_reply(2),
    ]);
    let executor =
        super::HttpSemanticEmbeddingExecutor::new(&server.base_url, accepted.clone()).unwrap();
    assert_eq!(
        executor
            .embed_query(
                executor
                    .contract()
                    .prepare_query("  query: keep exact 世界".to_owned())
            )
            .unwrap(),
        unit_embedding(5, 1)
    );
    assert_eq!(
        executor
            .embed_documents(
                executor.contract().prepare_documents(vec![
                    "passage: already raw".to_owned(),
                    "  leading whitespace".to_owned(),
                ]),
                None,
            )
            .unwrap(),
        vec![unit_embedding(5, 2); 2]
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(request.header(SCHEMA_HEADER), Some("1"));
        assert_eq!(request.header_count("authorization"), 0);
    }
    let query = requests[1].json();
    assert_eq!(query["schema_version"], 1);
    assert_eq!(query["space_id"], "raw-text-v1");
    assert_eq!(query["dimensions"], 5);
    assert_eq!(query["input_kind"], "query");
    assert_eq!(query["inputs"][0]["text"], "  query: keep exact 世界");
    assert!(query["request_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    assert!(query["inputs"][0]["id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    let documents = requests[2].json();
    assert_eq!(documents["input_kind"], "documents");
    assert_eq!(documents["inputs"][0]["text"], "passage: already raw");
    assert_eq!(documents["inputs"][1]["text"], "  leading whitespace");
    assert_ne!(documents["inputs"][0]["id"], documents["inputs"][1]["id"]);
}

#[test]
fn response_space_request_id_cardinality_and_output_ids_are_exact() {
    let accepted = space("identity-v1", 3);
    let cases: Vec<Responder> = vec![
        Box::new(|request| {
            let mut value = embedding_value_with_outputs(
                request,
                embedding_outputs(request, vec![unit_embedding(3, 0)]),
            );
            value["space_id"] = json!("other-v1");
            json_http(value)
        }),
        Box::new(|request| {
            let mut value = embedding_value_with_outputs(
                request,
                embedding_outputs(request, vec![unit_embedding(3, 0)]),
            );
            value["dimensions"] = json!(4);
            json_http(value)
        }),
        Box::new(|request| {
            let mut value = embedding_value_with_outputs(
                request,
                embedding_outputs(request, vec![unit_embedding(3, 0)]),
            );
            value["request_id"] = json!("wrong-request");
            json_http(value)
        }),
        Box::new(|request| json_http(embedding_value_with_outputs(request, Vec::new()))),
        Box::new(|request| {
            json_http(embedding_value_with_outputs(
                request,
                vec![json!({"id": "unknown", "embedding": unit_embedding(3, 0)})],
            ))
        }),
    ];
    for responder in cases {
        let error = query_failure(
            accepted.clone(),
            vec![contract_reply(accepted.clone()), responder],
            "identity",
        );
        assert!(semantic_embedding_failure_is_permanent(&error));
    }
}

#[test]
fn shuffled_outputs_are_restored_and_duplicate_ids_fail_closed() {
    let accepted = space("mapping-v1", 4);
    let server = FakeServer::start(vec![
        contract_reply(accepted.clone()),
        Box::new(|request| {
            let mut outputs =
                embedding_outputs(request, vec![unit_embedding(4, 0), unit_embedding(4, 1)]);
            outputs.swap(0, 1);
            json_http(embedding_value_with_outputs(request, outputs))
        }),
    ]);
    let executor =
        super::HttpSemanticEmbeddingExecutor::new(&server.base_url, accepted.clone()).unwrap();
    assert_eq!(
        executor
            .embed_documents(
                executor
                    .contract()
                    .prepare_documents(vec!["one".to_owned(), "two".to_owned()]),
                None,
            )
            .unwrap(),
        vec![unit_embedding(4, 0), unit_embedding(4, 1)]
    );
    assert_eq!(server.finish().len(), 2);

    let duplicate_error = query_failure(
        accepted.clone(),
        vec![
            contract_reply(accepted),
            Box::new(|request| {
                let output = embedding_outputs(request, vec![unit_embedding(4, 0)])[0].clone();
                json_http(embedding_value_with_outputs(
                    request,
                    vec![output.clone(), output],
                ))
            }),
        ],
        "duplicate",
    );
    assert!(duplicate_error.to_string().contains("duplicate input ID"));
}

#[test]
fn vectors_must_have_exact_dimensions_and_be_finite_nonzero_and_normalized() {
    assert!(validate_embedding(&unit_embedding(3, 0), 3).is_ok());
    assert!(validate_embedding(&unit_embedding(2, 0), 3)
        .unwrap_err()
        .to_string()
        .contains("wrong dimensions"));
    assert!(validate_embedding(&[f32::NAN, 0.0, 0.0], 3)
        .unwrap_err()
        .to_string()
        .contains("non-finite"));
    assert!(validate_embedding(&[0.0, 0.0, 0.0], 3)
        .unwrap_err()
        .to_string()
        .contains("zero-norm"));
    assert!(validate_embedding(&[1.0, 1.0, 0.0], 3)
        .unwrap_err()
        .to_string()
        .contains("not L2-normalized"));

    let accepted = space("wrong-vector-v1", 3);
    let error = query_failure(
        accepted.clone(),
        vec![
            contract_reply(accepted),
            Box::new(|request| {
                json_http(embedding_value_with_outputs(
                    request,
                    embedding_outputs(request, vec![unit_embedding(4, 0)]),
                ))
            }),
        ],
        "wrong dimensions",
    );
    assert!(semantic_embedding_failure_is_permanent(&error));
}

#[test]
fn auth_is_required_for_remote_endpoints_bound_exactly_and_redacted() {
    let accepted = space("auth-v1", 8);
    assert!(super::HttpSemanticEmbeddingExecutor::new(
        "https://embed.example.test/base",
        accepted.clone()
    )
    .unwrap_err()
    .to_string()
    .contains("requires an authentication token"));
    assert!(super::HttpSemanticEmbeddingExecutor::new_with_auth(
        "https://embed.example.test/base",
        accepted.clone(),
        SemanticEmbeddingExecutorAuth::bearer(
            "secret".to_owned(),
            "https://other.example.test/base".to_owned(),
        ),
    )
    .is_err());
    assert!(super::HttpSemanticEmbeddingExecutor::new_with_auth(
        "https://embed.example.test/base",
        accepted,
        SemanticEmbeddingExecutorAuth::bearer(
            "bad token".to_owned(),
            "https://embed.example.test/base".to_owned(),
        ),
    )
    .is_err());
    let auth = SemanticEmbeddingExecutorAuth::bearer(
        "do-not-print".to_owned(),
        "https://embed.example.test/base".to_owned(),
    );
    assert!(!format!("{auth:?}").contains("do-not-print"));
}

#[test]
fn https_protocol_uses_injected_trust_and_ignores_proxy() {
    let accepted = space("https-v1", 6);
    if let Ok(endpoint) = env::var(HTTPS_CHILD_ENDPOINT_ENV) {
        let certificate =
            ureq_semantic::tls::Certificate::from_pem(TEST_CA_CERTIFICATE_PEM.as_bytes()).unwrap();
        let auth =
            SemanticEmbeddingExecutorAuth::bearer("https-token".to_owned(), endpoint.clone());
        let executor = super::HttpSemanticEmbeddingExecutor::new_with_auth_and_root_certs(
            &endpoint,
            accepted,
            auth,
            ureq_semantic::tls::RootCerts::new_with_certs(&[certificate]),
        )
        .unwrap();
        assert_eq!(
            executor
                .embed_query(executor.contract().prepare_query("raw https".to_owned()))
                .unwrap(),
            unit_embedding(6, 2)
        );
        return;
    }

    let server = FakeServer::start_https(vec![contract_reply(accepted), embedding_reply(2)]);
    let output = std::process::Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            HTTPS_TEST_NAME,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HTTPS_CHILD_ENDPOINT_ENV, &server.base_url)
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
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|request| request.header("authorization") == Some("Bearer https-token")));
}

#[test]
fn transport_has_no_redirects_or_proxy_and_bounded_timeouts() {
    let accepted = space("transport-v1", 8);
    let executor =
        super::HttpSemanticEmbeddingExecutor::new("http://127.0.0.1:9", accepted.clone()).unwrap();
    let config = executor.agent.config();
    let timeouts = config.timeouts();
    assert!(!config.http_status_as_error());
    assert_eq!(config.max_redirects(), 0);
    assert!(config.proxy().is_none());
    assert_eq!(timeouts.global, Some(EXECUTION_BUDGET));
    assert_eq!(timeouts.resolve, Some(DNS_RESOLVE_TIMEOUT));
    assert_eq!(timeouts.connect, Some(CONNECT_TIMEOUT));

    let server = FakeServer::start(vec![Box::new(|_| http(302, Vec::new()))]);
    let executor = super::HttpSemanticEmbeddingExecutor::new(&server.base_url, accepted).unwrap();
    assert!(semantic_embedding_failure_is_permanent(
        &executor.verify_contract().unwrap_err()
    ));
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn resolver_timeouts_have_fixed_workers_and_a_bounded_queue() {
    let (lookup_started_sender, lookup_started_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let release_receiver = Arc::new(Mutex::new(release_receiver));
    let runtime = ResolverRuntime::spawn(Arc::new(move |_, _| {
        lookup_started_sender
            .send(())
            .map_err(|_| std::io::Error::other("resolver test receiver stopped"))?;
        release_receiver
            .lock()
            .map_err(|_| std::io::Error::other("resolver test release lock failed"))?
            .recv()
            .map_err(|_| std::io::Error::other("resolver test release sender stopped"))?;
        Ok(vec!["127.0.0.1:443".parse().unwrap()])
    }))
    .unwrap();
    let resolver = runtime.resolver().unwrap();
    let uri: ureq_semantic::http::Uri = "https://resolver.test/".parse().unwrap();
    let config = ureq_semantic::config::Config::default();
    let blocking_timeout = ureq_semantic::unversioned::transport::NextTimeout {
        after: ureq_semantic::unversioned::transport::time::Duration::from_millis(50),
        reason: ureq_semantic::Timeout::Resolve,
    };

    let blocked = (0..RESOLVER_THREADS)
        .map(|_| {
            let resolver = resolver.clone();
            let uri = uri.clone();
            let config = config.clone();
            thread::spawn(move || {
                matches!(
                    ureq_semantic::unversioned::resolver::Resolver::resolve(
                        &resolver,
                        &uri,
                        &config,
                        blocking_timeout,
                    ),
                    Err(ureq_semantic::Error::Timeout(_))
                )
            })
        })
        .collect::<Vec<_>>();
    for _ in 0..RESOLVER_THREADS {
        lookup_started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
    }
    for blocked in blocked {
        assert!(blocked.join().unwrap());
    }

    let queued_timeout = ureq_semantic::unversioned::transport::NextTimeout {
        after: ureq_semantic::unversioned::transport::time::Duration::from_millis(5),
        reason: ureq_semantic::Timeout::Resolve,
    };
    for _ in 0..RESOLVER_QUEUE_CAPACITY {
        assert!(matches!(
            ureq_semantic::unversioned::resolver::Resolver::resolve(
                &resolver,
                &uri,
                &config,
                queued_timeout,
            ),
            Err(ureq_semantic::Error::Timeout(_))
        ));
    }
    let saturated_at = Instant::now();
    assert!(matches!(
        ureq_semantic::unversioned::resolver::Resolver::resolve(
            &resolver,
            &uri,
            &config,
            queued_timeout,
        ),
        Err(ureq_semantic::Error::Timeout(_))
    ));
    assert!(saturated_at.elapsed() < Duration::from_millis(50));
    assert!(lookup_started_receiver.try_recv().is_err());

    for _ in 0..RESOLVER_THREADS {
        release_sender.send(()).unwrap();
    }
    drop(resolver);
    drop(release_sender);
    drop(runtime);
    assert!(lookup_started_receiver.try_recv().is_err());
}

#[test]
fn transport_and_retryable_status_retry_once_without_changing_request_id() {
    let accepted = space("retry-v1", 4);
    let server = FakeServer::start(vec![
        Box::new(|_| WireResponse::Close),
        contract_reply(accepted.clone()),
        Box::new(|_| http(503, Vec::new())),
        embedding_reply(1),
    ]);
    let executor = super::HttpSemanticEmbeddingExecutor::new(&server.base_url, accepted).unwrap();
    assert_eq!(
        executor
            .embed_query(executor.contract().prepare_query("retry".to_owned()))
            .unwrap(),
        unit_embedding(4, 1)
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2].body, requests[3].body);
    assert_eq!(
        requests[2].json()["request_id"],
        requests[3].json()["request_id"]
    );
}

#[test]
fn retry_exhaustion_is_transient_and_permanent_protocol_failure_is_cached() {
    let accepted = space("lifecycle-v1", 4);
    let server = FakeServer::start(vec![
        contract_reply(accepted.clone()),
        Box::new(|_| http(500, Vec::new())),
        Box::new(|_| http(500, Vec::new())),
        embedding_reply(2),
    ]);
    let executor =
        super::HttpSemanticEmbeddingExecutor::new(&server.base_url, accepted.clone()).unwrap();
    let first = executor
        .embed_query(executor.contract().prepare_query("first".to_owned()))
        .unwrap_err();
    assert!(!semantic_embedding_failure_is_permanent(&first));
    assert_eq!(
        executor
            .embed_query(executor.contract().prepare_query("second".to_owned()))
            .unwrap(),
        unit_embedding(4, 2)
    );
    assert_eq!(server.finish().len(), 4);

    let server = FakeServer::start(vec![
        contract_reply(accepted.clone()),
        Box::new(|_| json_http(json!({"malformed": true}))),
    ]);
    let executor = super::HttpSemanticEmbeddingExecutor::new(&server.base_url, accepted).unwrap();
    let first = executor
        .embed_query(executor.contract().prepare_query("first".to_owned()))
        .unwrap_err();
    assert!(semantic_embedding_failure_is_permanent(&first));
    let second = executor
        .embed_query(executor.contract().prepare_query("second".to_owned()))
        .unwrap_err();
    assert!(semantic_embedding_failure_is_permanent(&second));
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn aggregate_deadline_and_response_metadata_bounds_are_enforced() {
    let accepted = space("bounds-v1", 8);
    let executor =
        super::HttpSemanticEmbeddingExecutor::new("http://127.0.0.1:9", accepted.clone()).unwrap();
    let inputs = vec!["deadline probe".to_owned()];
    let started = Instant::now();
    let error = executor
        .embed(
            InputKind::Query,
            &inputs,
            Instant::now() + Duration::from_millis(40),
        )
        .unwrap_err();
    assert!(error.to_string().contains("aggregate time budget"));
    assert!(started.elapsed() < Duration::from_secs(1));

    let server = FakeServer::start(vec![Box::new(|_| {
        http(200, vec![b' '; MAX_CONTRACT_BODY_BYTES + 1])
    })]);
    let executor = super::HttpSemanticEmbeddingExecutor::new(&server.base_url, accepted).unwrap();
    let error = executor.verify_contract().unwrap_err();
    assert!(semantic_embedding_failure_is_permanent(&error));
    assert!(error.to_string().contains("body size limit"));
    assert_eq!(server.finish().len(), 1);

    let accepted = space("embedding-body-bound-v1", 8);
    let error = query_failure(
        accepted.clone(),
        vec![
            contract_reply(accepted),
            Box::new(|_| WireResponse::Http {
                status: 200,
                body: Vec::new(),
                declared_length: Some(MAX_RESPONSE_BODY_BYTES + 1),
            }),
        ],
        "oversized response",
    );
    assert!(semantic_embedding_failure_is_permanent(&error));
    assert!(error.to_string().contains("body size limit"));
}

#[test]
fn malformed_or_unsafe_discovery_contracts_fail_closed() {
    for value in [
        json!({"schema_version": 2, "space_id": "safe-v1", "dimensions": 8}),
        json!({"schema_version": 1, "space_id": "bad space", "dimensions": 8}),
        json!({"schema_version": 1, "space_id": "safe-v1", "dimensions": 0}),
        json!({"schema_version": 1, "space_id": "safe-v1", "dimensions": 4097}),
        json!({"schema_version": 1, "space_id": "safe-v1", "dimensions": 8, "extra": true}),
    ] {
        let server = FakeServer::start(vec![Box::new(move |_| json_http(value.clone()))]);
        assert!(SemanticEmbeddingExecutorConfig::discover_http(
            &server.base_url,
            SemanticEmbeddingExecutorAuth::none(),
        )
        .is_err());
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn scalar_budget_bounds_document_batches() {
    let accepted = space("scalar-budget-v1", MAX_EXTERNAL_SEMANTIC_DIMENSIONS);
    let server = FakeServer::start(vec![
        contract_reply(accepted.clone()),
        embedding_reply(0),
        embedding_reply(1),
    ]);
    let executor = super::HttpSemanticEmbeddingExecutor::new(&server.base_url, accepted).unwrap();
    let direct_inputs = (0..65)
        .map(|index| format!("raw-{index}"))
        .collect::<Vec<_>>();
    let direct_error = executor
        .prepare_embeddings_request(InputKind::Documents, &direct_inputs)
        .err()
        .expect("oversized scalar work unit must fail");
    assert!(semantic_embedding_failure_is_permanent(&direct_error));
    let documents = (0..65).map(|index| format!("raw-{index}")).collect();
    let embeddings = executor
        .embed_documents(executor.contract().prepare_documents(documents), None)
        .unwrap();
    assert_eq!(embeddings.len(), 65);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].json()["inputs"].as_array().unwrap().len(), 64);
    assert_eq!(requests[2].json()["inputs"].as_array().unwrap().len(), 1);
}

#[test]
fn request_body_preflight_accepts_exact_limit_and_rejects_over_limit_permanently() {
    let accepted = space("request-body-v1", 8);
    let executor =
        super::HttpSemanticEmbeddingExecutor::new("http://127.0.0.1:9", accepted).unwrap();
    let empty_body_len = RequestBodySizer::new(&executor, InputKind::Query)
        .unwrap()
        .body_len();
    let empty_input_len = encoded_json_len(
        &EmbeddingInput {
            id: UUID_WIRE_VALUE,
            text: "",
        },
        MAX_REQUEST_BODY_BYTES,
    )
    .unwrap()
    .unwrap();
    let max_text_len = MAX_REQUEST_BODY_BYTES - empty_body_len - empty_input_len;
    let text = "x".repeat(max_text_len);
    let request = executor
        .prepare_embeddings_request(InputKind::Query, &[text])
        .unwrap();
    assert_eq!(request.body.len(), MAX_REQUEST_BODY_BYTES);
    drop(request);

    let text = "x".repeat(max_text_len + 1);
    let error = executor
        .prepare_embeddings_request(InputKind::Query, &[text])
        .err()
        .expect("over-limit request must fail preflight");
    assert!(semantic_embedding_failure_is_permanent(&error));
    assert!(error
        .to_string()
        .contains("request exceeds the body size limit"));

    let escaped = "\"".repeat((MAX_REQUEST_BODY_BYTES / 2) + 1);
    let error = executor
        .prepare_embeddings_request(InputKind::Query, &[escaped])
        .err()
        .expect("escaped over-limit request must fail preflight");
    assert!(semantic_embedding_failure_is_permanent(&error));
    assert!(error
        .to_string()
        .contains("request exceeds the body size limit"));
}

#[test]
fn encoded_byte_budget_splits_documents_without_changing_order() {
    let accepted = space("byte-batches-v1", 8);
    let server = FakeServer::start(vec![
        contract_reply(accepted.clone()),
        embedding_reply(0),
        embedding_reply(1),
    ]);
    let executor = super::HttpSemanticEmbeddingExecutor::new(&server.base_url, accepted).unwrap();
    let text_len = (MAX_REQUEST_BODY_BYTES / 3) + 128;
    let documents = (0..3)
        .map(|index| format!("{index}:{}", "x".repeat(text_len)))
        .collect::<Vec<_>>();
    let embeddings = executor
        .embed_documents(executor.contract().prepare_documents(documents), None)
        .unwrap();
    assert_eq!(
        embeddings,
        vec![
            unit_embedding(8, 0),
            unit_embedding(8, 0),
            unit_embedding(8, 1)
        ]
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    let batch_sizes = requests[1..]
        .iter()
        .map(|request| {
            assert!(request.body.len() <= MAX_REQUEST_BODY_BYTES);
            request.json()["inputs"].as_array().unwrap().len()
        })
        .collect::<Vec<_>>();
    assert_eq!(batch_sizes, vec![2, 1]);
}

#[test]
fn empty_documents_are_content_free_and_concurrent_first_use_shares_one_get() {
    let accepted = space("concurrent-v1", 8);
    let executor =
        super::HttpSemanticEmbeddingExecutor::new("http://127.0.0.1:9", accepted.clone()).unwrap();
    assert!(executor
        .embed_documents(executor.contract().prepare_documents(Vec::new()), None)
        .unwrap()
        .is_empty());
    assert!(!executor.contract_verified());

    let responders = std::iter::once(contract_reply(accepted.clone()))
        .chain((0..8).map(|_| embedding_reply(0)))
        .collect();
    let server = FakeServer::start(responders);
    let executor =
        Arc::new(super::HttpSemanticEmbeddingExecutor::new(&server.base_url, accepted).unwrap());
    let barrier = Arc::new(Barrier::new(8));
    let threads = (0..8)
        .map(|index| {
            let executor = Arc::clone(&executor);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                executor
                    .embed_query(
                        executor
                            .contract()
                            .prepare_query(format!("concurrent-{index}")),
                    )
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        assert_eq!(thread.join().unwrap(), unit_embedding(8, 0));
    }
    let requests = server.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "GET")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        8
    );
}

#[test]
fn executor_handle_keeps_builtin_and_http_behind_one_interface() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<super::HttpSemanticEmbeddingExecutor>();
    assert_send_sync::<SemanticEmbeddingExecutorHandle>();

    let builtin = SemanticEmbeddingExecutorHandle::build(
        SemanticEmbeddingExecutorConfig::builtin(),
        SharedSemanticRuntime::default(),
        model_config(),
    )
    .unwrap();
    assert_eq!(builtin.kind(), SemanticEmbeddingExecutorKind::Builtin);
    assert_eq!(builtin.executor().contract(), semantic_model_contract());
    assert!(builtin.verify_contract().is_ok());

    let accepted = space("handle-v1", 12);
    let config =
        SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:9", accepted.clone()).unwrap();
    let http = SemanticEmbeddingExecutorHandle::build(
        config,
        SharedSemanticRuntime::default(),
        model_config(),
    )
    .unwrap();
    assert_eq!(http.kind(), SemanticEmbeddingExecutorKind::Http);
    assert_eq!(http.http_executor().unwrap().external_space(), &accepted);
    assert_eq!(http.executor().contract().dimensions(), 12);
}
