use std::path::Path;

use ctx_history_store::Store;
use serde_json::Value;

use crate::common::path_inventory::SortedJsonlPathInventory;
use crate::provider::adapter::{
    AuggieSessionJsonAdapter, ClineTaskJsonAdapter, CodeBuddyHistoryJsonAdapter,
    CrushSqliteAdapter, GooseSessionsSqliteAdapter, HermesSqliteAdapter, JunieSessionEventsAdapter,
    OpenClawJsonlAdapter, ProviderCaptureAdapter, RooTaskJsonAdapter,
};
use crate::provider::importer::{
    import_native_jsonl_tree, import_normalized_provider_capture_stream,
    import_normalized_provider_capture_stream_with_metrics, import_normalized_provider_captures,
    NativeJsonlTreeImport, ProviderNormalizationStreamMetrics,
};
use crate::provider::providers::claude::{
    scan_claude_projects_jsonl_reader, stream_claude_projects_jsonl_reader,
};
use crate::provider::providers::native_jsonl::native_jsonl_missing_reason;
use crate::provider::providers::pi::{
    scan_pi_session_jsonl_reader, stream_pi_session_jsonl_reader,
};
use crate::provider::providers::trae::normalize_trae_history;
use crate::{
    AuggieImportOptions, CaptureError, ClaudeProjectsImportOptions, ClineTaskJsonImportOptions,
    CodeBuddyImportOptions, CrushSqliteImportOptions, GooseSessionsSqliteImportOptions,
    HermesSqliteImportOptions, JunieImportOptions, NormalizedProviderImportOptions,
    OpenClawImportOptions, PiSessionImportOptions, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportSummary, ProviderJsonlReader, ProviderNormalizationResult, Result,
    RooTaskJsonImportOptions, TraeImportOptions,
};

pub fn import_pi_session_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: PiSessionImportOptions,
) -> Result<ProviderImportSummary> {
    import_pi_session_jsonl_streamed(path.as_ref(), store, options).map(|(summary, _)| summary)
}

fn import_pi_session_jsonl_streamed(
    path: &Path,
    store: &mut Store,
    options: PiSessionImportOptions,
) -> Result<(ProviderImportSummary, ProviderNormalizationStreamMetrics)> {
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let base_context = ProviderAdapterContext {
        machine_id: options.machine_id,
        source_path: Some(source_path),
        source_root: None,
        imported_at: options.imported_at,
    };
    let path_inventory = SortedJsonlPathInventory::build(path, |_| true)?;
    if path_inventory.metrics().paths == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(ctx_history_core::CaptureProvider::Pi),
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
            path_inventory.for_each(|file_path| {
                let mut context = base_context.clone();
                if path != file_path {
                    context.source_path = Some(file_path.clone());
                }
                let mut reader = ProviderJsonlReader::open_replacement(&file_path)?;
                let scan = scan_pi_session_jsonl_reader(&mut reader, &context, None, false)?
                    .expect("whole-replacement Pi parsing cannot request replacement");
                stream_pi_session_jsonl_reader(&mut reader, &context, None, |mut batch| {
                    if !scan.has_real_message {
                        batch.captures.clear();
                        batch.files_touched.clear();
                    } else {
                        batch = scan.admission.filter_batch(batch)?;
                    }
                    emit(batch)
                })?;
                if scan.has_real_message {
                    let skipped_sessions = scan.admission.rejected_session_count()?;
                    if skipped_sessions > 0 {
                        let mut rejected = ProviderNormalizationResult::default();
                        rejected.summary.skipped_sessions = skipped_sessions;
                        emit(rejected)?;
                    }
                } else if scan.failed == 0 {
                    let mut rejected = ProviderNormalizationResult::default();
                    rejected.summary.failed = 1;
                    rejected.summary.sample_failure(ProviderImportFailure {
                        line: scan.last_line_number,
                        error: "pi session JSONL contained no real message content".to_owned(),
                    });
                    emit(rejected)?;
                }
                Ok(())
            })?;
            Ok(())
        },
    )?;
    let path_metrics = path_inventory.metrics();
    metrics.path_inventory_entries = path_metrics.paths;
    metrics.max_path_inventory_batch = path_metrics.max_in_memory_batch;
    Ok((summary, metrics))
}

pub fn import_claude_projects_jsonl_tree(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ClaudeProjectsImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
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
    let path_inventory = SortedJsonlPathInventory::build(path, |_| true)?;
    if path_inventory.metrics().paths == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Claude Code project JSONL transcripts found",
        });
    }

    import_normalized_provider_capture_stream(
        store,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
        |emit| {
            let mut any_real_message = false;
            let mut any_capture = false;
            let mut normalization_failures = 0usize;
            path_inventory.for_each(|file_path| {
                let mut reader = ProviderJsonlReader::open_replacement(&file_path)?;
                let scan = scan_claude_projects_jsonl_reader(&mut reader, &context)?;
                any_real_message |= scan.has_real_message;
                any_capture |= scan.valid_records > 0;
                normalization_failures += scan.failed;
                let no_header = scan.header.is_none();
                let header = scan.header.unwrap_or(Value::Null);
                let started_at = scan.earliest_started_at.unwrap_or(context.imported_at);
                let mut saw_capture = false;
                stream_claude_projects_jsonl_reader(
                    &file_path,
                    &mut reader,
                    &context,
                    &header,
                    started_at,
                    |mut batch| {
                        saw_capture |= !batch.captures.is_empty();
                        if !scan.has_real_message {
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
                if !scan.has_real_message && saw_capture {
                    let mut rejected = ProviderNormalizationResult::default();
                    rejected.summary.skipped_sessions = 1;
                    emit(rejected)?;
                } else if no_header && scan.failed == 0 {
                    let mut empty = ProviderNormalizationResult::default();
                    empty.summary.skipped = 1;
                    empty.summary.skipped_sessions = 1;
                    emit(empty)?;
                }
                Ok(())
            })?;
            if !any_real_message && normalization_failures == 0 {
                let mut rejected = ProviderNormalizationResult::default();
                rejected.summary.failed = 1;
                rejected.summary.sample_failure(ProviderImportFailure {
                    line: 0,
                    error: if any_capture {
                        "provider source contained no real conversation message".to_owned()
                    } else {
                        "Claude Code project JSONL contained no real conversation messages"
                            .to_owned()
                    },
                });
                emit(rejected)?;
            }
            Ok(())
        },
    )
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

pub fn import_roo_task_json_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: RooTaskJsonImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = RooTaskJsonAdapter.normalize_path(
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

pub fn import_codebuddy_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CodeBuddyImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = CodeBuddyHistoryJsonAdapter.normalize_path(
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

pub fn import_trae_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: TraeImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = normalize_trae_history(
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

pub fn import_crush_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CrushSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = CrushSqliteAdapter.normalize_path(
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

pub fn import_goose_sessions_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: GooseSessionsSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = GooseSessionsSqliteAdapter.normalize_path(
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

pub fn import_openclaw_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: OpenClawImportOptions,
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
        OpenClawJsonlAdapter,
    )
}

pub fn import_hermes_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: HermesSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = HermesSqliteAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
    )?;
    let import_options = NormalizedProviderImportOptions {
        history_record_id: options.history_record_id,
        persist_cursors: true,
        wrap_transaction: true,
        fast_event_inserts: true,
    };
    import_normalized_provider_captures(store, normalization, import_options)
}

pub fn import_auggie_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: AuggieImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = AuggieSessionJsonAdapter.normalize_path(
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

pub fn import_junie_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: JunieImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = JunieSessionEventsAdapter.normalize_path(
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

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{json, Value};
    use tempfile::tempdir;

    use super::*;
    use crate::provider::importer::provider_source_cursor_stream;
    use crate::provider::providers::pi::normalize_pi_session_jsonl_path;
    use crate::summaries::MAX_PROVIDER_IMPORT_FAILURE_SAMPLES;

    fn jsonl(value: Value) -> String {
        format!("{}\n", serde_json::to_string(&value).unwrap())
    }

    #[test]
    fn pi_production_streaming_filters_mixed_sessions_like_legacy_import() {
        let temp = tempdir().unwrap();
        for metadata_first in [true, false] {
            let case = if metadata_first {
                "metadata-first"
            } else {
                "real-first"
            };
            let path = temp.path().join(format!("pi/{case}/mixed.jsonl"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let metadata_session = format!(
                "{}{}",
                jsonl(json!({
                    "type": "session",
                    "id": "pi-metadata-only",
                    "timestamp": "2026-07-14T12:00:00Z"
                })),
                jsonl(json!({
                    "type": "model_change",
                    "id": "pi-metadata",
                    "timestamp": "2026-07-14T12:00:01Z",
                    "provider": "example",
                    "modelId": "notice-only"
                }))
            );
            let real_session = format!(
                "{}{}",
                jsonl(json!({
                    "type": "session",
                    "id": "pi-real",
                    "timestamp": "2026-07-14T12:00:02Z"
                })),
                jsonl(json!({
                    "type": "message",
                    "id": "pi-real-message",
                    "timestamp": "2026-07-14T12:00:03Z",
                    "message": {"role": "user", "content": "admitted"}
                }))
            );
            let contents = if metadata_first {
                format!("{metadata_session}{real_session}")
            } else {
                format!("{real_session}{metadata_session}")
            };
            fs::write(&path, contents).unwrap();

            let imported_at = "2026-07-14T18:00:00Z".parse().unwrap();
            let machine_id = format!("pi-stream-test-{case}");
            let options = PiSessionImportOptions {
                machine_id: machine_id.clone(),
                source_path: None,
                imported_at,
                history_record_id: None,
            };
            let context = ProviderAdapterContext {
                machine_id: machine_id.clone(),
                source_path: Some(path.clone()),
                source_root: None,
                imported_at,
            };

            let mut legacy_store =
                Store::open(temp.path().join(format!("legacy-{case}.sqlite"))).unwrap();
            let legacy_normalization = normalize_pi_session_jsonl_path(&path, &context).unwrap();
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

            let mut streamed_store =
                Store::open(temp.path().join(format!("streamed-{case}.sqlite"))).unwrap();
            let streamed_summary =
                import_pi_session_jsonl(&path, &mut streamed_store, options).unwrap();
            assert_eq!(streamed_summary, legacy_summary, "{case}");
            assert_eq!(streamed_summary.failed, 0, "{case}");
            assert_eq!(streamed_summary.skipped_sessions, 1, "{case}");
            assert_eq!(streamed_summary.skipped_events, 1, "{case}");
            assert_eq!(streamed_summary.imported_edges, 0, "{case}");
            assert_eq!(streamed_summary.skipped_edges, 0, "{case}");

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
                session_projection(&legacy.sessions),
                "{case}"
            );
            assert_eq!(streamed.sessions.len(), 1, "{case}");
            assert_eq!(
                streamed.sessions[0].external_session_id.as_deref(),
                Some("pi-real"),
                "{case}"
            );
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
                event_projection(&legacy.events),
                "{case}"
            );
            assert_eq!(streamed.events.len(), 1, "{case}");
            assert_eq!(streamed.files_touched, legacy.files_touched, "{case}");
            assert!(streamed.files_touched.is_empty(), "{case}");

            let source_root = path.display().to_string();
            let cursor_stream = provider_source_cursor_stream(
                ctx_history_core::CaptureProvider::Pi,
                "pi_session_jsonl",
                Some(&source_root),
            );
            let legacy_cursor = legacy_store
                .get_sync_cursor(None, &machine_id, &cursor_stream)
                .unwrap();
            let streamed_cursor = streamed_store
                .get_sync_cursor(None, &machine_id, &cursor_stream)
                .unwrap();
            assert_eq!(streamed_cursor, legacy_cursor, "{case}");
            assert!(streamed_cursor.is_some(), "{case}");
        }
    }

    #[test]
    fn pi_tree_import_bounds_paths_caches_cursors_and_failure_samples() {
        const SESSIONS: usize = 257;
        const MALFORMED: usize = MAX_PROVIDER_IMPORT_FAILURE_SAMPLES * 3;

        let temp = tempdir().unwrap();
        let root = temp.path().join("pi/sessions");
        fs::create_dir_all(&root).unwrap();
        for index in (0..SESSIONS).rev() {
            fs::write(
                root.join(format!("session-{index:04}.jsonl")),
                format!(
                    "{}{}",
                    jsonl(json!({
                        "type": "session",
                        "id": format!("pi-many-{index:04}"),
                        "timestamp": "2026-07-14T12:00:00Z"
                    })),
                    jsonl(json!({
                        "type": "message",
                        "id": format!("pi-message-{index:04}"),
                        "timestamp": "2026-07-14T12:00:01Z",
                        "message": {"role": "user", "content": "bounded many-session import"}
                    }))
                ),
            )
            .unwrap();
        }
        let malformed_path = root.join("zzzz-malformed.jsonl");
        fs::write(&malformed_path, "not-json\n".repeat(MALFORMED)).unwrap();

        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let (summary, metrics) = import_pi_session_jsonl_streamed(
            &root,
            &mut store,
            PiSessionImportOptions {
                machine_id: "pi-many-session-test".to_owned(),
                source_path: None,
                imported_at: "2026-07-14T18:00:00Z".parse().unwrap(),
                history_record_id: None,
            },
        )
        .unwrap();

        assert_eq!(summary.imported_sessions, SESSIONS);
        assert_eq!(summary.failed, MALFORMED);
        assert_eq!(summary.failures.len(), MAX_PROVIDER_IMPORT_FAILURE_SAMPLES);
        assert_eq!(summary.failures.first().unwrap().line, 1);
        assert_eq!(
            summary.failures.last().unwrap().line,
            MAX_PROVIDER_IMPORT_FAILURE_SAMPLES
        );
        assert_eq!(metrics.path_inventory_entries, SESSIONS + 1);
        assert_eq!(metrics.max_path_inventory_batch, 128);
        assert!(metrics.max_pending_cursors <= 2);
        assert!(metrics.max_cache_entries <= 32);
    }

    #[test]
    fn pi_large_session_identity_reimport_uses_bounded_store_windows() {
        const EVENTS: usize = 257;

        let temp = tempdir().unwrap();
        let path = temp.path().join("pi/large.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut contents = jsonl(json!({
            "type": "session",
            "id": "pi-large-identity",
            "timestamp": "2026-07-14T12:00:00Z"
        }));
        for index in 0..EVENTS {
            contents.push_str(&jsonl(json!({
                "type": "message",
                "id": format!("pi-large-message-{index:04}"),
                "timestamp": "2026-07-14T12:00:01Z",
                "message": {"role": "assistant", "content": format!("large row {index}")}
            })));
        }
        fs::write(&path, contents).unwrap();

        let options = || PiSessionImportOptions {
            machine_id: "pi-large-identity-test".to_owned(),
            source_path: None,
            imported_at: "2026-07-14T18:00:00Z".parse().unwrap(),
            history_record_id: None,
        };
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let first = import_pi_session_jsonl_streamed(&path, &mut store, options())
            .unwrap()
            .0;
        assert_eq!(first.imported_events, EVENTS);

        let (second, metrics) =
            import_pi_session_jsonl_streamed(&path, &mut store, options()).unwrap();
        assert_eq!(second.failed, 0, "{:?}", second.failures);
        assert_eq!(second.skipped_events, EVENTS);
        assert_eq!(metrics.max_pi_identity_load_batch, 128);
        assert!(metrics.max_cache_entries <= 32);
        assert!(metrics.max_pending_cursors <= 64);
    }
}
