//! Experimental in-repo Rust SDK for ctx agent history.
//!
//! This SDK is intentionally not published. The local backend shells out to the
//! `ctx` CLI and adapts its private JSON into the public `agent-history-v1` envelope.

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use ctx_protocol::{camel_alias_object, camelize_object_keys, JsonObject};
pub use ctx_protocol::{
    AgentHistoryEnvelope, AgentHistoryErrorBody, AgentHistoryErrorCode, AgentHistoryEvent,
    AgentHistoryOperation, AgentHistoryStatus, BackendInfo, BackendKind, CoreContentMetadata,
    CoreContentPolicyStatus, EventResult, Freshness, ImportResult, ProviderSource, SearchHit,
    SearchResult, SearchResultWindow, SearchRetrieval, SearchRetrievalCoverage, SessionResult,
    SessionSummary, Totals, CONTRACT_VERSION, SCHEMA_VERSION,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use thiserror::Error;

mod subprocess;

use subprocess::collect_ctx_json;
#[cfg(test)]
use subprocess::{read_bounded_pipe, MAX_RETAINED_SUBPROCESS_STDERR_BYTES};

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
    pub env: BTreeMap<String, String>,
    pub timeout: Duration,
}

impl Default for LocalBackendConfig {
    fn default() -> Self {
        Self {
            ctx_binary: PathBuf::from("ctx"),
            data_root: None,
            env: BTreeMap::new(),
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
pub struct InitOptions;

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    pub provider: Option<String>,
    pub path: Option<PathBuf>,
    pub all: bool,
    pub resume: bool,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub query: Option<String>,
    pub terms: Vec<String>,
    pub limit: usize,
    pub backend: Option<String>,
    pub semantic_weight: Option<f64>,
    pub provider: Option<String>,
    pub workspace: Option<String>,
    pub since: Option<String>,
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
            terms: Vec::new(),
            limit: 20,
            backend: None,
            semantic_weight: None,
            provider: None,
            workspace: None,
            since: None,
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
        self.query
            .as_deref()
            .map(str::trim)
            .is_some_and(|query| !query.is_empty())
            || self.terms.iter().any(|term| !term.trim().is_empty())
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
    /// Maximum selected transcript events in one resumable MCP page.
    ///
    /// `None` preserves the SDK's unbounded streaming CLI adapter. Supplying a
    /// limit opts this call into the existing MCP `show_session` paging
    /// contract, which accepts values from 1 through 4,096.
    pub limit: Option<usize>,
    /// Opaque `next_cursor` from a preceding MCP `show_session` page.
    ///
    /// Supplying a cursor opts this call into MCP paging with its default
    /// 200-event limit when `limit` is `None`. Cursors are nonempty ASCII
    /// strings of at most 4,096 bytes, bound to the exact session and active
    /// Core generation.
    pub cursor: Option<String>,
}

impl Default for ShowSessionOptions {
    fn default() -> Self {
        Self {
            mode: "lite".to_owned(),
            limit: None,
            cursor: None,
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
        self.local_json(AgentHistoryOperation::Status, &["status", "--format=json"])
    }

    pub fn init(&self, options: InitOptions) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        let _ = options;
        self.local_json(
            AgentHistoryOperation::Init,
            &["setup", "--format=json", "--progress", "none"],
        )
    }

    pub fn sources(&self) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json(
            AgentHistoryOperation::Sources,
            &["sources", "--format=json"],
        )
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
                "search requires a query, term, or file option",
                false,
            ));
        }
        let mut owned = Vec::<String>::new();
        owned.push("search".to_owned());
        if let Some(query) = options.query {
            owned.push(query);
        }
        for term in options.terms {
            owned.push("--term".to_owned());
            owned.push(term);
        }
        owned.extend(["--limit".to_owned(), options.limit.to_string()]);
        push_opt(&mut owned, "--backend", options.backend);
        if let Some(semantic_weight) = options.semantic_weight {
            owned.extend(["--semantic-weight".to_owned(), semantic_weight.to_string()]);
        }
        push_opt(&mut owned, "--provider", options.provider);
        push_opt(&mut owned, "--workspace", options.workspace);
        push_opt(&mut owned, "--since", options.since);
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
        owned.push("--format=json".to_owned());
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
        if options.limit.is_some() || options.cursor.is_some() {
            let config = self.local_backend_config()?;
            let raw = run_ctx_mcp_show_session(config, id.as_ref(), &options)?;
            return normalize(AgentHistoryOperation::ShowSession, self.backend_info(), raw);
        }
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

    fn local_backend_config(&self) -> Result<&LocalBackendConfig, AgentHistoryError> {
        match &self.backend {
            AgentHistoryBackend::Local(config) => Ok(config),
            AgentHistoryBackend::Hosted(config) => {
                let mut details = JsonObject::new();
                details.insert("backend".to_owned(), json!("hosted"));
                Err(AgentHistoryError {
                    body: AgentHistoryErrorBody {
                        details: Some(details),
                        ..AgentHistoryErrorBody::new(
                            AgentHistoryErrorCode::NotSupported,
                            "hosted ctx agent history backend is not available in this in-repo SDK",
                            false,
                        )
                    },
                }
                .with_cause(config.base_url.clone()))
            }
        }
    }

    fn import_or_sync(
        &self,
        operation: AgentHistoryOperation,
        options: ImportOptions,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        let mut owned = vec![
            "import".to_owned(),
            "--format=json".to_owned(),
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
        let config = self.local_backend_config()?;

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

fn run_ctx_json(config: &LocalBackendConfig, args: &[String]) -> Result<Value, AgentHistoryError> {
    let mut command = Command::new(&config.ctx_binary);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.envs(&config.env);
    if let Some(data_root) = &config.data_root {
        command.env("CTX_DATA_ROOT", data_root);
    }
    command.env("CTX_ANALYTICS_ENABLED", "false");
    let child = command.spawn().map_err(|err| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::BackendUnavailable,
            "failed to start ctx CLI",
            true,
        )
        .with_cause(err.to_string())
    })?;
    collect_ctx_json(child, config.timeout)
}

fn run_ctx_mcp_show_session(
    config: &LocalBackendConfig,
    id: &str,
    options: &ShowSessionOptions,
) -> Result<Value, AgentHistoryError> {
    let mut arguments = serde_json::Map::from_iter([
        ("ctx_session_id".to_owned(), json!(id)),
        ("mode".to_owned(), json!(options.mode)),
    ]);
    if let Some(limit) = options.limit {
        arguments.insert("limit".to_owned(), json!(limit));
    }
    if let Some(cursor) = &options.cursor {
        arguments.insert("cursor".to_owned(), json!(cursor));
    }
    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "ctx-sdk", "version": env!("CARGO_PKG_VERSION") }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "show_session",
                "arguments": Value::Object(arguments)
            }
        }),
    ];

    let mut command = Command::new(&config.ctx_binary);
    command
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.envs(&config.env);
    if let Some(data_root) = &config.data_root {
        command.env("CTX_DATA_ROOT", data_root);
    }
    command.env("CTX_ANALYTICS_ENABLED", "false");
    let mut child = command.spawn().map_err(|err| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::BackendUnavailable,
            "failed to start ctx MCP server",
            true,
        )
        .with_cause(err.to_string())
    })?;

    let mut stdin = child.stdin.take().ok_or_else(|| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            "ctx MCP stdin was unavailable",
            true,
        )
    })?;
    for request in requests {
        serde_json::to_writer(&mut stdin, &request).map_err(|err| {
            AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "failed to encode ctx MCP request",
                false,
            )
            .with_cause(err.to_string())
        })?;
        stdin.write_all(b"\n").map_err(|err| {
            AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "failed to write ctx MCP request",
                true,
            )
            .with_cause(err.to_string())
        })?;
    }
    drop(stdin);

    let stdout = child.stdout.take().ok_or_else(|| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            "ctx MCP stdout was unavailable",
            true,
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            "ctx MCP stderr was unavailable",
            true,
        )
    })?;
    let stdout_reader = thread::spawn(move || read_pipe(stdout));
    let stderr_reader = thread::spawn(move || read_pipe(stderr));

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|err| {
            AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                "failed to wait for ctx MCP server",
                true,
            )
            .with_cause(err.to_string())
        })? {
            break status;
        }
        if started.elapsed() > config.timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::Timeout,
                "ctx MCP request timed out",
                true,
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = join_pipe(stdout_reader, "stdout")?;
    let stderr = join_pipe(stderr_reader, "stderr")?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        return Err(AgentHistoryError::new(
            classify_stderr(&stderr),
            stderr.trim().to_owned(),
            false,
        ));
    }

    let mut tool_response = None;
    for line in stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let response: Value = serde_json::from_slice(line).map_err(|err| {
            AgentHistoryError::new(
                AgentHistoryErrorCode::DecodeError,
                "failed to decode ctx MCP response",
                false,
            )
            .with_cause(err.to_string())
        })?;
        if response.get("id") == Some(&json!(2)) {
            tool_response = Some(response);
        }
    }
    let response = tool_response.ok_or_else(|| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::DecodeError,
            "ctx MCP response omitted show_session result",
            false,
        )
    })?;
    if let Some(error) = response.get("error") {
        return Err(AgentHistoryError::new(
            AgentHistoryErrorCode::AdapterError,
            "ctx MCP show_session request failed",
            false,
        )
        .with_cause(error.to_string()));
    }
    let result = response.get("result").ok_or_else(|| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::DecodeError,
            "ctx MCP response omitted result",
            false,
        )
    })?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        let structured = result
            .get("structuredContent")
            .cloned()
            .unwrap_or(Value::Null);
        let message = structured
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("ctx MCP show_session tool failed");
        let code = match structured.get("error_code").and_then(Value::as_str) {
            Some("invalid_request" | "invalid_cursor" | "cursor_mismatch" | "cursor_stale") => {
                AgentHistoryErrorCode::InvalidRequest
            }
            Some("not_found") => AgentHistoryErrorCode::NotFound,
            _ => AgentHistoryErrorCode::AdapterError,
        };
        return Err(AgentHistoryError::new(code, message, false).with_cause(structured.to_string()));
    }
    result.get("structuredContent").cloned().ok_or_else(|| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::DecodeError,
            "ctx MCP response omitted structuredContent",
            false,
        )
    })
}

fn read_pipe(mut pipe: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn join_pipe(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    name: &str,
) -> Result<Vec<u8>, AgentHistoryError> {
    reader
        .join()
        .map_err(|_| {
            AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                format!("ctx MCP {name} reader panicked"),
                true,
            )
        })?
        .map_err(|err| {
            AgentHistoryError::new(
                AgentHistoryErrorCode::AdapterError,
                format!("failed to read ctx MCP {name}"),
                true,
            )
            .with_cause(err.to_string())
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
    let current = camelize_object_keys(raw);
    let mut status = serde_json::Map::new();
    for key in [
        "initialized",
        "readOnly",
        "dataRoot",
        "indexedItems",
        "indexedSessions",
        "indexedEvents",
        "indexedSources",
        "historyEpoch",
        "lexical",
        "refresh",
        "semantic",
        "daemon",
    ] {
        if let Some(value) = current.get(key) {
            status.insert(key.to_owned(), value.clone());
        }
    }
    status.entry("initialized".to_owned()).or_insert_with(|| {
        Value::Bool(
            current
                .get("lexical")
                .and_then(|lexical| lexical.get("generationId"))
                .and_then(Value::as_str)
                .is_some(),
        )
    });
    status.insert("localOnly".to_owned(), Value::Bool(true));
    decode_payload(Value::Object(status), "status")
}

fn normalize_import(raw: &Value) -> Result<ImportResult, AgentHistoryError> {
    let value = camel_alias_object(raw, &[("resume_mode", "resumeMode")]);
    decode_payload(camelize_object_keys(&value), "import")
}

fn normalize_search(raw: &Value) -> Result<SearchResult, AgentHistoryError> {
    let value = camel_alias_object(raw, &[("generated_at", "generatedAt")]);
    let mut value = camelize_object_keys(&value);
    bridge_search_pagination(&mut value);
    decode_payload(value, "search")
}

fn bridge_search_pagination(value: &mut Value) {
    let Some(search) = value.as_object_mut() else {
        return;
    };
    if search.contains_key("pagination") {
        return;
    }
    let Some(result_window) = search.get("resultWindow").and_then(Value::as_object) else {
        return;
    };

    let mut pagination = serde_json::Map::new();
    if let Some(limit) = result_window.get("limit") {
        pagination.insert("limit".to_owned(), limit.clone());
    }
    if let Some(more_available) = result_window.get("moreAvailable") {
        pagination.insert("hasMore".to_owned(), more_available.clone());
    }
    search.insert("pagination".to_owned(), Value::Object(pagination));
}

fn normalize_event(raw: &Value) -> Result<EventResult, AgentHistoryError> {
    let value = json!({
        "event": raw.get("event").cloned(),
        "events": raw.get("events").cloned().unwrap_or_else(|| json!([]))
    });
    decode_payload(camelize_object_keys(&value), "event")
}

fn normalize_session(raw: &Value) -> Result<SessionResult, AgentHistoryError> {
    let value = json!({
        "session": raw.get("session").cloned(),
        "events": raw.get("events").cloned().unwrap_or_else(|| json!([])),
        "mode": raw.get("mode").cloned(),
        "format": raw.get("format").cloned(),
        "pagination": raw.get("pagination").cloned()
    });
    decode_payload(camelize_object_keys(&value), "session")
}

pub fn fixture_path(name: impl AsRef<Path>) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/agent-history-v1/fixtures")
        .join(name)
}

#[cfg(test)]
mod tests;
