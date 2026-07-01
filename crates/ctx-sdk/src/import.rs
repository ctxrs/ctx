use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_capture::{
    import_antigravity_cli_history, import_claude_projects_jsonl_tree, import_codex_history_jsonl,
    import_codex_session_jsonl, import_codex_session_tree, import_copilot_cli_session_events,
    import_cursor_native_history, import_factory_ai_droid_sessions, import_gemini_cli_history,
    import_opencode_sqlite, import_pi_session_jsonl, stable_capture_uuid,
    AntigravityCliImportOptions, ClaudeProjectsImportOptions, CodexEventImportMode,
    CodexHistoryImportOptions, CodexSessionImportOptions, CodexSessionImportProgressCallback,
    CodexToolOutputMode, CopilotCliImportOptions, CursorNativeImportOptions,
    FactoryAiDroidImportOptions, GeminiCliImportOptions, OpenCodeSqliteImportOptions,
    PiSessionImportOptions, ProviderImportSummary, ProviderImportSupport, ProviderSource,
    ProviderSourceStatus,
};
use ctx_history_core::{CaptureProvider, HistoryRecord};
use ctx_history_store::Store;

use crate::{
    client::CtxClient,
    error::{Error, Result},
};

/// Options for importing provider history.
#[derive(Clone)]
pub struct ImportOptions {
    pub allow_partial_failures: bool,
    pub continue_on_error: bool,
    pub refresh_search_index: bool,
    pub optimize_search_index: bool,
    pub codex_tool_output_mode: CodexToolOutputMode,
    pub codex_event_mode: CodexEventImportMode,
    pub include_codex_notices: bool,
    pub codex_progress: Option<CodexSessionImportProgressCallback>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            allow_partial_failures: true,
            continue_on_error: true,
            refresh_search_index: true,
            optimize_search_index: false,
            codex_tool_output_mode: CodexToolOutputMode::Skip,
            codex_event_mode: CodexEventImportMode::Search,
            include_codex_notices: false,
            codex_progress: None,
        }
    }
}

impl ImportOptions {
    /// Preserve richer Codex events and tool-output previews.
    pub fn rich_codex(mut self) -> Self {
        self.codex_tool_output_mode = CodexToolOutputMode::Full;
        self.codex_event_mode = CodexEventImportMode::Rich;
        self.include_codex_notices = true;
        self
    }

    pub fn fail_fast(mut self) -> Self {
        self.continue_on_error = false;
        self
    }
}

/// Import totals aggregated across all sources.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportTotals {
    pub source_files: usize,
    pub source_bytes: u64,
    pub imported_sources: usize,
    pub failed_sources: usize,
    pub imported_sessions: usize,
    pub imported_events: usize,
    pub imported_edges: usize,
    pub skipped: usize,
    pub failed: usize,
}

impl ImportTotals {
    fn add_imported(&mut self, summary: &ProviderImportSummary, stats: SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.imported_sources += 1;
        self.imported_sessions += summary.imported_sessions;
        self.imported_events += summary.imported_events;
        self.imported_edges += summary.imported_edges;
        self.skipped += summary.skipped;
        self.failed += summary.failed;
    }

    fn add_failed(&mut self, stats: SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.failed_sources += 1;
        self.failed += 1;
    }
}

/// Per-source import result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSourceReport {
    pub source: ProviderSource,
    pub stats: SourceStats,
    pub summary: Option<ProviderImportSummary>,
    pub error: Option<String>,
}

/// Import report returned by SDK import methods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub totals: ImportTotals,
    pub sources: Vec<ImportSourceReport>,
}

/// Source file statistics used for import reporting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceStats {
    pub files: usize,
    pub bytes: u64,
}

impl CtxClient {
    /// Import all currently available native sources, optionally scoped to a provider.
    pub fn import_available_sources(
        &self,
        provider: Option<CaptureProvider>,
        options: ImportOptions,
    ) -> Result<ImportReport> {
        let sources = match provider {
            Some(provider) => self.sources_for_provider(provider),
            None => self.sources(),
        }
        .into_iter()
        .filter(is_importable_source)
        .collect::<Vec<_>>();

        if sources.is_empty() {
            return Err(Error::NoImportableSources);
        }

        self.import_sources(sources, options)
    }

    /// Import one explicit provider history path.
    pub fn import_path(
        &self,
        provider: CaptureProvider,
        path: impl Into<PathBuf>,
        options: ImportOptions,
    ) -> Result<ImportReport> {
        self.import_sources(vec![self.source_for_path(provider, path)], options)
    }

    /// Import a set of provider sources directly.
    pub fn import_sources(
        &self,
        sources: Vec<ProviderSource>,
        options: ImportOptions,
    ) -> Result<ImportReport> {
        if sources.is_empty() {
            return Err(Error::NoImportableSources);
        }

        let mut store = self.open_store()?;
        let mut report = ImportReport::default();
        for source in sources {
            let stats = source_stats(&source.path).unwrap_or_default();
            match validate_import_supported(&source)
                .and_then(|()| import_one_source(&mut store, &source, &options))
            {
                Ok(summary) => {
                    report.totals.add_imported(&summary, stats);
                    report.sources.push(ImportSourceReport {
                        source,
                        stats,
                        summary: Some(summary),
                        error: None,
                    });
                }
                Err(error) if options.continue_on_error => {
                    report.totals.add_failed(stats);
                    report.sources.push(ImportSourceReport {
                        source,
                        stats,
                        summary: None,
                        error: Some(error.to_string()),
                    });
                }
                Err(error) => return Err(error),
            }
        }

        if options.refresh_search_index
            && (report.totals.imported_sessions > 0
                || report.totals.imported_events > 0
                || report.totals.imported_edges > 0)
        {
            store.refresh_search_index()?;
        }
        if options.optimize_search_index {
            store.optimize_search_index()?;
        }

        Ok(report)
    }
}

fn is_importable_source(source: &ProviderSource) -> bool {
    source.exists
        && source.import_support == ProviderImportSupport::Native
        && source.status == ProviderSourceStatus::Available
}

fn validate_import_supported(source: &ProviderSource) -> Result<()> {
    match source.import_support {
        ProviderImportSupport::Native => Ok(()),
        ProviderImportSupport::Unsupported => Err(Error::UnsupportedProviderImport {
            provider: source.provider,
            reason: source
                .unsupported_reason
                .unwrap_or("no native local-history parser is implemented")
                .to_owned(),
        }),
    }
}

fn import_one_source(
    store: &mut Store,
    source: &ProviderSource,
    options: &ImportOptions,
) -> Result<ProviderImportSummary> {
    let record = import_record_for_source(source);
    let record_id = record.id;
    store.upsert_record(&record)?;

    let source_path = Some(source.path.clone());
    let summary = match source.provider {
        CaptureProvider::Codex if source.path.is_dir() => import_codex_session_tree(
            &source.path,
            store,
            CodexSessionImportOptions {
                source_path,
                history_record_id: Some(record_id),
                allow_partial_failures: options.allow_partial_failures,
                tool_output_mode: options.codex_tool_output_mode,
                event_mode: options.codex_event_mode,
                include_notices: options.include_codex_notices,
                progress: options.codex_progress.clone(),
                ..CodexSessionImportOptions::default()
            },
        )?,
        CaptureProvider::Codex
            if source.path.file_name().and_then(|name| name.to_str()) == Some("history.jsonl") =>
        {
            import_codex_history_jsonl(
                &source.path,
                store,
                CodexHistoryImportOptions {
                    source_path,
                    history_record_id: Some(record_id),
                    allow_partial_failures: options.allow_partial_failures,
                    ..CodexHistoryImportOptions::default()
                },
            )?
        }
        CaptureProvider::Codex => import_codex_session_jsonl(
            &source.path,
            store,
            CodexSessionImportOptions {
                source_path,
                history_record_id: Some(record_id),
                allow_partial_failures: options.allow_partial_failures,
                tool_output_mode: options.codex_tool_output_mode,
                event_mode: options.codex_event_mode,
                include_notices: options.include_codex_notices,
                progress: options.codex_progress.clone(),
                ..CodexSessionImportOptions::default()
            },
        )?,
        CaptureProvider::Pi => import_pi_session_jsonl(
            &source.path,
            store,
            PiSessionImportOptions {
                source_path,
                history_record_id: Some(record_id),
                allow_partial_failures: options.allow_partial_failures,
                ..PiSessionImportOptions::default()
            },
        )?,
        CaptureProvider::Claude => import_claude_projects_jsonl_tree(
            &source.path,
            store,
            ClaudeProjectsImportOptions {
                source_path,
                history_record_id: Some(record_id),
                allow_partial_failures: options.allow_partial_failures,
                ..ClaudeProjectsImportOptions::default()
            },
        )?,
        CaptureProvider::OpenCode => import_opencode_sqlite(
            &source.path,
            store,
            OpenCodeSqliteImportOptions {
                source_path,
                history_record_id: Some(record_id),
                allow_partial_failures: options.allow_partial_failures,
                ..OpenCodeSqliteImportOptions::default()
            },
        )?,
        CaptureProvider::Antigravity => import_antigravity_cli_history(
            &source.path,
            store,
            AntigravityCliImportOptions {
                source_path,
                history_record_id: Some(record_id),
                allow_partial_failures: options.allow_partial_failures,
                ..AntigravityCliImportOptions::default()
            },
        )?,
        CaptureProvider::Gemini => import_gemini_cli_history(
            &source.path,
            store,
            GeminiCliImportOptions {
                source_path,
                history_record_id: Some(record_id),
                allow_partial_failures: options.allow_partial_failures,
                ..GeminiCliImportOptions::default()
            },
        )?,
        CaptureProvider::Cursor => import_cursor_native_history(
            &source.path,
            store,
            CursorNativeImportOptions {
                source_path,
                history_record_id: Some(record_id),
                allow_partial_failures: options.allow_partial_failures,
                ..CursorNativeImportOptions::default()
            },
        )?,
        CaptureProvider::CopilotCli => import_copilot_cli_session_events(
            &source.path,
            store,
            CopilotCliImportOptions {
                source_path,
                history_record_id: Some(record_id),
                allow_partial_failures: options.allow_partial_failures,
                ..CopilotCliImportOptions::default()
            },
        )?,
        CaptureProvider::FactoryAiDroid => import_factory_ai_droid_sessions(
            &source.path,
            store,
            FactoryAiDroidImportOptions {
                source_path,
                history_record_id: Some(record_id),
                allow_partial_failures: options.allow_partial_failures,
                ..FactoryAiDroidImportOptions::default()
            },
        )?,
        provider => return Err(Error::UnregisteredProvider { provider }),
    };

    Ok(summary)
}

fn import_record_for_source(source: &ProviderSource) -> HistoryRecord {
    let key = format!(
        "agent-history:{}:{}",
        source.provider.as_str(),
        source.path.display()
    );
    let mut record = HistoryRecord::new(
        format!("{} agent history", source.provider.as_str()),
        format!(
            "Indexed local agent history from {} ({})",
            source.path.display(),
            source.source_format
        ),
        vec!["agent-history".into(), source.provider.as_str().into()],
        "agent_history",
        source.path.parent().map(|path| path.display().to_string()),
    );
    record.id = stable_capture_uuid(&key, "record");
    record
}

fn source_stats(path: &Path) -> Result<SourceStats> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_file() {
        return Ok(SourceStats {
            files: 1,
            bytes: metadata.len(),
        });
    }
    if !metadata.file_type().is_dir() {
        return Ok(SourceStats::default());
    }

    let mut stats = SourceStats::default();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                let metadata = entry.metadata()?;
                stats.files += 1;
                stats.bytes = stats.bytes.saturating_add(metadata.len());
            }
        }
    }
    Ok(stats)
}
