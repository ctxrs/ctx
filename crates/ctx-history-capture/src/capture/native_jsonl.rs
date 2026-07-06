#[allow(unused_imports)]
use super::*;

pub(crate) struct NativeJsonlTreeImport<'a> {
    pub(crate) path: &'a Path,
    pub(crate) machine_id: String,
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) imported_at: DateTime<Utc>,
    pub(crate) history_record_id: Option<Uuid>,
    pub(crate) allow_partial_failures: bool,
}

pub(crate) fn import_native_jsonl_tree<A: ProviderCaptureAdapter>(
    store: &mut Store,
    request: NativeJsonlTreeImport<'_>,
    adapter: A,
) -> Result<ProviderImportSummary> {
    let source_path = request
        .source_path
        .unwrap_or_else(|| request.path.to_path_buf());
    let normalization = adapter.normalize_path(
        request.path,
        &ProviderAdapterContext {
            machine_id: request.machine_id,
            source_path: Some(source_path),
            imported_at: request.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: request.history_record_id,
            allow_partial_failures: request.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )
}

pub(crate) fn native_jsonl_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
        .or_else(|| {
            value
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_utc)
        })
        .or_else(|| {
            value
                .pointer("/time/created")
                .and_then(Value::as_i64)
                .and_then(DateTime::<Utc>::from_timestamp_millis)
        })
}

pub(crate) fn native_jsonl_tokens(_provider: CaptureProvider, value: &Value) -> Option<Value> {
    value
        .get("tokens")
        .or_else(|| value.get("usageMetadata"))
        .cloned()
}

pub(crate) fn native_jsonl_content_has(value: &Value, expected: &str) -> bool {
    value
        .pointer("/message/content")
        .or_else(|| value.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}
