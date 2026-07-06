#[allow(unused_imports)]
use super::*;

pub type KiloSqliteImportOptions = OpenCodeSqliteImportOptions;

#[derive(Debug, Clone, Copy, Default)]
pub struct KiloSqliteAdapter;

impl ProviderCaptureAdapter for KiloSqliteAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Kilo
    }

    fn source_format(&self) -> &str {
        KILO_SQLITE_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        ensure_regular_provider_transcript_file(path)?;
        normalize_opencode_sqlite(path, context, &KILO_SQLITE_DIALECT)
    }
}

pub fn import_kilo_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: KiloSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = KiloSqliteAdapter.normalize_path(
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

pub(crate) const KILO_SQLITE_SOURCE_FORMAT: &str = "kilo_sqlite";

pub(crate) const KILO_SQLITE_DIALECT: OpenCodeSqliteDialect = OpenCodeSqliteDialect {
    provider: CaptureProvider::Kilo,
    display_name: "Kilo",
    source_format: KILO_SQLITE_SOURCE_FORMAT,
    session_time_created_field: "Kilo session time_created",
    session_message_seq_field: "Kilo session_message seq",
    session_message_time_created_field: "Kilo session_message time_created",
    event_time_created_field: "Kilo event time.created",
};
