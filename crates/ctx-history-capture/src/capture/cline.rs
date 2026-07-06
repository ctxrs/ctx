#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct ClineTaskJsonImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for ClineTaskJsonImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: utc_now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ClineTaskJsonAdapter;

impl ProviderCaptureAdapter for ClineTaskJsonAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Cline
    }

    fn source_format(&self) -> &str {
        CLINE_TASK_JSON_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_task_json_history(path, context, task_json_provider(CaptureProvider::Cline))
    }
}

pub fn import_cline_task_json_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ClineTaskJsonImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = ClineTaskJsonAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;

    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )
}

pub(crate) const CLINE_TASK_JSON_SOURCE_FORMAT: &str = "cline_task_directory_json";

pub(crate) fn task_json_root_history_items(
    path: &Path,
    spec: TaskJsonProviderSpec,
    context: &ProviderAdapterContext,
) -> BTreeMap<String, Value> {
    if spec.provider != CaptureProvider::Cline {
        return BTreeMap::new();
    }
    let mut candidates = Vec::new();
    if path.is_dir() {
        candidates.push(path.join("state").join("taskHistory.json"));
        candidates.push(path.join("..").join("state").join("taskHistory.json"));
    }
    if let Some(parent) = path.parent() {
        candidates.push(parent.join("state").join("taskHistory.json"));
        if let Some(grandparent) = parent.parent() {
            candidates.push(grandparent.join("state").join("taskHistory.json"));
        }
    }

    for candidate in candidates {
        let Ok(value) = read_task_json_value(&candidate, context) else {
            continue;
        };
        let Some(items) = value.as_array() else {
            continue;
        };
        let mut out = BTreeMap::new();
        for item in items {
            if let Some(id) = task_json_string_field(item, &["id", "taskId"]) {
                out.insert(id, item.clone());
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    BTreeMap::new()
}
