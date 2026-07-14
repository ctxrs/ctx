use std::path::Path;

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::common::path_inventory::SortedJsonlPathInventory;
use crate::provider::adapter::{
    AntigravityCliJsonlAdapter, CopilotCliSessionEventsAdapter, CursorAgentTranscriptJsonlAdapter,
    FactoryAiDroidJsonlAdapter, GeminiCliJsonlAdapter, KimiCodeCliWireJsonlAdapter,
    LingmaSqliteAdapter, MistralVibeJsonlAdapter, MuxJsonlAdapter, ProviderCaptureAdapter,
    QoderJsonlAdapter, QwenCodeJsonlAdapter, RovoDevSessionJsonAdapter,
    WindsurfCascadeHookTranscriptJsonlAdapter, ZedThreadsSqliteAdapter,
};
use crate::provider::importer::{
    import_native_jsonl_tree, import_normalized_provider_capture_stream_with_metrics,
    import_normalized_provider_captures, NativeJsonlTreeImport, ProviderNormalizationStreamMetrics,
};
use crate::provider::providers::native_jsonl::{
    native_jsonl_header_start_time, native_jsonl_missing_reason, native_jsonl_timestamp,
    provider_jsonl_path_is_native, scan_native_jsonl_session_reader,
    stream_native_jsonl_session_reader, NativeJsonlScan, NativeJsonlStreamOptions,
};
use crate::provider::providers::warp::normalize_warp_sqlite;
use crate::{
    AntigravityCliImportOptions, CaptureError, CopilotCliImportOptions, CursorNativeImportOptions,
    FactoryAiDroidImportOptions, GeminiCliImportOptions, KimiCodeCliImportOptions,
    LingmaSqliteImportOptions, MistralVibeImportOptions, MuxImportOptions,
    NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportSummary, ProviderJsonlReader, ProviderNormalizationResult, QoderImportOptions,
    QwenCodeImportOptions, Result, RovoDevImportOptions, TabnineCliImportOptions,
    WarpSqliteImportOptions, WindsurfCascadeHookImportOptions, ZedThreadsSqliteImportOptions,
    TABNINE_CLI_SOURCE_FORMAT,
};

pub fn import_antigravity_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: AntigravityCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        },
        AntigravityCliJsonlAdapter,
    )
}

pub fn import_gemini_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: GeminiCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        },
        GeminiCliJsonlAdapter,
    )
}

pub fn import_tabnine_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: TabnineCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_tabnine_cli_history_streamed(path.as_ref(), store, options).map(|(summary, _)| summary)
}

fn import_tabnine_cli_history_streamed(
    path: &Path,
    store: &mut Store,
    options: TabnineCliImportOptions,
) -> Result<(ProviderImportSummary, ProviderNormalizationStreamMetrics)> {
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let context = ProviderAdapterContext {
        machine_id: options.machine_id,
        source_path: Some(source_path),
        source_root: None,
        imported_at: options.imported_at,
    };
    let path_inventory = SortedJsonlPathInventory::build(path, |candidate| {
        provider_jsonl_path_is_native(CaptureProvider::Tabnine, candidate)
    })?;
    if path_inventory.metrics().paths == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::Tabnine),
        });
    }

    let (summary, mut metrics) = import_normalized_provider_capture_stream_with_metrics(
        store,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
        |emit| {
            let mut any_real_message = false;
            let mut normalization_failures = 0usize;
            path_inventory.for_each(|file_path| {
                let mut reader = ProviderJsonlReader::open_replacement(&file_path)?;
                let scan = scan_native_jsonl_session_reader(
                    &file_path,
                    &mut reader,
                    &context,
                    CaptureProvider::Tabnine,
                    TABNINE_CLI_SOURCE_FORMAT,
                )?;
                any_real_message |= scan.has_real_message;

                let NativeJsonlScan {
                    header,
                    has_real_message,
                    summary,
                    valid_records,
                    first_valid_line,
                } = scan;
                let Some(header) = header else {
                    let mut rejected = ProviderNormalizationResult {
                        summary,
                        ..ProviderNormalizationResult::default()
                    };
                    if valid_records == 0 {
                        if rejected.summary.failed == 0 {
                            rejected.summary.failed = 1;
                            rejected.summary.sample_failure(ProviderImportFailure {
                                line: 0,
                                error: native_jsonl_missing_reason(CaptureProvider::Tabnine)
                                    .to_owned(),
                            });
                        }
                    } else {
                        rejected.summary.failed += 1;
                        rejected.summary.sample_failure(ProviderImportFailure {
                            line: first_valid_line.unwrap_or(0),
                            error: "no importable native JSONL session header".to_owned(),
                        });
                    }
                    normalization_failures += rejected.summary.failed;
                    emit(rejected)?;
                    return Ok(());
                };

                let started_at = native_jsonl_header_start_time(CaptureProvider::Tabnine, &header)
                    .or_else(|| native_jsonl_timestamp(&header))
                    .unwrap_or(context.imported_at);
                let mut saw_capture = false;
                stream_native_jsonl_session_reader(
                    &file_path,
                    &mut reader,
                    &context,
                    NativeJsonlStreamOptions {
                        provider: CaptureProvider::Tabnine,
                        source_format: TABNINE_CLI_SOURCE_FORMAT,
                        header,
                        started_at,
                    },
                    |mut batch| {
                        normalization_failures += batch.summary.failed;
                        saw_capture |= !batch.captures.is_empty();
                        if !has_real_message {
                            batch.summary.skipped += batch.captures.len();
                            batch.summary.skipped_events += batch
                                .captures
                                .iter()
                                .filter(|(_, capture)| capture.event.is_some())
                                .count();
                            batch.summary.skipped += batch.files_touched.len();
                            batch.captures.clear();
                            batch.files_touched.clear();
                        }
                        emit(batch)
                    },
                )?;
                if !has_real_message && saw_capture {
                    let mut rejected = ProviderNormalizationResult::default();
                    rejected.summary.skipped_sessions = 1;
                    emit(rejected)?;
                }
                Ok(())
            })?;
            if !any_real_message && normalization_failures == 0 {
                let mut rejected = ProviderNormalizationResult::default();
                rejected.summary.failed = 1;
                rejected.summary.sample_failure(ProviderImportFailure {
                    line: 0,
                    error: "provider source contained no real conversation message".to_owned(),
                });
                emit(rejected)?;
            }
            Ok(())
        },
    )?;
    let path_metrics = path_inventory.metrics();
    metrics.path_inventory_entries = path_metrics.paths;
    metrics.max_path_inventory_batch = path_metrics.max_in_memory_batch;
    Ok((summary, metrics))
}

pub fn import_cursor_native_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CursorNativeImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        },
        CursorAgentTranscriptJsonlAdapter,
    )
}

pub fn import_windsurf_cascade_hook_transcripts(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: WindsurfCascadeHookImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        },
        WindsurfCascadeHookTranscriptJsonlAdapter,
    )
}

pub fn import_warp_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: WarpSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = normalize_warp_sqlite(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: false,
        },
    )
}

pub fn import_qoder_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: QoderImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        },
        QoderJsonlAdapter,
    )
}

pub fn import_zed_threads_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ZedThreadsSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = ZedThreadsSqliteAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )
}

pub fn import_lingma_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: LingmaSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = LingmaSqliteAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )
}

pub fn import_factory_ai_droid_sessions(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: FactoryAiDroidImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        },
        FactoryAiDroidJsonlAdapter,
    )
}

pub fn import_copilot_cli_session_events(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CopilotCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        },
        CopilotCliSessionEventsAdapter,
    )
}

pub fn import_qwen_code_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: QwenCodeImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        },
        QwenCodeJsonlAdapter,
    )
}

pub fn import_kimi_code_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: KimiCodeCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        },
        KimiCodeCliWireJsonlAdapter,
    )
}

pub fn import_rovodev_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: RovoDevImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = RovoDevSessionJsonAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )
}

pub fn import_mistral_vibe_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: MistralVibeImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        },
        MistralVibeJsonlAdapter,
    )
}

pub fn import_mux_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: MuxImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
        },
        MuxJsonlAdapter,
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ctx_history_core::FileChangeKind;
    use serde_json::{json, Value};
    use tempfile::tempdir;

    use super::*;
    use crate::provider::importer::{
        provider_edge_uuid, provider_source_cursor_stream, provider_source_edge_uuid,
    };
    use crate::provider::providers::native_jsonl::normalize_jsonl_tree;

    fn jsonl(value: Value) -> String {
        format!("{}\n", serde_json::to_string(&value).unwrap())
    }

    #[test]
    fn tabnine_production_entry_streams_bounded_batches_with_legacy_results() {
        const ROWS: usize = 64 * 5 + 7;

        let temp = tempdir().unwrap();
        let root = temp.path().join("tabnine/agent");
        let chats = root.join("tmp/project/chats");
        fs::create_dir_all(&chats).unwrap();

        let mut large = jsonl(json!({
            "sessionId": "tabnine-large",
            "startTime": "2026-07-14T10:00:00Z",
            "timestamp": "2026-07-14T11:00:00Z",
            "id": "large-header",
            "type": "user",
            "content": "first real message"
        }));
        for index in 0..ROWS {
            let row = if index == 0 {
                json!({
                    "id": "large-tool",
                    "timestamp": "2026-07-14T10:00:01Z",
                    "type": "tabnine",
                    "toolCalls": [{
                        "name": "apply_patch",
                        "arguments": "*** Begin Patch\n*** Add File: src/tabnine.txt\n+created\n*** End Patch"
                    }]
                })
            } else {
                json!({
                    "id": format!("large-{index}"),
                    "timestamp": "2026-07-14T10:00:01Z",
                    "type": if index % 2 == 0 { "user" } else { "tabnine" },
                    "content": format!("large message {index}")
                })
            };
            large.push_str(&jsonl(row));
        }
        large.push_str("{malformed-complete-row}\n");
        fs::write(chats.join("large.jsonl"), large).unwrap();

        fs::write(
            chats.join("metadata-only.jsonl"),
            format!(
                "{}{}",
                jsonl(json!({
                    "sessionId": "tabnine-metadata-only",
                    "startTime": "2026-07-14T12:00:00Z",
                    "id": "metadata-header",
                    "type": "notice"
                })),
                jsonl(json!({
                    "id": "metadata-row",
                    "timestamp": "2026-07-14T12:00:01Z",
                    "$set": {"model": "example"}
                }))
            ),
        )
        .unwrap();

        let child_dir = chats.join("tabnine-large");
        fs::create_dir_all(&child_dir).unwrap();
        fs::write(
            child_dir.join("child.jsonl"),
            jsonl(json!({
                "sessionId": "tabnine-child",
                "startTime": "2026-07-14T13:00:00Z",
                "id": "child-header",
                "type": "user",
                "content": "child real message"
            })),
        )
        .unwrap();

        let imported_at = "2026-07-14T18:00:00Z".parse().unwrap();
        let options = TabnineCliImportOptions {
            machine_id: "tabnine-stream-test".to_owned(),
            source_path: None,
            imported_at,
            history_record_id: None,
        };
        let context = ProviderAdapterContext {
            machine_id: options.machine_id.clone(),
            source_path: Some(root.clone()),
            source_root: None,
            imported_at,
        };

        let mut legacy_store = Store::open(temp.path().join("legacy.sqlite")).unwrap();
        let legacy_normalization = normalize_jsonl_tree(
            &root,
            &context,
            CaptureProvider::Tabnine,
            TABNINE_CLI_SOURCE_FORMAT,
        )
        .unwrap();
        let large_source_root = legacy_normalization
            .captures
            .iter()
            .find(|(_, capture)| capture.session.provider_session_id == "tabnine-large")
            .and_then(|(_, capture)| capture.source.source_root.clone())
            .expect("large Tabnine capture should have a source root");
        let legacy_summary = import_normalized_provider_captures(
            &mut legacy_store,
            legacy_normalization,
            NormalizedProviderImportOptions {
                history_record_id: None,
                persist_cursors: true,
                wrap_transaction: true,
                fast_event_inserts: true,
            },
        )
        .unwrap();

        let mut streamed_store = Store::open(temp.path().join("streamed.sqlite")).unwrap();
        let (streamed_summary, metrics) =
            import_tabnine_cli_history_streamed(&root, &mut streamed_store, options).unwrap();

        assert_eq!(metrics.path_inventory_entries, 3);
        assert_eq!(metrics.max_path_inventory_batch, 3);
        assert!(metrics.normalization_batches > 5);
        assert_eq!(metrics.max_batch_captures, 64);
        assert!(metrics.max_batch_files_touched <= 1);
        assert_eq!(metrics.normalization_captures, ROWS + 2);
        assert_eq!(metrics.normalization_files_touched, 1);
        assert!((1..=64).contains(&metrics.max_transaction_units));
        assert!(metrics.max_transaction_bytes > 0);
        assert!(metrics.max_transaction_bytes <= 8 * 1024 * 1024);
        assert_eq!(metrics.max_pending_cursors, 1);
        assert!(metrics.max_cache_entries > 0);
        assert!(metrics.max_cache_entries <= 32);
        assert_eq!(streamed_summary, legacy_summary);
        assert_eq!(streamed_summary.failed, 1);
        assert_eq!(streamed_summary.failures.len(), 1);
        assert!(streamed_summary.failures[0]
            .error
            .contains("malformed JSONL"));
        assert_eq!(streamed_summary.skipped_sessions, 1);
        assert_eq!(streamed_summary.skipped_events, 2);
        assert_eq!(streamed_summary.skipped, 2);
        assert_eq!(streamed_summary.imported_edges, 1);

        let legacy = legacy_store.export_archive().unwrap();
        let streamed = streamed_store.export_archive().unwrap();
        let session_projection = |sessions: &[ctx_history_core::Session]| {
            sessions
                .iter()
                .map(|session| {
                    (
                        session.id,
                        session.external_session_id.clone(),
                        session.parent_session_id,
                        session.started_at,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            session_projection(&streamed.sessions),
            session_projection(&legacy.sessions)
        );
        assert_eq!(streamed.sessions.len(), 2);
        assert!(streamed.sessions.iter().all(|session| {
            session.external_session_id.as_deref() != Some("tabnine-metadata-only")
        }));
        let child = streamed
            .sessions
            .iter()
            .find(|session| session.external_session_id.as_deref() == Some("tabnine-child"))
            .unwrap();
        let mut edge_ids = vec![provider_edge_uuid(
            CaptureProvider::Tabnine,
            "tabnine-child",
            "parent_child",
        )];
        let child_source = streamed_store
            .get_capture_source(child.capture_source_id.unwrap())
            .unwrap();
        if let Some(source_identity) = child_source.descriptor.source_identity.as_deref() {
            edge_ids.push(provider_source_edge_uuid(
                source_identity,
                "tabnine-child",
                "parent_child",
            ));
        }
        let edge_id = edge_ids
            .into_iter()
            .find(|edge_id| streamed_store.session_edge_exists(*edge_id).unwrap())
            .expect("streamed child session should retain its parent edge");
        assert!(legacy_store.session_edge_exists(edge_id).unwrap());
        let event_projection = |events: &[ctx_history_core::Event]| {
            events
                .iter()
                .map(|event| {
                    (
                        event.id,
                        event.seq,
                        event.session_id,
                        event.event_type,
                        event.role,
                        event.occurred_at,
                        event.payload.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            event_projection(&streamed.events),
            event_projection(&legacy.events)
        );
        let file_projection = |files: &[ctx_history_core::FileTouched]| {
            files
                .iter()
                .map(|file| {
                    (
                        file.id,
                        file.event_id,
                        file.source_id,
                        file.path.clone(),
                        file.change_kind,
                        file.old_path.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            file_projection(&streamed.files_touched),
            file_projection(&legacy.files_touched)
        );
        assert_eq!(streamed.files_touched.len(), 1);
        assert_eq!(streamed.files_touched[0].path, "src/tabnine.txt");
        assert_eq!(
            streamed.files_touched[0].change_kind,
            Some(FileChangeKind::Created)
        );

        let cursor_stream = provider_source_cursor_stream(
            CaptureProvider::Tabnine,
            TABNINE_CLI_SOURCE_FORMAT,
            Some(&large_source_root),
        );
        let legacy_cursor = legacy_store
            .get_sync_cursor(None, "tabnine-stream-test", &cursor_stream)
            .unwrap();
        let streamed_cursor = streamed_store
            .get_sync_cursor(None, "tabnine-stream-test", &cursor_stream)
            .unwrap();
        assert_eq!(streamed_cursor, legacy_cursor);
        // Importer policy withholds the cursor after any rejected row. The malformed
        // complete row above therefore makes the exact expected cursor state absent.
        assert!(streamed_cursor.is_none());

        let clean_root = temp.path().join("tabnine-clean/agent");
        let clean_chats = clean_root.join("tmp/project/chats");
        fs::create_dir_all(&clean_chats).unwrap();
        let clean_path = clean_chats.join("clean.jsonl");
        fs::write(
            &clean_path,
            format!(
                "{}{}",
                jsonl(json!({
                    "sessionId": "tabnine-clean",
                    "startTime": "2026-07-14T14:00:00Z",
                    "id": "clean-header",
                    "type": "user",
                    "content": "clean first message"
                })),
                jsonl(json!({
                    "id": "clean-assistant",
                    "timestamp": "2026-07-14T14:00:01Z",
                    "type": "tabnine",
                    "content": "clean response"
                }))
            ),
        )
        .unwrap();
        let clean_machine_id = "tabnine-clean-cursor";
        let clean_context = ProviderAdapterContext {
            machine_id: clean_machine_id.to_owned(),
            source_path: Some(clean_root.clone()),
            source_root: None,
            imported_at,
        };
        let clean_normalization = normalize_jsonl_tree(
            &clean_root,
            &clean_context,
            CaptureProvider::Tabnine,
            TABNINE_CLI_SOURCE_FORMAT,
        )
        .unwrap();
        let clean_source_root = clean_normalization
            .captures
            .last()
            .and_then(|(_, capture)| capture.source.source_root.clone())
            .expect("clean Tabnine capture should have a source root");
        let mut clean_legacy_store = Store::open(temp.path().join("clean-legacy.sqlite")).unwrap();
        let clean_legacy_summary = import_normalized_provider_captures(
            &mut clean_legacy_store,
            clean_normalization,
            NormalizedProviderImportOptions {
                history_record_id: None,
                persist_cursors: true,
                wrap_transaction: true,
                fast_event_inserts: true,
            },
        )
        .unwrap();
        let mut clean_streamed_store =
            Store::open(temp.path().join("clean-streamed.sqlite")).unwrap();
        let clean_streamed_summary = import_tabnine_cli_history(
            &clean_root,
            &mut clean_streamed_store,
            TabnineCliImportOptions {
                machine_id: clean_machine_id.to_owned(),
                source_path: None,
                imported_at,
                history_record_id: None,
            },
        )
        .unwrap();
        assert_eq!(clean_streamed_summary, clean_legacy_summary);
        assert_eq!(clean_streamed_summary.failed, 0);
        let clean_cursor_stream = provider_source_cursor_stream(
            CaptureProvider::Tabnine,
            TABNINE_CLI_SOURCE_FORMAT,
            Some(&clean_source_root),
        );
        let clean_legacy_cursor = clean_legacy_store
            .get_sync_cursor(None, clean_machine_id, &clean_cursor_stream)
            .unwrap();
        let clean_streamed_cursor = clean_streamed_store
            .get_sync_cursor(None, clean_machine_id, &clean_cursor_stream)
            .unwrap();
        assert_eq!(clean_streamed_cursor, clean_legacy_cursor);
        assert!(clean_streamed_cursor.is_some());
    }

    #[test]
    fn tabnine_production_tree_bounds_large_path_inventory_in_memory() {
        const PATHS: usize = 513;

        let temp = tempdir().unwrap();
        let root = temp.path().join("tabnine/agent");
        let chats = root.join("tmp/project/chats");
        fs::create_dir_all(&chats).unwrap();
        for index in (0..PATHS).rev() {
            fs::write(
                chats.join(format!("session-{index:04}.jsonl")),
                jsonl(json!({
                    "sessionId": format!("tabnine-path-{index:04}"),
                    "startTime": "2026-07-14T10:00:00Z",
                    "id": format!("path-header-{index:04}"),
                    "type": "user",
                    "content": "real message"
                })),
            )
            .unwrap();
        }

        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let (summary, metrics) = import_tabnine_cli_history_streamed(
            &root,
            &mut store,
            TabnineCliImportOptions {
                machine_id: "tabnine-path-inventory-test".to_owned(),
                source_path: None,
                imported_at: "2026-07-14T18:00:00Z".parse().unwrap(),
                history_record_id: None,
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.imported_sessions, PATHS);
        assert_eq!(metrics.path_inventory_entries, PATHS);
        assert_eq!(metrics.max_path_inventory_batch, 128);
    }
}
