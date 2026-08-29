use std::{
    cell::Cell,
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{CoreEventRecord, EventSearchFilters, GenerationWriter, WriterOptions};
use ctx_semantic_index::{
    source_backed_semantic_vector_path,
    test_support::{pinned_flat_generation, publish_chunk_replacements, semantic_chunk_document},
    SemanticBatchEmbedder, SemanticChunkDocument, SemanticDocumentBuilder, SemanticEventDocument,
    SemanticQueryPin, SemanticVectorStore, SourceBackedGenerationPin,
    SourceBackedSemanticDocumentBuilder,
};
use fs2::FileExt as _;
use serde_json::Value;
use uuid::Uuid;

use super::*;

fn external_executor_config(endpoint: &str) -> SemanticEmbeddingExecutorConfig {
    SemanticEmbeddingExecutorConfig::http(
        endpoint,
        crate::ExternalSemanticSpace::new("test-space", 7).unwrap(),
    )
    .unwrap()
}

fn default_compiled_filter() -> CompiledSearchFilter {
    CompiledSearchFilter::compile(EventSearchFilters::default()).unwrap()
}

fn semantic_index(root: &Path) -> Result<(VerifiedIndex, Uuid)> {
    semantic_index_revision(root, 1, true)
}

fn semantic_index_revision(
    root: &Path,
    revision: u64,
    include_record: bool,
) -> Result<(VerifiedIndex, Uuid)> {
    semantic_index_revision_at(&root.join("index"), revision, include_record)
}

fn semantic_index_revision_at(
    index_root: &Path,
    revision: u64,
    include_record: bool,
) -> Result<(VerifiedIndex, Uuid)> {
    let source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8("query-adapter.jsonl")?)?,
    )?;
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("query-adapter-session")?)?;
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("message", TypedKey::U64(revision))?,
        subrecord_selector: None,
    })?;
    let mut writer = GenerationWriter::open(index_root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    writer.begin_source(source.clone())?;
    if include_record {
        let mut record = CoreRecord::new_selected(
            event_id,
            session_id,
            source.clone(),
            revision,
            "message",
            "semantic-query-adapter-v1",
            format!("query adapter fixture {revision}"),
        )?;
        record.provider_session_id = Some("query-adapter-session".to_owned());
        record.native_event_id = Some(TypedKey::U64(revision));
        record.role = Some("user".to_owned());
        record.validate_contract()?;
        writer.add_core_record(record)?;
    }
    let record_count = u64::from(include_record);
    let observation =
        SourceObservation::new(source, "regular-file-v1", revision.to_le_bytes().to_vec())?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "query-adapter-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: record_count,
            retained_records: record_count,
            indexed_documents: record_count,
            certified_bytes: record_count,
            ..ScannedSourceCounts::default()
        },
    )?)?;
    writer.commit(|_| true)?;
    Ok((VerifiedIndex::open_pinned(index_root)?, event_id.as_uuid()))
}

struct RejectingSemanticPorts;

impl SemanticDocumentBuilder for RejectingSemanticPorts {
    fn build_document(
        &mut self,
        _record: &CoreEventRecord,
    ) -> Result<Option<SemanticEventDocument>> {
        Err(anyhow!(
            "empty semantic fixture unexpectedly requested a document"
        ))
    }
}

impl SemanticBatchEmbedder for RejectingSemanticPorts {
    fn embed_chunks(&mut self, _chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        Err(anyhow!(
            "empty semantic fixture unexpectedly requested embeddings"
        ))
    }
}

fn acknowledge_empty_generation(
    store: &mut SemanticVectorStore,
    index: &VerifiedIndex,
) -> Result<()> {
    let mut builder = RejectingSemanticPorts;
    let mut embedder = RejectingSemanticPorts;
    for _ in 0..32 {
        if store
            .reconcile_source_backed_index(index, &mut builder, &mut embedder)?
            .ready()
        {
            return Ok(());
        }
    }
    Err(anyhow!("empty semantic fixture did not converge"))
}

fn embedding() -> Vec<f32> {
    let mut embedding = vec![0.0; semantic_model_contract().dimensions()];
    embedding[0] = 1.0;
    embedding
}

static SEMANTIC_ENV_LOCK: Mutex<()> = Mutex::new(());

struct SemanticEnvironmentGuard {
    _lock: MutexGuard<'static, ()>,
    token: Option<OsString>,
    endpoint: Option<OsString>,
    cache_dir: Option<OsString>,
}

impl SemanticEnvironmentGuard {
    fn http_auth(endpoint: &str) -> Self {
        Self::set(
            Some(OsString::from("passive-semantic-test-token")),
            Some(OsString::from(endpoint)),
            None,
        )
    }

    fn invalid_cache(cache_dir: &Path) -> Self {
        Self::set(None, None, Some(cache_dir.as_os_str().to_owned()))
    }

    fn set(
        token: Option<OsString>,
        endpoint: Option<OsString>,
        cache_dir: Option<OsString>,
    ) -> Self {
        let lock = SEMANTIC_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved_token = env::var_os(ctx_semantic_model::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV);
        let saved_endpoint =
            env::var_os(ctx_semantic_model::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV);
        let saved_cache_dir = env::var_os("CTX_SEMANTIC_CACHE_DIR");
        set_environment(
            ctx_semantic_model::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV,
            token.as_deref(),
        );
        set_environment(
            ctx_semantic_model::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
            endpoint.as_deref(),
        );
        set_environment("CTX_SEMANTIC_CACHE_DIR", cache_dir.as_deref());
        Self {
            _lock: lock,
            token: saved_token,
            endpoint: saved_endpoint,
            cache_dir: saved_cache_dir,
        }
    }
}

impl Drop for SemanticEnvironmentGuard {
    fn drop(&mut self) {
        set_environment(
            ctx_semantic_model::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV,
            self.token.take().as_deref(),
        );
        set_environment(
            ctx_semantic_model::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
            self.endpoint.take().as_deref(),
        );
        set_environment("CTX_SEMANTIC_CACHE_DIR", self.cache_dir.take().as_deref());
    }
}

fn set_environment(name: &str, value: Option<&std::ffi::OsStr>) {
    match value {
        Some(value) => env::set_var(name, value),
        None => env::remove_var(name),
    }
}

#[derive(Debug)]
struct RecordedHttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Value,
}

impl RecordedHttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

struct LoopbackEmbeddingServer {
    base_url: String,
    requests: Arc<Mutex<Vec<RecordedHttpRequest>>>,
    thread: thread::JoinHandle<()>,
}

impl LoopbackEmbeddingServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}/semantic-base", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let thread = thread::spawn(move || {
            for request_number in 0..4 {
                let (mut stream, _) = listener.accept().expect("accept embedding request");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("set embedding request timeout");
                let request = read_http_request(&mut stream);
                let response = match request_number {
                    0 => compact_json(json!({
                        "schema_version": 1,
                        "model_key": semantic_model_contract().model_key(),
                        "model_contract_fingerprint": semantic_model_contract().fingerprint(),
                    })),
                    1 => embedding_response(
                        &request.body,
                        ctx_semantic_model::test_support::http_embedding_canary_vector("query"),
                    ),
                    2 => embedding_response(
                        &request.body,
                        ctx_semantic_model::test_support::http_embedding_canary_vector("documents"),
                    ),
                    3 => embedding_response(&request.body, embedding()),
                    _ => unreachable!(),
                };
                captured.lock().unwrap().push(request);
                write_http_json(&mut stream, &response);
            }
        });
        Self {
            base_url,
            requests,
            thread,
        }
    }

    fn finish(self) -> Vec<RecordedHttpRequest> {
        self.thread.join().expect("join embedding server");
        Arc::try_unwrap(self.requests)
            .expect("embedding server has no remaining request references")
            .into_inner()
            .unwrap()
    }
}

fn read_http_request(stream: &mut TcpStream) -> RecordedHttpRequest {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).expect("read embedding request");
        assert!(read > 0, "embedding request ended before headers");
        bytes.extend_from_slice(&chunk[..read]);
    };
    let header = std::str::from_utf8(&bytes[..header_end]).expect("embedding headers utf-8");
    let mut lines = header.trim_end().split("\r\n");
    let mut request_line = lines.next().expect("request line").split_whitespace();
    let method = request_line.next().expect("method").to_owned();
    let path = request_line.next().expect("path").to_owned();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').expect("header separator");
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect::<Vec<_>>();
    let content_length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .map(|(_, value)| value.parse::<usize>().expect("content length"))
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).expect("read embedding body");
        assert!(read > 0, "embedding request ended before body");
        bytes.extend_from_slice(&chunk[..read]);
    }
    let body = if content_length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..header_end + content_length])
            .expect("embedding body json")
    };
    RecordedHttpRequest {
        method,
        path,
        headers,
        body,
    }
}

fn embedding_response(request: &Value, embedding: Vec<f32>) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "model_key": semantic_model_contract().model_key(),
        "model_contract_fingerprint": semantic_model_contract().fingerprint(),
        "request_id": request["request_id"],
        "embeddings": request["inputs"].as_array().expect("embedding inputs").iter().map(|input| json!({
            "id": input["id"],
            "embedding": embedding,
        })).collect::<Vec<_>>(),
    }))
}

fn write_http_json(stream: &mut TcpStream, body: &Value) {
    let body = serde_json::to_vec(body).expect("embedding response json");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .expect("write embedding response headers");
    stream
        .write_all(&body)
        .expect("write embedding response body");
    stream.flush().expect("flush embedding response");
}

fn tree_snapshot(root: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    fn visit(root: &Path, path: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
        if path.is_file() {
            files.push((path.strip_prefix(root)?.to_path_buf(), fs::read(path)?));
            return Ok(());
        }
        for entry in fs::read_dir(path)? {
            visit(root, &entry?.path(), files)?;
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn durable_query_state_snapshot(
    data_root: &Path,
    cache_dir: &Path,
) -> Result<(Vec<u8>, Vec<u8>, Vec<(PathBuf, Vec<u8>)>)> {
    let semantic = source_backed_semantic_vector_path(data_root);
    Ok((
        fs::read(data_root.join("index").join("active-generation.json"))?,
        fs::read(semantic.join("state.sqlite"))?,
        if cache_dir.exists() {
            tree_snapshot(cache_dir)?
        } else {
            Vec::new()
        },
    ))
}

fn daemon_embedding_response(contract: &SemanticModelContract, embedding: Value) -> Value {
    compact_json(json!({
        "ok": true,
        "schema_version": DAEMON_SEMANTIC_QUERY_SCHEMA_VERSION,
        "model_key": contract.model_key(),
        "model_contract_fingerprint": contract.fingerprint(),
        "executor_route_identity": contract.executor_route_identity(),
        "query_embed_ms": 17,
        "embedding": embedding,
    }))
}

#[test]
fn request_adapter_borrows_the_exact_data_root() {
    let data_root = std::path::PathBuf::from("borrowed-query-root");
    let adapter = SemanticQueryAdapter::new(&data_root);

    assert!(std::ptr::eq(adapter.data_root, data_root.as_path()));
}

#[test]
fn daemon_query_embedding_request_binds_the_model_contract_and_executor_route() {
    let contract = semantic_model_contract();

    assert_eq!(
        daemon_query_embedding_request(contract, "query text"),
        compact_json(json!({
            "schema_version": DAEMON_SEMANTIC_QUERY_SCHEMA_VERSION,
            "op": "embed_query",
            "model_key": contract.model_key(),
            "model_contract_fingerprint": contract.fingerprint(),
            "executor_route_identity": contract.executor_route_identity(),
            "text": "query text",
        }))
    );
}

#[test]
fn daemon_query_embedding_response_accepts_the_exact_model_contract() -> Result<()> {
    let contract = semantic_model_contract();
    let expected = embedding();
    let response = daemon_embedding_response(contract, json!(expected));

    let (actual, query_embed_ms) = parse_daemon_query_embedding_response(&response, contract)?;

    assert_eq!(actual, expected);
    assert_eq!(query_embed_ms, 17);
    Ok(())
}

#[test]
fn daemon_query_embedding_response_rejects_v1_without_a_routing_fence() {
    let contract = semantic_model_contract();
    let expected = embedding();
    let mut response = daemon_embedding_response(contract, json!(expected));
    let response_object = response.as_object_mut().expect("object");
    response_object.insert("schema_version".to_owned(), json!(1));
    response_object.remove("executor_route_identity");

    let error = parse_daemon_query_embedding_response(&response, contract)
        .expect_err("a V1 response cannot prove the selected executor route");

    assert_eq!(
        error.to_string(),
        "daemon query response schema_version mismatch"
    );
}

#[test]
fn daemon_query_embedding_response_rejects_incompatible_protocol_identity() {
    let contract = semantic_model_contract();
    let valid = daemon_embedding_response(contract, json!(embedding()));
    let mut invalid = Vec::new();

    let mut missing_ok = valid.clone();
    missing_ok.as_object_mut().expect("object").remove("ok");
    invalid.push(("missing ok".to_owned(), missing_ok, "daemon query failed"));

    let mut negative_ok = valid.clone();
    negative_ok["ok"] = Value::Bool(false);
    negative_ok["error"] = json!("daemon rejected response");
    invalid.push((
        "negative ok".to_owned(),
        negative_ok,
        "daemon rejected response",
    ));

    for (case, field, mismatch, expected) in [
        (
            "schema",
            "schema_version",
            json!(1),
            "daemon query response schema_version mismatch",
        ),
        (
            "model key",
            "model_key",
            json!("different-model"),
            "daemon query response model key mismatch",
        ),
        (
            "model contract fingerprint",
            "model_contract_fingerprint",
            json!("sha256:mismatched"),
            "daemon query response model contract fingerprint mismatch",
        ),
        (
            "executor route identity",
            "executor_route_identity",
            json!("sha256:mismatched"),
            "daemon query response executor route identity mismatch",
        ),
    ] {
        let mut missing = valid.clone();
        missing.as_object_mut().expect("object").remove(field);
        invalid.push((format!("missing {case}"), missing, expected));

        let mut mismatched = valid.clone();
        mismatched[field] = mismatch;
        invalid.push((format!("mismatched {case}"), mismatched, expected));
    }

    for (case, response, expected) in invalid {
        let error = parse_daemon_query_embedding_response(&response, contract)
            .expect_err("an incompatible daemon response must fail closed");

        assert_eq!(error.to_string(), expected, "{case}");
    }
}

#[test]
fn client_rejects_a_stale_same_space_daemon_on_another_endpoint() {
    let contract_a = ctx_semantic_index::external_http_semantic_model_contract(
        "http://127.0.0.1:41040",
        "test-space",
        7,
    )
    .unwrap();
    let contract_b = ctx_semantic_index::external_http_semantic_model_contract(
        "http://127.0.0.1:41041",
        "test-space",
        7,
    )
    .unwrap();
    assert_eq!(contract_a.fingerprint(), contract_b.fingerprint());
    assert_ne!(
        contract_a.executor_route_identity(),
        contract_b.executor_route_identity()
    );

    let request = daemon_query_embedding_request(&contract_b, "private endpoint-B query");
    assert_eq!(
        request["executor_route_identity"],
        contract_b.executor_route_identity()
    );
    let mut stale_embedding = vec![0.0; contract_a.dimensions()];
    stale_embedding[0] = 1.0;
    let stale_response = daemon_embedding_response(&contract_a, json!(stale_embedding));

    let error = parse_daemon_query_embedding_response(&stale_response, &contract_b)
        .expect_err("endpoint B must reject endpoint A's daemon response");
    assert_eq!(
        error.to_string(),
        "daemon query response executor route identity mismatch"
    );
}

#[test]
fn legacy_fixed_http_index_bridge_preserves_builtin_vectors_and_endpoint_fence() {
    let selected = ctx_semantic_model::SemanticEmbeddingExecutorConfig::legacy_fixed_http(
        "http://127.0.0.1:41042",
    )
    .unwrap();
    let bridged = semantic_index_contract_for_selected(selected.contract()).unwrap();

    assert_eq!(
        bridged.fingerprint(),
        ctx_semantic_index::semantic_model_contract().fingerprint()
    );
    assert_eq!(
        bridged.executor_route_identity(),
        selected.contract().executor_route_identity()
    );
    assert_ne!(
        bridged.executor_route_identity(),
        ctx_semantic_index::semantic_model_contract().executor_route_identity()
    );
}

#[test]
fn daemon_query_embedding_response_rejects_malformed_vectors() {
    let contract = semantic_model_contract();
    let malformed = [
        (
            "dimensions",
            json!(vec![0.0; contract.dimensions() - 1]),
            "dimensions, expected",
        ),
        (
            "finiteness",
            {
                let mut vector = embedding().into_iter().map(Value::from).collect::<Vec<_>>();
                vector[0] = json!(f64::MAX);
                Value::Array(vector)
            },
            "contains a non-finite value",
        ),
        (
            "normalization",
            {
                let mut vector = embedding();
                vector[0] = 2.0;
                json!(vector)
            },
            "is not L2-normalized",
        ),
    ];

    for (case, vector, expected) in malformed {
        let response = daemon_embedding_response(contract, vector);
        let error = parse_daemon_query_embedding_response(&response, contract)
            .expect_err("a malformed daemon vector must fail closed");
        assert!(
            error.to_string().contains(expected),
            "{case} error was {error:#}"
        );
    }
}

#[test]
fn foreground_adapter_is_lazy_and_borrows_the_exact_data_root() {
    let data_root = std::path::PathBuf::from("foreground-query-root");
    let adapter =
        SemanticQueryAdapter::foreground(&data_root, SemanticEmbeddingExecutorConfig::builtin());

    assert!(std::ptr::eq(adapter.data_root, data_root.as_path()));
    let SemanticQueryExecution::Foreground { executor, .. } = &adapter.execution else {
        panic!("manual wait must select foreground semantic execution");
    };
    assert!(
        !executor.is_resolved(),
        "constructing the adapter must not build the executor before semantic preflight"
    );
}

#[test]
fn foreground_adapter_uses_the_exact_external_executor_without_config_reread() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(ctx_app_config::CONFIG_FILE),
        "[semantic]\nendpoint = \"this file is intentionally invalid\"\n",
    )
    .unwrap();

    let adapter = SemanticQueryAdapter::foreground(
        temp.path(),
        external_executor_config("http://127.0.0.1:9"),
    );
    let SemanticQueryExecution::Foreground { executor, .. } = &adapter.execution else {
        panic!("manual wait must select foreground semantic execution");
    };
    assert_eq!(executor.config.kind().as_str(), "http");
    assert_eq!(executor.config.endpoint(), Some("http://127.0.0.1:9/"));
    assert_eq!(executor.config.contract().dimensions(), 7);
    assert_eq!(
        executor
            .config
            .contract()
            .prepare_query("raw query".to_owned())
            .into_text(),
        "raw query"
    );
    assert!(!executor.is_resolved());
}

#[test]
fn foreground_empty_generation_converges_without_loading_a_model() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, _) = semantic_index_revision(temp.path(), 1, false)?;
    let adapter =
        SemanticQueryAdapter::foreground(temp.path(), SemanticEmbeddingExecutorConfig::builtin());

    let mut session = adapter
        .begin_query(&index)
        .map_err(|error| anyhow!(error.to_string()))?;
    assert_eq!(
        session.prepare_alternative("empty generation")?,
        compact_json(json!({"query_embed_ms": null}))
    );
    let SemanticQueryExecution::Foreground { executor, .. } = &adapter.execution else {
        unreachable!("foreground constructor selected daemon execution")
    };
    assert!(executor.is_resolved());
    assert!(!executor
        .resolve(temp.path())?
        .builtin_executor()
        .expect("default executor is built in")
        .shared_runtime()
        .is_loaded());
    Ok(())
}

#[test]
fn unavailable_external_contract_preserves_ready_builtin_store() -> Result<()> {
    for include_record in [false, true] {
        let temp = tempfile::tempdir()?;
        let (index, _) = semantic_index_revision(temp.path(), 1, include_record)?;
        let semantic_path = source_backed_semantic_vector_path(temp.path());
        if include_record {
            reconcile_ready_nonempty_generation(&index, temp.path())?;
        } else {
            let mut store = SemanticVectorStore::open(&semantic_path, semantic_model_contract())?;
            acknowledge_empty_generation(&mut store, &index)?;
        }
        SemanticQueryPin::preflight(&index, temp.path(), semantic_model_contract())?;

        let unavailable = TcpListener::bind("127.0.0.1:0")?;
        let endpoint = format!("http://{}", unavailable.local_addr()?);
        drop(unavailable);
        let adapter = SemanticQueryAdapter::foreground(
            temp.path(),
            external_executor_config(endpoint.as_str()),
        );

        let error = adapter
            .begin_query(&index)
            .err()
            .expect("an unavailable external endpoint must fail verification");
        assert!(
            error
                .to_string()
                .contains("semantic embedding HTTP transport failed"),
            "unexpected foreground verification error: {error}"
        );
        SemanticQueryPin::preflight(&index, temp.path(), semantic_model_contract())?;
    }
    Ok(())
}

struct FixtureSemanticEmbedder;

impl SemanticBatchEmbedder for FixtureSemanticEmbedder {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        Ok(chunks.iter().map(|_| embedding()).collect())
    }
}

fn reconcile_ready_nonempty_generation(index: &VerifiedIndex, data_root: &Path) -> Result<()> {
    let contract = semantic_model_contract();
    let mut store =
        SemanticVectorStore::open(&source_backed_semantic_vector_path(data_root), contract)?;
    let mut builder = SourceBackedSemanticDocumentBuilder::new(index);
    let mut embedder = FixtureSemanticEmbedder;
    for _ in 0..32 {
        if store
            .reconcile_source_backed_index(index, &mut builder, &mut embedder)?
            .ready()
        {
            return Ok(());
        }
    }
    Err(anyhow!("nonempty semantic fixture did not converge"))
}

#[test]
fn passive_ready_nonempty_builtin_uses_cache_only_without_acquisition_or_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, _) = semantic_index(temp.path())?;
    reconcile_ready_nonempty_generation(&index, temp.path())?;
    let invalid_cache = temp.path().join("invalid-semantic-cache");
    fs::create_dir_all(&invalid_cache)?;
    fs::write(invalid_cache.join("not-a-model"), "invalid")?;
    let before = durable_query_state_snapshot(temp.path(), &invalid_cache)?;
    let _environment = SemanticEnvironmentGuard::invalid_cache(&invalid_cache);
    let adapter = SemanticQueryAdapter::foreground_read_only(
        temp.path(),
        SemanticEmbeddingExecutorConfig::builtin(),
    );

    let mut session = adapter
        .begin_query(&index)
        .map_err(|error| anyhow!(error.to_string()))?;
    reset_foreground_acquisition_attempts();
    let error = session
        .prepare_alternative("cache-only passive query")
        .expect_err("an absent or invalid cache must fail without acquisition");
    assert!(error.to_string().contains("semantic"));
    assert_eq!(foreground_acquisition_attempts(), 0);
    assert_eq!(
        durable_query_state_snapshot(temp.path(), &invalid_cache)?,
        before
    );
    Ok(())
}

#[test]
fn passive_ready_nonempty_http_uses_the_configured_authenticated_executor_without_mutation(
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, event_id) = semantic_index(temp.path())?;
    reconcile_ready_nonempty_generation(&index, temp.path())?;
    let before =
        durable_query_state_snapshot(temp.path(), &temp.path().join("semantic-model-cache"))?;
    let server = LoopbackEmbeddingServer::start();
    let _environment = SemanticEnvironmentGuard::http_auth(&server.base_url);
    let adapter = SemanticQueryAdapter::foreground_read_only(
        temp.path(),
        SemanticEmbeddingExecutorConfig::http(&server.base_url)?,
    );

    let mut session = adapter
        .begin_query(&index)
        .map_err(|error| anyhow!(error.to_string()))?;
    session.prepare_alternative("exact selected HTTP executor")?;
    let batch = session.candidates(&default_compiled_filter(), 10)?;
    assert_eq!(batch.candidates.len(), 1);
    assert_eq!(batch.candidates[0].event.event_id, event_id);
    assert_eq!(
        durable_query_state_snapshot(temp.path(), &temp.path().join("semantic-model-cache"))?,
        before
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/semantic-base/v1/contract");
    assert!(requests.iter().all(|request| {
        request.header("authorization") == Some("Bearer passive-semantic-test-token")
    }));
    assert_eq!(requests[1].body["input_kind"], "query");
    assert_eq!(requests[2].body["input_kind"], "documents");
    assert_eq!(requests[2].body["inputs"].as_array().unwrap().len(), 1);
    assert!(requests[2].body["inputs"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Restart the background daemon"));
    assert!(!requests[2]
        .body
        .to_string()
        .contains("query adapter fixture"));
    assert_eq!(requests[3].body["input_kind"], "query");
    Ok(())
}

#[test]
fn passive_stale_projection_preflight_makes_no_http_request_or_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (old_index, _) = semantic_index_revision(temp.path(), 1, true)?;
    reconcile_ready_nonempty_generation(&old_index, temp.path())?;
    let (index, _) = semantic_index_revision(temp.path(), 2, true)?;
    let before =
        durable_query_state_snapshot(temp.path(), &temp.path().join("semantic-model-cache"))?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}/semantic-base", listener.local_addr()?);
    let adapter = SemanticQueryAdapter::foreground_read_only(
        temp.path(),
        SemanticEmbeddingExecutorConfig::http(endpoint)?,
    );

    let error = match adapter.begin_query(&index) {
        Ok(_) => panic!("a stale exact-generation projection must fail preflight"),
        Err(error) => error,
    };
    assert!(matches!(
        error.reason(),
        Some(SemanticReason::GenerationNotAcknowledged | SemanticReason::ProjectionEventMismatch)
    ));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "stale preflight must not contact the configured HTTP endpoint"
    );
    assert_eq!(
        durable_query_state_snapshot(temp.path(), &temp.path().join("semantic-model-cache"))?,
        before
    );
    Ok(())
}

#[test]
fn passive_ready_empty_ignores_an_unusable_external_executor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, _) = semantic_index_revision(temp.path(), 1, false)?;
    let contract = semantic_model_contract();
    let mut store =
        SemanticVectorStore::open(&source_backed_semantic_vector_path(temp.path()), contract)?;
    acknowledge_empty_generation(&mut store, &index)?;
    let adapter = SemanticQueryAdapter::foreground_read_only(
        temp.path(),
        SemanticEmbeddingExecutorConfig::http("https://embedding.invalid/ctx")?,
    );

    let mut session = adapter
        .begin_query(&index)
        .map_err(|error| anyhow!(error.to_string()))?;
    assert_eq!(
        session.prepare_alternative("ready empty ignores executor")?,
        compact_json(json!({"query_embed_ms": null}))
    );
    let SemanticQueryExecution::Foreground { executor, .. } = &adapter.execution else {
        unreachable!("ready empty test selected foreground execution")
    };
    assert!(!executor.is_resolved());
    Ok(())
}

#[test]
fn foreground_ready_nonempty_generation_skips_model_and_writable_reconciliation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, _) = semantic_index(temp.path())?;
    reconcile_ready_nonempty_generation(&index, temp.path())?;
    let semantic_path = source_backed_semantic_vector_path(temp.path());
    let transaction_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(semantic_path.join("flat_transaction.lock"))?;
    transaction_lock.lock_exclusive()?;
    let state_path = semantic_path.join("state.sqlite");
    let mut state_permissions = fs::metadata(&state_path)?.permissions();
    state_permissions.set_readonly(true);
    fs::set_permissions(&state_path, state_permissions)?;

    let adapter =
        SemanticQueryAdapter::foreground(temp.path(), SemanticEmbeddingExecutorConfig::builtin());
    let started = Instant::now();
    let _session = adapter
        .begin_query(&index)
        .map_err(|error| anyhow!(error.to_string()))?;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a ready foreground query must not wait on the Flat write transaction lock"
    );
    let SemanticQueryExecution::Foreground { executor, .. } = &adapter.execution else {
        unreachable!("foreground constructor selected daemon execution")
    };
    assert!(
        !executor.is_resolved(),
        "a ready foreground query must not build the selected executor"
    );
    Ok(())
}

#[test]
fn foreground_read_only_ready_generation_skips_writable_reconciliation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, _) = semantic_index(temp.path())?;
    reconcile_ready_nonempty_generation(&index, temp.path())?;
    let semantic_path = source_backed_semantic_vector_path(temp.path());
    let transaction_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(semantic_path.join("flat_transaction.lock"))?;
    transaction_lock.lock_exclusive()?;

    let adapter = SemanticQueryAdapter::foreground_read_only(
        temp.path(),
        SemanticEmbeddingExecutorConfig::builtin(),
    );
    let started = Instant::now();
    let _session = adapter
        .begin_query(&index)
        .map_err(|error| anyhow!(error.to_string()))?;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "refresh off must not wait on the Flat write transaction lock"
    );
    Ok(())
}

#[test]
fn foreground_read_only_missing_store_does_not_reconcile() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, _) = semantic_index(temp.path())?;
    let semantic_path = source_backed_semantic_vector_path(temp.path());
    let adapter = SemanticQueryAdapter::foreground_read_only(
        temp.path(),
        SemanticEmbeddingExecutorConfig::builtin(),
    );

    let error = adapter
        .begin_query(&index)
        .err()
        .expect("refresh off must fail when the semantic projection is absent");
    assert_eq!(error.reason(), Some(SemanticReason::StoreMissing));
    assert!(
        !semantic_path.exists(),
        "refresh off must not create semantic projection state"
    );
    Ok(())
}

fn ready_adapter<'a>(
    index: &'a VerifiedIndex,
    data_root: &'a Path,
    event_id: Uuid,
    vector_root: &Path,
) -> Result<SemanticQuerySession<'a>> {
    let contract = semantic_model_contract();
    let mut store = SemanticVectorStore::open(vector_root, contract)?;
    publish_chunk_replacements(
        &mut store,
        &[(
            semantic_chunk_document(event_id, 1, 0, "1".repeat(64), String::new(), 0, 1),
            embedding(),
        )],
        &[],
    )?;
    let pinned = pinned_flat_generation(&store)?;
    Ok(SemanticQuerySession::from_pin(
        index,
        data_root,
        SemanticQueryPin::from_readiness_for_test(
            index.generation_id(),
            SourceBackedGenerationPin::Ready(pinned),
        )?,
    ))
}

#[test]
fn adapter_never_embeds_before_missing_or_unacknowledged_store_preflight() -> Result<()> {
    for unacknowledged_store in [false, true] {
        let temp = tempfile::tempdir()?;
        let (index, _) = semantic_index(temp.path())?;
        if unacknowledged_store {
            let contract = semantic_model_contract();
            SemanticVectorStore::open(&source_backed_semantic_vector_path(temp.path()), contract)?;
        }
        let error = SemanticQuerySession::begin(&index, temp.path())
            .err()
            .expect("unready semantic state must fail closed");
        assert!(matches!(
            error,
            SemanticQueryError::NotReady {
                code,
                retryable: true,
                ..
            } if code == if unacknowledged_store {
                "semantic_generation_not_acknowledged"
            } else {
                "semantic_store_missing"
            }
        ));
    }
    Ok(())
}

#[test]
fn daemon_generation_wait_observes_delayed_acknowledgement_without_sleeping() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let (index, _) = semantic_index_revision_at(&index_root, 1, true)?;
    let generation = index.generation_id().to_owned();
    let reconciliation_index = VerifiedIndex::open_pinned(&index_root)?;
    let pauses = Cell::new(0_u32);

    let pin = wait_for_daemon_semantic_generation_with(
        temp.path(),
        PinnedSourceBackedGeneration::from_index(index),
        Duration::from_secs(1),
        || crate::pin_active_verified_generation(temp.path()),
        |_| {
            pauses.set(pauses.get() + 1);
            reconcile_ready_nonempty_generation(&reconciliation_index, temp.path()).unwrap();
        },
    )?;

    assert_eq!(pauses.get(), 1);
    assert_eq!(pin.generation_id(), generation);
    SemanticQueryPin::preflight(pin.verified_index(), temp.path(), semantic_model_contract())?;
    Ok(())
}

#[test]
fn daemon_generation_wait_repins_both_indexes_after_core_supersession() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let (first, _) = semantic_index_revision_at(&index_root, 1, true)?;
    let first_generation = first.generation_id().to_owned();
    let (second, _) = semantic_index_revision_at(&index_root, 2, true)?;
    let second_generation = second.generation_id().to_owned();
    reconcile_ready_nonempty_generation(&second, temp.path())?;

    let pin = wait_for_daemon_semantic_generation_with(
        temp.path(),
        PinnedSourceBackedGeneration::from_index(first),
        Duration::from_secs(1),
        || crate::pin_active_verified_generation(temp.path()),
        |_| {},
    )?;

    assert_ne!(first_generation, second_generation);
    assert_eq!(pin.generation_id(), second_generation);
    SemanticQueryPin::preflight(pin.verified_index(), temp.path(), semantic_model_contract())?;
    Ok(())
}

#[test]
fn daemon_generation_wait_repins_before_returning_a_ready_old_generation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let (first, _) = semantic_index_revision_at(&index_root, 1, true)?;
    reconcile_ready_nonempty_generation(&first, temp.path())?;
    let (second, _) = semantic_index_revision_at(&index_root, 2, true)?;
    let second_generation = second.generation_id().to_owned();
    reconcile_ready_nonempty_generation(&second, temp.path())?;

    let pin = wait_for_daemon_semantic_generation_with(
        temp.path(),
        PinnedSourceBackedGeneration::from_index(first),
        Duration::from_secs(1),
        || crate::pin_active_verified_generation(temp.path()),
        |_| {},
    )?;

    assert_eq!(pin.generation_id(), second_generation);
    Ok(())
}

#[test]
fn daemon_generation_wait_retries_a_concurrent_repin_before_preflight() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let (first, _) = semantic_index_revision_at(&index_root, 1, true)?;
    reconcile_ready_nonempty_generation(&first, temp.path())?;
    let (second, _) = semantic_index_revision_at(&index_root, 2, true)?;
    let second_generation = second.generation_id().to_owned();
    reconcile_ready_nonempty_generation(&second, temp.path())?;
    let repins = Cell::new(0_u32);

    let pin = wait_for_daemon_semantic_generation_with(
        temp.path(),
        PinnedSourceBackedGeneration::from_index(first),
        Duration::from_secs(1),
        || {
            repins.set(repins.get() + 1);
            if repins.get() == 1 {
                Err(IndexError::ConcurrentGenerationChange.into())
            } else {
                crate::pin_active_verified_generation(temp.path())
            }
        },
        |_| {},
    )?;

    assert_eq!(repins.get(), 2);
    assert_eq!(pin.generation_id(), second_generation);
    Ok(())
}

#[test]
fn daemon_generation_wait_timeout_preserves_typed_query_preflight_failure() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let (index, _) = semantic_index_revision_at(&index_root, 1, true)?;
    SemanticVectorStore::open(
        &source_backed_semantic_vector_path(temp.path()),
        semantic_model_contract(),
    )?;

    let pin = wait_for_daemon_semantic_generation_with(
        temp.path(),
        PinnedSourceBackedGeneration::from_index(index),
        Duration::ZERO,
        || crate::pin_active_verified_generation(temp.path()),
        |_| panic!("zero timeout must not sleep"),
    )?;
    let error =
        SemanticQueryPin::preflight(pin.verified_index(), temp.path(), semantic_model_contract())
            .err()
            .expect("unacknowledged generation must remain a typed query failure");
    let not_ready = error
        .downcast_ref::<SemanticNotReady>()
        .expect("semantic preflight failure remains typed");
    assert_eq!(not_ready.code(), "semantic_generation_not_acknowledged");
    assert!(not_ready.retryable());
    Ok(())
}

#[test]
fn adapter_never_embeds_before_acknowledged_stale_generation_preflight() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (stale_index, _) = semantic_index_revision(temp.path(), 1, false)?;
    let semantic_path = source_backed_semantic_vector_path(temp.path());
    let contract = semantic_model_contract();
    let mut store = SemanticVectorStore::open(&semantic_path, contract)?;
    acknowledge_empty_generation(&mut store, &stale_index)?;
    assert!(matches!(
        store.source_backed_generation_pin_exact(stale_index.generation_id(), 0)?,
        SourceBackedGenerationPin::ReadyEmpty
    ));
    drop(store);

    let (index, _) = semantic_index_revision(temp.path(), 2, true)?;
    let error = SemanticQuerySession::begin(&index, temp.path())
        .err()
        .expect("an acknowledged stale generation must fail closed");
    assert!(matches!(
        error,
        SemanticQueryError::NotReady {
            code: "semantic_generation_not_acknowledged",
            retryable: true,
            ..
        }
    ));
    Ok(())
}

#[test]
fn adapter_never_embeds_for_mismatched_or_ready_empty_pins() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, _) = semantic_index(temp.path())?;
    for generation in ["different-generation", index.generation_id()] {
        let pin = SemanticQueryPin::from_readiness_for_test(
            generation,
            SourceBackedGenerationPin::ReadyEmpty,
        )?;
        let mut adapter = SemanticQuerySession::from_pin(&index, temp.path(), pin);
        let calls = Cell::new(0_u8);
        let result = adapter.prepare_alternative_with("query", |_, _, _| {
            calls.set(calls.get() + 1);
            Ok(Some((embedding(), 1)))
        });

        if generation == index.generation_id() {
            assert_eq!(result?, compact_json(json!({"query_embed_ms": null})));
            let (candidates, diagnostics) = adapter.search(&default_compiled_filter(), 1)?;
            assert!(candidates.is_empty());
            assert_eq!(
                diagnostics,
                compact_json(json!({
                    "vector_backend": "flat_f32",
                    "core_generation_id": index.generation_id(),
                    "flat_generation": null,
                    "flat_generation_hash": null,
                    "vector_scan_ms": null,
                    "query_vectors": null,
                    "vector_passes": 0,
                    "chunks_scanned": null,
                    "vector_bytes_read": null,
                    "events_scored": null,
                    "dot_products": null,
                    "initial_k": 1,
                    "final_k": 1,
                    "iterations": 0,
                    "raw_candidates": 0,
                    "eligible_candidates": 0,
                    "filtered_candidates": 0,
                    "non_positive_candidates": 0,
                    "metadata_records_loaded": 0,
                    "core_records_decoded": 0,
                    "exhausted": true,
                    "cap_reached": false,
                }))
            );
        } else {
            let error = result.expect_err("a mismatched pin must fail closed");
            assert!(matches!(
                error,
                SemanticQueryError::NotReady {
                    code: "semantic_generation_receipt_mismatch",
                    retryable: true,
                    ..
                }
            ));
        }
        assert_eq!(calls.get(), 0);
    }
    Ok(())
}

#[test]
fn adapter_embeds_ordered_queries_then_runs_one_scan_with_one_filter_projection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, event_id) = semantic_index(temp.path())?;
    let mut adapter = ready_adapter(&index, temp.path(), event_id, &temp.path().join("vectors"))?;
    let calls = Cell::new(0_u8);
    let filters = default_compiled_filter();

    let first_diagnostics =
        adapter.prepare_alternative_with("first normalized query", |_, _, _| {
            calls.set(calls.get() + 1);
            Ok(Some((embedding(), 17)))
        })?;
    assert_eq!(first_diagnostics["query_embed_ms"], 17);
    let second_diagnostics =
        adapter.prepare_alternative_with("second normalized query", |_, _, _| {
            calls.set(calls.get() + 1);
            Ok(Some((embedding(), 17)))
        })?;
    assert_eq!(second_diagnostics["query_embed_ms"], 17);
    assert_eq!(adapter.pin.filter_projection_identity_for_test(), None);
    let (candidates, scan_diagnostics) = adapter.search(&filters, 1)?;
    assert_eq!(candidates.len(), 1);
    assert_eq!(scan_diagnostics["query_vectors"], 2);
    assert_eq!(scan_diagnostics["vector_passes"], 1);
    assert_eq!(calls.get(), 2, "each ready query must embed exactly once");
    assert!(adapter.pin.filter_projection_identity_for_test().is_some());
    Ok(())
}

#[test]
fn adapter_preserves_daemon_query_service_unavailable_contract() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, event_id) = semantic_index(temp.path())?;
    let mut adapter = ready_adapter(&index, temp.path(), event_id, &temp.path().join("vectors"))?;
    let calls = Cell::new(0_u8);

    let error = adapter
        .prepare_alternative_with("query", |_, _, _| {
            calls.set(calls.get() + 1);
            Ok(None)
        })
        .expect_err("a ready pin still requires the daemon embedding service");
    assert_eq!(calls.get(), 1);
    assert!(matches!(
        error,
        SemanticQueryError::NotReady {
            code: "semantic_query_service_unavailable",
            detail,
            retryable: true,
        } if detail == "the daemon query embedding service is unavailable"
    ));
    Ok(())
}

#[test]
fn adapter_scores_only_the_active_flat_core_intersection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, _) = semantic_index(temp.path())?;
    let mut adapter = ready_adapter(
        &index,
        temp.path(),
        Uuid::new_v4(),
        &temp.path().join("vectors"),
    )?;

    adapter.prepare_alternative_with("query", |_, _, _| Ok(Some((embedding(), 1))))?;
    let (candidates, diagnostics) = adapter.search(&default_compiled_filter(), 1)?;

    assert!(candidates.is_empty());
    assert_eq!(diagnostics["events_scored"], 0);
    assert_eq!(diagnostics["filtered_candidates"], 1);
    Ok(())
}

#[test]
fn adapter_downcasts_engine_not_ready_without_parsing_display_text() {
    let error = anyhow::Error::new(SemanticNotReady::new(
        "semantic_projection_event_mismatch",
        "typed engine detail",
    ));
    let classified = SemanticQueryError::from(error);

    assert!(matches!(
        classified,
        SemanticQueryError::NotReady {
            code: "semantic_projection_event_mismatch",
            detail,
            retryable: true,
        } if detail == "typed engine detail"
    ));
}

#[test]
fn adapter_classifies_stale_daemon_endpoint_as_retryable_not_ready() {
    let classified = SemanticQueryError::from(anyhow::Error::new(
        ctx_daemon_service::DaemonQueryServiceUnavailable,
    ));

    assert!(matches!(
        classified,
        SemanticQueryError::NotReady {
            code: "semantic_query_service_unavailable",
            retryable: true,
            ..
        }
    ));
}

#[test]
fn adapter_maps_non_engine_failures_to_failed() {
    let classified = SemanticQueryError::from(anyhow!("transport failed"));
    assert!(matches!(
        classified,
        SemanticQueryError::Failed { detail } if detail == "transport failed"
    ));
}
