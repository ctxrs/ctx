use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
    time::{Duration, Instant},
};

use ctx_history_core::CaptureProvider;
use ctx_history_index::WriterOptions;
use serde_json::{json, Value};

use crate::{
    refresh_source_backed_generation_with_detailed_progress, register_landed_source_backed_route,
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus, SourceBackedCoordinatorError, SourceBackedCurrentSourceProgressStage,
    SourceBackedDetailedRefreshProgress, SourceBackedProviderRegistry, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteSelection,
};

const LONG_PREFIX_ROWS: usize = 4 * 1024;
const LONG_PREFIX_ROW_BYTES: usize = 8 * 1024;
const CANCELLATION_IGNORED_ROWS: usize = 8 * 1024 * 1024;
const CANCELLATION_WRITE_CHUNK_ROWS: usize = 64 * 1024;

fn write_long_ignored_prefix(path: &Path, retained_rows: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = BufWriter::new(File::create(path).unwrap());
    let mut ignored = vec![b' '; LONG_PREFIX_ROW_BYTES];
    ignored.push(b'\n');
    for _ in 0..LONG_PREFIX_ROWS {
        writer.write_all(&ignored).unwrap();
    }
    for row in retained_rows {
        serde_json::to_writer(&mut writer, row).unwrap();
        writer.write_all(b"\n").unwrap();
    }
    writer.flush().unwrap();
}

fn write_long_ignored_source(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = BufWriter::new(File::create(path).unwrap());
    let chunk = b"{}\n".repeat(CANCELLATION_WRITE_CHUNK_ROWS);
    for _ in 0..(CANCELLATION_IGNORED_ROWS / CANCELLATION_WRITE_CHUNK_ROWS) {
        writer.write_all(&chunk).unwrap();
    }
    writer.flush().unwrap();
}

fn registry(
    provider: CaptureProvider,
    source_format: &'static str,
    root: &Path,
) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider,
            path: root.to_path_buf(),
            exists: true,
            source_format,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
            route_provenance: Default::default(),
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn assert_long_ordinary_projection_reports_parsing(
    name: &str,
    registry: &SourceBackedProviderRegistry,
    index_root: &Path,
) {
    let mut updates = Vec::new();
    let receipt = refresh_source_backed_generation_with_detailed_progress(
        index_root,
        registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
        |update: SourceBackedDetailedRefreshProgress| {
            updates.push(update);
            Ok(())
        },
    )
    .unwrap();
    assert!(receipt.failed_routes.is_empty(), "{name}");

    let accepted = updates
        .iter()
        .position(|update| update.progress.processed_messages > 0)
        .unwrap_or_else(|| panic!("{name} retained message was never durably accepted"));
    let parsing = updates
        .iter()
        .position(|update| {
            update.current_source_progress.is_some_and(|progress| {
                progress.stage == SourceBackedCurrentSourceProgressStage::Parsing
            })
        })
        .unwrap_or_else(|| panic!("{name} ordinary projector emitted no Parsing activity"));
    assert!(parsing < accepted, "{name} Parsing must precede acceptance");
    assert_eq!(updates[parsing].progress.processed_sessions, 0, "{name}");
    assert_eq!(updates[parsing].progress.processed_messages, 0, "{name}");
    assert_eq!(updates[parsing].progress.processed_tool_calls, 0, "{name}");
    assert_eq!(updates[parsing].progress.processed_bytes, 0, "{name}");

    let terminal = updates.last().expect("terminal progress");
    assert_eq!(terminal.progress.phase, "committed", "{name}");
    assert!(terminal.progress.current_source.is_none(), "{name}");
    assert!(terminal.current_source_progress.is_none(), "{name}");
    assert_eq!(terminal.progress.processed_sessions, 1, "{name}");
    assert_eq!(terminal.progress.processed_messages, 1, "{name}");
    assert_eq!(terminal.progress.processed_tool_calls, 0, "{name}");
    assert!(terminal.progress.processed_bytes > 0, "{name}");
}

#[test]
fn production_ordinary_projectors_report_long_pre_acceptance_parsing() {
    let temp = crate::test_support_paths::tempdir().unwrap();

    let claude_root = temp.path().join("claude-projects");
    let claude_session = "ordinary-claude-session";
    write_long_ignored_prefix(
        &claude_root
            .join("project")
            .join(format!("{claude_session}.jsonl")),
        &[json!({
            "type": "user",
            "uuid": "ordinary-claude-message",
            "sessionId": claude_session,
            "message": {"role": "user", "content": "claude ordinary marker"}
        })],
    );
    assert_long_ordinary_projection_reports_parsing(
        "Claude",
        &registry(
            CaptureProvider::Claude,
            "claude_projects_jsonl_tree",
            &claude_root,
        ),
        &temp.path().join("claude-index"),
    );

    let gemini_root = temp.path().join("gemini");
    write_long_ignored_prefix(
        &gemini_root.join("tmp/project/chats/ordinary-session.jsonl"),
        &[
            json!({
                "sessionId": "ordinary-gemini-session",
                "startTime": "2026-08-19T00:00:00Z",
                "kind": "main"
            }),
            json!({
                "id": "ordinary-gemini-message",
                "timestamp": "2026-08-19T00:00:01Z",
                "type": "user",
                "content": "gemini ordinary marker"
            }),
        ],
    );
    assert_long_ordinary_projection_reports_parsing(
        "Gemini",
        &registry(
            CaptureProvider::Gemini,
            ctx_history_provider_gemini::GEMINI_CLI_SOURCE_FORMAT,
            &gemini_root,
        ),
        &temp.path().join("gemini-index"),
    );

    let codex_history = temp.path().join("codex/history.jsonl");
    write_long_ignored_prefix(
        &codex_history,
        &[json!({
            "session_id": "ordinary-codex-prompt-session",
            "ts": 1_787_097_600_i64,
            "text": "codex prompt ordinary marker"
        })],
    );
    assert_long_ordinary_projection_reports_parsing(
        "Codex prompt history",
        &registry(
            CaptureProvider::Codex,
            "codex_history_jsonl",
            &codex_history,
        ),
        &temp.path().join("codex-prompt-index"),
    );
}

#[test]
fn callback_failure_cancels_and_joins_long_production_jsonl_scan() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let claude_root = temp.path().join("claude-projects");
    write_long_ignored_source(&claude_root.join("project/ignored.jsonl"));
    let registry = registry(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        &claude_root,
    );
    let mut parsing_callbacks = 0;
    let started = Instant::now();

    let error = refresh_source_backed_generation_with_detailed_progress(
        temp.path().join("index"),
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
        |update: SourceBackedDetailedRefreshProgress| {
            if update.current_source_progress.is_some_and(|progress| {
                progress.stage == SourceBackedCurrentSourceProgressStage::Parsing
            }) {
                parsing_callbacks += 1;
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "injected production parsing callback failure",
                ));
            }
            Ok(())
        },
    )
    .expect_err("production parsing callback failure must remain systemic");

    assert!(matches!(error, SourceBackedCoordinatorError::Progress(_)));
    assert_eq!(parsing_callbacks, 1);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "production callback failure must cancel and join the JSONL scanner within one second"
    );
}
