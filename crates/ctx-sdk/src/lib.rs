//! Experimental in-repo Rust SDK for ctx agent history.
//!
//! This SDK is intentionally not published. The local backend shells out to the
//! `ctx` CLI and adapts its private JSON into the public `agent-history-v1` envelope.

use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use ctx_protocol::{camel_alias_object, camelize_object_keys, JsonObject};
pub use ctx_protocol::{
    AgentHistoryEnvelope, AgentHistoryErrorBody, AgentHistoryErrorCode, AgentHistoryEvent,
    AgentHistoryOperation, AgentHistoryStatus, BackendInfo, BackendKind, EventResult, Freshness,
    ImportResult, LocationResult, ProviderSource, SearchClause, SearchEffectiveBackend,
    SearchExecutionConsumption, SearchExecutionDiagnostics, SearchExecutionLimits, SearchHit,
    SearchQuery, SearchQueryError, SearchQueryVersion, SearchResult, SearchRetrieval,
    SearchRetrievalCoverage, SearchSemanticCompleteness, SearchSemanticCoverage,
    SearchSemanticDiagnostics, SearchSemanticReadiness, SearchSemanticSkipReason, SessionResult,
    SourceLocation, Totals, CONTRACT_VERSION, SCHEMA_VERSION, SEARCH_MAX_RESULTS,
    SEARCH_MAX_SERIALIZED_RESPONSE_BYTES, SEARCH_QUERY_VERSION,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use thiserror::Error;

/// The nested `ctx search --json` response schema supported by this SDK.
pub const SEARCH_SCHEMA_VERSION: u16 = 2;

#[derive(Debug, Error)]
#[error("{body:?}")]
pub struct AgentHistoryError {
    pub body: AgentHistoryErrorBody,
}

impl AgentHistoryError {
    fn new(code: AgentHistoryErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            body: AgentHistoryErrorBody::new(code, message, retryable),
        }
    }

    fn with_cause(mut self, cause: impl Into<String>) -> Self {
        self.body.cause = Some(cause.into());
        self
    }
}

#[derive(Debug, Clone)]
pub enum AgentHistoryBackend {
    Local(LocalBackendConfig),
    Hosted(HostedBackendConfig),
}

#[derive(Debug, Clone)]
pub struct LocalBackendConfig {
    pub ctx_binary: PathBuf,
    pub data_root: Option<PathBuf>,
    pub timeout: Duration,
}

impl Default for LocalBackendConfig {
    fn default() -> Self {
        Self {
            ctx_binary: PathBuf::from("ctx"),
            data_root: None,
            timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostedBackendConfig {
    pub base_url: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    pub catalog_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    pub provider: Option<String>,
    pub path: Option<PathBuf>,
    pub all: bool,
    pub resume: bool,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub query: Option<SearchQuery>,
    pub limit: usize,
    pub backend: Option<String>,
    pub provider: Option<String>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub workspace: Option<String>,
    pub since: Option<String>,
    pub include_subagents: bool,
    pub event_type: Option<String>,
    pub file: Option<PathBuf>,
    pub session: Option<String>,
    pub events: bool,
    pub refresh: SearchRefresh,
    pub include_current_session: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            query: None,
            limit: 20,
            backend: None,
            provider: None,
            history_source: None,
            provider_key: None,
            source_id: None,
            source_format: None,
            workspace: None,
            since: None,
            include_subagents: false,
            event_type: None,
            file: None,
            session: None,
            events: false,
            refresh: SearchRefresh::Background,
            include_current_session: false,
        }
    }
}

impl SearchOptions {
    fn has_intent(&self) -> bool {
        self.query.is_some()
            || self
                .file
                .as_ref()
                .map(|path| !path.to_string_lossy().trim().is_empty())
                .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchRefresh {
    Background,
    Off,
    Wait,
}

impl SearchRefresh {
    fn as_arg(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Off => "off",
            Self::Wait => "wait",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ShowEventOptions {
    pub before: usize,
    pub after: usize,
    pub window: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ShowSessionOptions {
    pub mode: String,
}

impl Default for ShowSessionOptions {
    fn default() -> Self {
        Self {
            mode: "lite".to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentHistoryClient {
    backend: AgentHistoryBackend,
}

impl AgentHistoryClient {
    pub fn local(config: LocalBackendConfig) -> Self {
        Self {
            backend: AgentHistoryBackend::Local(config),
        }
    }

    pub fn hosted(config: HostedBackendConfig) -> Self {
        Self {
            backend: AgentHistoryBackend::Hosted(config),
        }
    }

    pub fn backend_info(&self) -> BackendInfo {
        match &self.backend {
            AgentHistoryBackend::Local(config) => BackendInfo::local(
                config
                    .data_root
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
            ),
            AgentHistoryBackend::Hosted(config) => {
                BackendInfo::hosted(Some(config.base_url.clone()))
            }
        }
    }

    pub fn status(&self) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json(AgentHistoryOperation::Status, &["status", "--json"])
    }

    pub fn init(&self, options: InitOptions) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        let mut args = vec!["setup", "--json", "--progress", "none"];
        if options.catalog_only {
            args.push("--catalog-only");
        }
        self.local_json(AgentHistoryOperation::Init, &args)
    }

    pub fn sources(&self) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json(AgentHistoryOperation::Sources, &["sources", "--json"])
    }

    pub fn import_history(
        &self,
        options: ImportOptions,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.import_or_sync(AgentHistoryOperation::Import, options)
    }

    pub fn sync(&self, options: ImportOptions) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.import_or_sync(AgentHistoryOperation::Sync, options)
    }

    pub fn search(
        &self,
        options: SearchOptions,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        if !options.has_intent() {
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::InvalidRequest,
                "search requires a ctx-search-v1 query or file option",
                false,
            ));
        }
        if !(1..=SEARCH_MAX_RESULTS).contains(&options.limit) {
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::InvalidRequest,
                format!("search limit must be between 1 and {SEARCH_MAX_RESULTS}"),
                false,
            ));
        }
        let mut owned = Vec::<String>::new();
        owned.push("search".to_owned());
        if let Some(query) = options.query {
            owned.push("--query-json".to_owned());
            owned.push(serialize_search_query(&query)?);
        }
        owned.extend(["--limit".to_owned(), options.limit.to_string()]);
        push_opt(&mut owned, "--backend", options.backend);
        push_opt(&mut owned, "--provider", options.provider);
        push_opt(&mut owned, "--history-source", options.history_source);
        push_opt(&mut owned, "--provider-key", options.provider_key);
        push_opt(&mut owned, "--source-id", options.source_id);
        push_opt(&mut owned, "--source-format", options.source_format);
        push_opt(&mut owned, "--workspace", options.workspace);
        push_opt(&mut owned, "--since", options.since);
        if options.include_subagents {
            owned.push("--include-subagents".to_owned());
        }
        push_opt(&mut owned, "--event-type", options.event_type);
        if let Some(file) = options.file {
            push_opt(
                &mut owned,
                "--file",
                Some(file.to_string_lossy().into_owned()),
            );
        }
        push_opt(&mut owned, "--session", options.session);
        if options.events {
            owned.push("--events".to_owned());
        }
        owned.extend(["--refresh".to_owned(), options.refresh.as_arg().to_owned()]);
        if options.include_current_session {
            owned.push("--include-current-session".to_owned());
        }
        owned.push("--json".to_owned());
        self.local_json_owned(AgentHistoryOperation::Search, owned)
    }

    pub fn show_event(
        &self,
        id: impl AsRef<str>,
        options: ShowEventOptions,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        let mut owned = vec![
            "show".to_owned(),
            "event".to_owned(),
            id.as_ref().to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ];
        if options.before > 0 {
            owned.extend(["--before".to_owned(), options.before.to_string()]);
        }
        if options.after > 0 {
            owned.extend(["--after".to_owned(), options.after.to_string()]);
        }
        if let Some(window) = options.window {
            owned.extend(["--window".to_owned(), window.to_string()]);
        }
        self.local_json_owned(AgentHistoryOperation::ShowEvent, owned)
    }

    pub fn show_session(
        &self,
        id: impl AsRef<str>,
        options: ShowSessionOptions,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json_owned(
            AgentHistoryOperation::ShowSession,
            vec![
                "show".to_owned(),
                "session".to_owned(),
                id.as_ref().to_owned(),
                "--mode".to_owned(),
                options.mode,
                "--format".to_owned(),
                "json".to_owned(),
            ],
        )
    }

    pub fn locate_event(
        &self,
        id: impl AsRef<str>,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json_owned(
            AgentHistoryOperation::LocateEvent,
            vec![
                "locate".to_owned(),
                "event".to_owned(),
                id.as_ref().to_owned(),
                "--format".to_owned(),
                "json".to_owned(),
            ],
        )
    }

    pub fn locate_session(
        &self,
        id: impl AsRef<str>,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json_owned(
            AgentHistoryOperation::LocateSession,
            vec![
                "locate".to_owned(),
                "session".to_owned(),
                id.as_ref().to_owned(),
                "--format".to_owned(),
                "json".to_owned(),
            ],
        )
    }

    fn import_or_sync(
        &self,
        operation: AgentHistoryOperation,
        options: ImportOptions,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        let mut owned = vec![
            "import".to_owned(),
            "--json".to_owned(),
            "--progress".to_owned(),
            "none".to_owned(),
        ];
        push_opt(&mut owned, "--provider", options.provider);
        if let Some(path) = options.path {
            push_opt(
                &mut owned,
                "--path",
                Some(path.to_string_lossy().into_owned()),
            );
        }
        if options.all {
            owned.push("--all".to_owned());
        }
        if options.resume {
            owned.push("--resume".to_owned());
        }
        self.local_json_owned(operation, owned)
    }

    fn local_json(
        &self,
        operation: AgentHistoryOperation,
        args: &[&str],
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json_owned(
            operation,
            args.iter().map(|arg| (*arg).to_owned()).collect(),
        )
    }

    fn local_json_owned(
        &self,
        operation: AgentHistoryOperation,
        args: Vec<String>,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        let config = match &self.backend {
            AgentHistoryBackend::Local(config) => config,
            AgentHistoryBackend::Hosted(config) => {
                let mut details = JsonObject::new();
                details.insert("backend".to_owned(), json!("hosted"));
                return Err(AgentHistoryError {
                    body: AgentHistoryErrorBody {
                        details: Some(details),
                        ..AgentHistoryErrorBody::new(
                            AgentHistoryErrorCode::NotSupported,
                            "hosted ctx agent history backend is not available in this in-repo SDK",
                            false,
                        )
                    },
                }
                .with_cause(config.base_url.clone()));
            }
        };

        let raw = run_ctx_json(config, &args)?;
        normalize(operation, self.backend_info(), raw)
    }
}

fn push_opt(args: &mut Vec<String>, name: &str, value: Option<String>) {
    if let Some(value) = value {
        args.push(name.to_owned());
        args.push(value);
    }
}

/// Validate, canonicalize, and serialize one `ctx-search-v1` query for `--query-json`.
pub fn serialize_search_query(query: &SearchQuery) -> Result<String, AgentHistoryError> {
    let canonical = query.clone().canonicalized().map_err(|error| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::InvalidRequest,
            "invalid ctx-search-v1 query",
            false,
        )
        .with_cause(error.to_string())
    })?;
    let bytes = serde_json::to_vec(&canonical).map_err(|error| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::InvalidRequest,
            "failed to encode ctx-search-v1 query",
            false,
        )
        .with_cause(error.to_string())
    })?;
    SearchQuery::from_json_slice(&bytes).map_err(|error| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::InvalidRequest,
            "invalid serialized ctx-search-v1 query",
            false,
        )
        .with_cause(error.to_string())
    })?;
    String::from_utf8(bytes).map_err(|error| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::InvalidRequest,
            "ctx-search-v1 query was not UTF-8",
            false,
        )
        .with_cause(error.to_string())
    })
}

fn run_ctx_json(config: &LocalBackendConfig, args: &[String]) -> Result<Value, AgentHistoryError> {
    let mut command = Command::new(&config.ctx_binary);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(data_root) = &config.data_root {
        command.env("CTX_DATA_ROOT", data_root);
    }
    let mut child = command.spawn().map_err(|err| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::BackendUnavailable,
            "failed to start ctx CLI",
            true,
        )
        .with_cause(err.to_string())
    })?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "ctx CLI stdout pipe was not available",
                true,
            ));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "ctx CLI stderr pipe was not available",
                true,
            ));
        }
    };
    let stdout_reader =
        thread::spawn(move || drain_bounded(stdout, SEARCH_MAX_SERIALIZED_RESPONSE_BYTES));
    let stderr_reader =
        thread::spawn(move || drain_bounded(stderr, SEARCH_MAX_SERIALIZED_RESPONSE_BYTES));
    let started = Instant::now();
    let status = loop {
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AgentHistoryError::new(
                    AgentHistoryErrorCode::AdapterError,
                    "failed to wait for ctx CLI",
                    true,
                )
                .with_cause(error.to_string()));
            }
        };
        if let Some(status) = status {
            break status;
        }
        if started.elapsed() > config.timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::Timeout,
                "ctx CLI command timed out",
                true,
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = join_output_reader(stdout_reader, "stdout")?;
    let stderr = join_output_reader(stderr_reader, "stderr")?;
    if !status.success() {
        let mut message = String::from_utf8_lossy(&stderr.bytes).trim().to_owned();
        if stderr.exceeded {
            message.push_str(" [stderr truncated at response cap]");
        }
        return Err(AgentHistoryError::new(
            classify_stderr(&message),
            message,
            false,
        ));
    }
    if stdout.exceeded {
        return Err(AgentHistoryError::new(
            AgentHistoryErrorCode::DecodeError,
            format!(
                "ctx CLI JSON exceeds the {SEARCH_MAX_SERIALIZED_RESPONSE_BYTES}-byte response cap"
            ),
            false,
        ));
    }
    serde_json::from_slice(&stdout.bytes).map_err(|err| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::DecodeError,
            "failed to decode ctx JSON",
            false,
        )
        .with_cause(err.to_string())
    })
}

struct BoundedOutput {
    bytes: Vec<u8>,
    exceeded: bool,
}

fn drain_bounded(mut reader: impl Read, cap: usize) -> io::Result<BoundedOutput> {
    let mut bytes = Vec::with_capacity(cap.min(64 * 1024));
    let mut exceeded = false;
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let retained = cap.saturating_sub(bytes.len()).min(read);
        bytes.extend_from_slice(&chunk[..retained]);
        exceeded |= retained < read;
    }
    Ok(BoundedOutput { bytes, exceeded })
}

fn join_output_reader(
    reader: thread::JoinHandle<io::Result<BoundedOutput>>,
    stream: &str,
) -> Result<BoundedOutput, AgentHistoryError> {
    reader
        .join()
        .map_err(|_| {
            AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                format!("ctx CLI {stream} reader panicked"),
                true,
            )
        })?
        .map_err(|error| {
            AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                format!("failed to read ctx CLI {stream}"),
                true,
            )
            .with_cause(error.to_string())
        })
}

fn classify_stderr(stderr: &str) -> AgentHistoryErrorCode {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("not found") || lower.contains("no such") {
        AgentHistoryErrorCode::NotFound
    } else if lower.contains("not initialized") || lower.contains("setup") {
        AgentHistoryErrorCode::NotInitialized
    } else {
        AgentHistoryErrorCode::AdapterError
    }
}

fn normalize(
    operation: AgentHistoryOperation,
    backend: BackendInfo,
    raw: Value,
) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
    let mut envelope = AgentHistoryEnvelope::new(operation.clone(), Some(backend));
    match operation {
        AgentHistoryOperation::Status => envelope.status = Some(normalize_status(&raw)?),
        AgentHistoryOperation::Init => envelope.status = Some(normalize_status(&raw)?),
        AgentHistoryOperation::Sources => {
            envelope.sources = Some(decode_payload(
                camelize_object_keys(&raw.get("sources").cloned().unwrap_or_else(|| json!([]))),
                "sources",
            )?)
        }
        AgentHistoryOperation::Import | AgentHistoryOperation::Sync => {
            envelope.import_result = Some(normalize_import(&raw)?)
        }
        AgentHistoryOperation::Search => envelope.search = Some(normalize_search(&raw)?),
        AgentHistoryOperation::ShowEvent => envelope.event = Some(normalize_event(&raw)?),
        AgentHistoryOperation::ShowSession => envelope.session = Some(normalize_session(&raw)?),
        AgentHistoryOperation::LocateEvent | AgentHistoryOperation::LocateSession => {
            envelope.location = Some(normalize_location(&raw)?)
        }
        AgentHistoryOperation::Error => {}
    }
    Ok(envelope)
}

fn decode_payload<T: DeserializeOwned>(
    value: Value,
    payload: &str,
) -> Result<T, AgentHistoryError> {
    serde_json::from_value(value).map_err(|err| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::DecodeError,
            format!("failed to decode agent-history-v1 {payload} payload"),
            false,
        )
        .with_cause(err.to_string())
    })
}

fn normalize_status(raw: &Value) -> Result<AgentHistoryStatus, AgentHistoryError> {
    let mut value = camel_alias_object(
        raw,
        &[
            ("schema_version", "schemaVersion"),
            ("data_root", "dataRoot"),
            ("indexed_items", "indexedItems"),
            ("indexed_sources", "indexedSources"),
            ("cataloged_sessions", "catalogedSessions"),
            ("indexed_catalog_sessions", "indexedCatalogSessions"),
            ("pending_catalog_sessions", "pendingCatalogSessions"),
            ("failed_catalog_sessions", "failedCatalogSessions"),
            ("stale_catalog_sessions", "staleCatalogSessions"),
            ("local_only", "localOnly"),
        ],
    );
    if let Some(object) = value.as_object_mut() {
        if !object.contains_key("initialized") {
            let initialized = object
                .get("mode")
                .and_then(Value::as_str)
                .map(|mode| matches!(mode, "ready" | "catalog_only"))
                .unwrap_or(true);
            object.insert("initialized".to_owned(), Value::Bool(initialized));
        }
        if !object.contains_key("localOnly") {
            object.insert("localOnly".to_owned(), Value::Bool(true));
        }
    }
    decode_payload(camelize_object_keys(&value), "status")
}

fn normalize_import(raw: &Value) -> Result<ImportResult, AgentHistoryError> {
    let value = camel_alias_object(raw, &[("resume_mode", "resumeMode")]);
    decode_payload(camelize_object_keys(&value), "import")
}

fn normalize_search(raw: &Value) -> Result<SearchResult, AgentHistoryError> {
    let schema_version = raw.get("schema_version").and_then(Value::as_u64);
    if schema_version != Some(u64::from(SEARCH_SCHEMA_VERSION)) {
        return Err(AgentHistoryError::new(
            AgentHistoryErrorCode::DecodeError,
            format!(
                "ctx search returned unsupported schema version {schema_version:?}; expected {SEARCH_SCHEMA_VERSION}"
            ),
            false,
        ));
    }

    let query = match raw.get("query") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            serde_json::from_value::<SearchQuery>(value.clone())
                .map_err(|error| {
                    AgentHistoryError::new(
                        AgentHistoryErrorCode::DecodeError,
                        "ctx search returned an invalid canonical query",
                        false,
                    )
                    .with_cause(error.to_string())
                })?
                .canonicalized()
                .map_err(|error| {
                    AgentHistoryError::new(
                        AgentHistoryErrorCode::DecodeError,
                        "ctx search returned an invalid canonical query",
                        false,
                    )
                    .with_cause(error.to_string())
                })?,
        ),
    };
    let query_execution = raw
        .get("query_execution")
        .filter(|value| value.is_object())
        .ok_or_else(|| {
            AgentHistoryError::new(
                AgentHistoryErrorCode::DecodeError,
                "ctx search response is missing query_execution diagnostics",
                false,
            )
        })
        .and_then(|value| {
            serde_json::from_value::<SearchExecutionDiagnostics>(value.clone()).map_err(|error| {
                AgentHistoryError::new(
                    AgentHistoryErrorCode::DecodeError,
                    "ctx search returned invalid query_execution diagnostics",
                    false,
                )
                .with_cause(error.to_string())
            })
        })?;

    let mut public_raw = raw.clone();
    if let Some(object) = public_raw.as_object_mut() {
        object.remove("schema_version");
        object.remove("query");
        object.remove("query_execution");
    }
    let value = camel_alias_object(&public_raw, &[("generated_at", "generatedAt")]);
    let mut result: SearchResult = decode_payload(camelize_object_keys(&value), "search")?;
    result.query = query;
    result.query_execution = query_execution;
    result.extra.insert(
        "schema_version".to_owned(),
        Value::from(SEARCH_SCHEMA_VERSION),
    );
    Ok(result)
}

fn normalize_event(raw: &Value) -> Result<EventResult, AgentHistoryError> {
    let value = json!({
        "event": raw.get("event").cloned(),
        "events": raw.get("events").cloned().unwrap_or_else(|| json!([])),
        "source": raw.get("source").cloned()
    });
    decode_payload(camelize_object_keys(&value), "event")
}

fn normalize_session(raw: &Value) -> Result<SessionResult, AgentHistoryError> {
    let value = json!({
        "session": raw.get("session").cloned(),
        "events": raw.get("events").cloned().unwrap_or_else(|| json!([])),
        "source": raw.get("source").cloned(),
        "mode": raw.get("mode").cloned(),
        "format": raw.get("format").cloned()
    });
    decode_payload(camelize_object_keys(&value), "session")
}

fn normalize_location(raw: &Value) -> Result<LocationResult, AgentHistoryError> {
    let value = camel_alias_object(
        raw,
        &[
            ("ctx_session_id", "ctxSessionId"),
            ("ctx_event_id", "ctxEventId"),
            ("provider_session_id", "providerSessionId"),
        ],
    );
    decode_payload(camelize_object_keys(&value), "location")
}

pub fn fixture_path(name: impl AsRef<Path>) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/agent-history-v1/fixtures")
        .join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn all_query(value: &str) -> SearchQuery {
        SearchQuery::new(vec![SearchClause::all(value)])
    }

    fn schema_v2_search(query: Option<SearchQuery>, results: Value) -> Value {
        json!({
            "schema_version": SEARCH_SCHEMA_VERSION,
            "query": query,
            "query_execution": SearchExecutionDiagnostics::default(),
            "results": results,
        })
    }

    #[test]
    fn reads_shared_search_fixture() {
        let value: AgentHistoryEnvelope = serde_json::from_str(include_str!(
            "../../../contracts/agent-history-v1/fixtures/search.results.json"
        ))
        .unwrap();
        assert_eq!(value.contract_version, CONTRACT_VERSION);
        assert_eq!(value.operation, AgentHistoryOperation::Search);
        let search = value.search.unwrap();
        assert_eq!(
            search.query.as_ref().and_then(SearchQuery::single_all_text),
            None,
            "the shared fixture intentionally combines lexical and semantic alternatives"
        );
        assert_eq!(
            search.query.as_ref().unwrap().any[0],
            SearchClause::all("local agent history")
        );
        assert_eq!(search.results.len(), 1);
        assert_eq!(
            search.results[0].ctx_event_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
    }

    #[test]
    fn init_normalizes_real_setup_json_into_status_contract() {
        let envelope = normalize(
            AgentHistoryOperation::Init,
            BackendInfo::local(Some("/tmp/ctx".to_owned())),
            json!({
                "schema_version": 1,
                "data_root": "/tmp/ctx",
                "database_path": "/tmp/ctx/history.sqlite3",
                "config_path": "/tmp/ctx/config.toml",
                "mode": "ready",
                "indexed_items": 12,
                "network_required": false,
                "catalog": {"cataloged_sessions": 4},
                "import": {"resume": false, "totals": {}}
            }),
        )
        .unwrap();

        assert_eq!(envelope.operation, AgentHistoryOperation::Init);
        let status = envelope.status.unwrap();
        assert!(status.initialized);
        assert!(status.local_only);
        assert_eq!(status.data_root.as_deref(), Some("/tmp/ctx"));
        assert_eq!(status.indexed_items, Some(12));
        assert!(status.extra.contains_key("mode"));
        assert!(status.extra.contains_key("networkRequired"));
    }

    #[test]
    fn hosted_backend_returns_structured_error() {
        let client = AgentHistoryClient::hosted(HostedBackendConfig {
            base_url: "https://ctx.example.invalid".to_owned(),
            timeout: Duration::from_secs(1),
        });
        let err = client.status().unwrap_err();
        assert_eq!(err.body.code, AgentHistoryErrorCode::NotSupported);
        assert!(!err.body.retryable);
    }

    #[test]
    fn builds_search_cli_arguments_without_running_for_public_options() {
        let options = SearchOptions {
            query: Some(SearchQuery::new(vec![
                SearchClause::all("agent history"),
                SearchClause::semantic("ctx"),
            ])),
            limit: 3,
            backend: Some("hybrid".to_owned()),
            provider: Some("codex".to_owned()),
            refresh: SearchRefresh::Off,
            events: true,
            ..SearchOptions::default()
        };
        assert_eq!(options.refresh.as_arg(), "off");
        assert_eq!(options.backend.as_deref(), Some("hybrid"));
        assert!(SearchOptions::default().backend.is_none());
        assert_eq!(
            serialize_search_query(options.query.as_ref().unwrap()).unwrap(),
            r#"{"version":"ctx-search-v1","any":[{"all":"agent history"},{"semantic":"ctx"}]}"#
        );
    }

    #[test]
    fn search_options_map_retrieval_controls_to_cli_flags() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("ctx-fake");
        let response = schema_v2_search(Some(all_query("agent history")), json!([])).to_string();
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
set -eu
printf '%s\n' "$@" > "$CTX_DATA_ROOT/argv.txt"
if [ "$1" = "search" ]; then
  printf '%s\n' '{response}'
  exit 0
fi
echo "unexpected command: $*" >&2
exit 2
"#
            ),
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let client = AgentHistoryClient::local(LocalBackendConfig {
            ctx_binary: script,
            data_root: Some(temp.path().to_path_buf()),
            timeout: Duration::from_secs(5),
        });

        client
            .search(SearchOptions {
                query: Some(all_query("agent history")),
                limit: 7,
                backend: Some("hybrid".to_owned()),
                provider: Some("custom".to_owned()),
                history_source: Some("hermes/default".to_owned()),
                provider_key: Some("hermes".to_owned()),
                source_id: Some("default".to_owned()),
                source_format: Some("hermes-history-v1".to_owned()),
                include_subagents: true,
                event_type: Some("message".to_owned()),
                refresh: SearchRefresh::Off,
                ..SearchOptions::default()
            })
            .unwrap();

        let argv = fs::read_to_string(temp.path().join("argv.txt")).unwrap();
        let argv = argv.lines().collect::<Vec<_>>();
        assert_eq!(
            argv,
            vec![
                "search",
                "--query-json",
                r#"{"version":"ctx-search-v1","any":[{"all":"agent history"}]}"#,
                "--limit",
                "7",
                "--backend",
                "hybrid",
                "--provider",
                "custom",
                "--history-source",
                "hermes/default",
                "--provider-key",
                "hermes",
                "--source-id",
                "default",
                "--source-format",
                "hermes-history-v1",
                "--include-subagents",
                "--event-type",
                "message",
                "--refresh",
                "off",
                "--json",
            ]
        );
    }

    #[test]
    fn search_normalization_omits_obsolete_retrieval_fields() {
        let envelope = normalize(
            AgentHistoryOperation::Search,
            BackendInfo::local(None),
            json!({
                "schema_version": SEARCH_SCHEMA_VERSION,
                "query": all_query("semantic defaults"),
                "query_execution": SearchExecutionDiagnostics::default(),
                "generated_at": "2026-07-05T00:00:00Z",
                "retrieval": {
                    "requested_mode": "hybrid",
                    "effective_mode": "lexical",
                    "semantic_weight": 0.0,
                    "semantic_fallback_code": "semantic_retrieval_failed",
                    "semantic_fallback": "semantic_retrieval_failed",
                    "coverage": {"embedded_items": 4, "indexed_now": 1},
                    "diagnostics": {"query_embed_ms": 2}
                },
                "results": [{
                    "ctx_event_id": "event-1",
                    "ctx_session_id": "session-1",
                    "result_scope": "event",
                    "snippet": "semantic match",
                }],
            }),
        )
        .unwrap();

        let search = envelope.search.unwrap();
        assert_eq!(
            search.extra.get("schema_version"),
            Some(&json!(SEARCH_SCHEMA_VERSION))
        );
        assert_eq!(search.query_execution.query_version, SEARCH_QUERY_VERSION);
        let retrieval = search.retrieval.unwrap();
        assert_eq!(retrieval.requested_mode.as_deref(), Some("hybrid"));
        assert_eq!(retrieval.effective_mode.as_deref(), Some("lexical"));
        assert!(!retrieval.extra.contains_key("semanticWeight"));
        assert!(!retrieval.extra.contains_key("semanticFallbackCode"));
        assert!(!retrieval.extra.contains_key("semanticFallback"));
        assert_eq!(retrieval.coverage.as_ref().unwrap().embedded_items, Some(4));
        assert_eq!(
            retrieval.diagnostics.as_ref().unwrap().get("queryEmbedMs"),
            Some(&json!(2))
        );
        assert!(
            !search.extra.contains_key("retrieval"),
            "top-level retrieval should be typed, not left in extra"
        );
        assert_eq!(
            search.results[0].extra.get("retrieval"),
            None,
            "per-hit retrieval is not part of the canonical SDK search hit shape"
        );
    }

    #[test]
    fn search_requires_query_or_file_before_cli() {
        let client = AgentHistoryClient::local(LocalBackendConfig {
            ctx_binary: PathBuf::from("/definitely/missing/ctx"),
            data_root: None,
            timeout: Duration::from_secs(1),
        });

        for options in [
            SearchOptions::default(),
            SearchOptions {
                refresh: SearchRefresh::Off,
                ..SearchOptions::default()
            },
        ] {
            let err = client.search(options).unwrap_err();
            assert_eq!(err.body.code, AgentHistoryErrorCode::InvalidRequest);
        }

        let err = client
            .search(SearchOptions {
                query: Some(all_query("   ")),
                ..SearchOptions::default()
            })
            .unwrap_err();
        assert_eq!(err.body.code, AgentHistoryErrorCode::InvalidRequest);
    }

    #[test]
    fn search_rejects_limits_outside_public_range_before_cli() {
        let client = AgentHistoryClient::local(LocalBackendConfig {
            ctx_binary: PathBuf::from("/definitely/missing/ctx"),
            data_root: None,
            timeout: Duration::from_secs(1),
        });
        for limit in [0, SEARCH_MAX_RESULTS + 1] {
            let err = client
                .search(SearchOptions {
                    query: Some(all_query("bounded limit")),
                    limit,
                    ..SearchOptions::default()
                })
                .unwrap_err();
            assert_eq!(err.body.code, AgentHistoryErrorCode::InvalidRequest);
        }
    }

    #[test]
    fn structured_query_validation_is_closed_and_canonical() {
        let query = SearchQuery::from_json_slice(
            br#"{"version":"ctx-search-v1","any":[{"all":"  disk   pressure "},{"all":"disk pressure"}],"must_not":[{"literal":" logs_2.db "}]}"#,
        )
        .unwrap();
        assert_eq!(
            serialize_search_query(&query).unwrap(),
            r#"{"version":"ctx-search-v1","any":[{"all":"disk pressure"}],"must_not":[{"literal":"logs_2.db"}]}"#
        );
        for raw in [
            br#"{"version":"ctx-search-v1","unknown":true,"any":[{"all":"ctx"}]}"#.as_slice(),
            br#"{"version":"ctx-search-v1","any":[{"all":"ctx","phrase":"ctx"}]}"#.as_slice(),
            br#"{"version":"ctx-search-v1","must":[{"semantic":"ctx"}]}"#.as_slice(),
        ] {
            assert!(SearchQuery::from_json_slice(raw).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn local_cli_drains_stderr_while_waiting_for_bounded_json() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("ctx-noisy");
        fs::write(
            &script,
            r#"#!/bin/sh
set -eu
i=0
while [ "$i" -lt 12000 ]; do
  printf 'bounded stderr line %08d ................................\n' "$i" >&2
  i=$((i + 1))
done
printf '{"ok":true}\n'
"#,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let value = run_ctx_json(
            &LocalBackendConfig {
                ctx_binary: script,
                data_root: None,
                timeout: Duration::from_secs(5),
            },
            &[],
        )
        .unwrap();
        assert_eq!(value["ok"], true);
    }

    #[test]
    fn query_execution_wire_keys_remain_snake_case() {
        let value = serde_json::to_value(SearchExecutionDiagnostics::default()).unwrap();
        let object = value.as_object().unwrap();
        assert!(object.contains_key("query_version"));
        assert!(object.contains_key("candidate_strategy"));
        assert!(!object.contains_key("queryVersion"));
        let semantic = object.get("semantic").and_then(Value::as_object).unwrap();
        assert!(semantic.contains_key("effective_backend"));
        assert!(semantic.contains_key("positive_text_rule_version"));
        assert!(!semantic.contains_key("effectiveBackend"));
    }

    #[test]
    fn search_rejects_legacy_or_incomplete_response_shape() {
        let backend = BackendInfo::local(None);
        for raw in [
            json!({"schema_version": 1, "query": "ctx", "results": []}),
            json!({"schema_version": 2, "query": "ctx", "query_execution": {}, "results": []}),
            json!({"schema_version": 2, "query": all_query("ctx"), "results": []}),
        ] {
            let error = normalize(AgentHistoryOperation::Search, backend.clone(), raw).unwrap_err();
            assert_eq!(error.body.code, AgentHistoryErrorCode::DecodeError);
        }
    }

    #[test]
    fn local_client_can_dogfood_fake_ctx_without_private_history() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("ctx-fake");
        let response = schema_v2_search(
            Some(all_query("rust sdk")),
            json!([{
                "ctx_event_id": "event-1",
                "ctx_session_id": "session-1",
                "result_scope": "event",
                "snippet": "typed ergonomics",
            }]),
        )
        .to_string();
        fs::write(
            &script,
            format!(r#"#!/bin/sh
set -eu
if [ "$1" = "status" ]; then
  printf '%s\n' '{{"initialized":true,"local_only":true,"data_root":"'"$CTX_DATA_ROOT"'","indexed_items":2}}'
  exit 0
fi
if [ "$1" = "search" ]; then
  printf '%s\n' '{response}'
  exit 0
fi
echo "unexpected command: $*" >&2
exit 2
"#),
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let data_root = temp.path().join("data-root");
        let client = AgentHistoryClient::local(LocalBackendConfig {
            ctx_binary: script,
            data_root: Some(data_root.clone()),
            timeout: Duration::from_secs(5),
        });

        let status = client.status().unwrap();
        let status_body = status.status.unwrap();
        assert!(status_body.initialized);
        assert!(status_body.local_only);
        assert_eq!(
            status_body.data_root.as_deref(),
            Some(data_root.to_string_lossy().as_ref())
        );
        assert_eq!(status_body.indexed_items, Some(2));

        let search = client
            .search(SearchOptions {
                query: Some(all_query("rust sdk")),
                refresh: SearchRefresh::Off,
                limit: 1,
                ..SearchOptions::default()
            })
            .unwrap();
        let search_body = search.search.unwrap();
        assert_eq!(search_body.results.len(), 1);
        assert_eq!(search_body.results[0].result_scope, "event");
        assert_eq!(
            search_body.results[0].snippet.as_deref(),
            Some("typed ergonomics")
        );
    }
}
