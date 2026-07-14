use std::collections::BTreeMap;

use ctx_history_core::{
    CaptureProvider, SessionEdge, SessionEdgeType, SyncCursor, CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
};
use ctx_history_store::Store;
use serde_json::json;
use uuid::Uuid;

use crate::provider::importer::{
    provider_edge_uuid, provider_scoped_source_uuid, provider_session_exists_cached,
    provider_session_uuid, provider_sync_metadata, timestamps, ProviderImportTransaction,
    ProviderImportTransactionStep,
};
use crate::{stable_capture_uuid, ProviderImportSummary, Result};

use super::{
    custom_history_key, push_provider_import_failure, CustomHistoryJsonlV1EdgeImport,
    CustomHistoryJsonlV1SourceCursorImport,
};

pub(crate) fn import_custom_history_edges(
    store: &mut Store,
    edges: &[(usize, CustomHistoryJsonlV1EdgeImport)],
    history_record_id: Option<Uuid>,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    if edges.is_empty() {
        return Ok(());
    }

    let mut transaction = ProviderImportTransaction::begin_bounded(store, true)?;
    for (line_number, edge) in edges {
        let edge_bytes = custom_edge_estimated_bytes(edge);
        if transaction.prepare_unit(store, edge_bytes)? == ProviderImportTransactionStep::Halted {
            transaction.apply_maintenance_warning(summary);
            return Ok(());
        }
        let persist = (|| -> Result<()> {
            let edge_id = if edge.edge_type == SessionEdgeType::ParentChild {
                provider_edge_uuid(
                    CaptureProvider::Custom,
                    &edge.to_provider_session_id,
                    "parent_child",
                )
            } else {
                let key = custom_history_key(json!({
                    "schema": CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
                    "kind": "session_edge",
                    "provider_key": edge.provider_key,
                    "source_id": edge.source_id,
                    "from_provider_session_id": edge.from_provider_session_id,
                    "to_provider_session_id": edge.to_provider_session_id,
                    "edge_type": edge.edge_type.as_str(),
                    "edge_id": edge.edge_id,
                }));
                stable_capture_uuid(&key, "session-edge")
            };
            let from_session_id =
                provider_session_uuid(CaptureProvider::Custom, &edge.from_provider_session_id);
            let to_session_id =
                provider_session_uuid(CaptureProvider::Custom, &edge.to_provider_session_id);
            let source_id = provider_scoped_source_uuid(
                CaptureProvider::Custom,
                &edge.to_provider_session_id,
                &edge.source_format,
                edge.raw_source_path.as_deref(),
            );
            let mut exists_cache = BTreeMap::<Uuid, bool>::new();
            if !provider_session_exists_cached(store, from_session_id, &mut exists_cache)?
                || !provider_session_exists_cached(store, to_session_id, &mut exists_cache)?
            {
                push_provider_import_failure(
                    summary,
                    *line_number,
                    "edge endpoint session was not imported".to_owned(),
                );
                return Ok(());
            }
            let was_present = store.session_edge_exists(edge_id)?;
            let session_edge = SessionEdge {
                id: edge_id,
                from_session_id,
                to_session_id,
                edge_type: edge.edge_type,
                confidence: edge.confidence,
                source_id: Some(source_id),
                timestamps: timestamps(edge.occurred_at),
                sync: provider_sync_metadata(
                    edge.fidelity,
                    json!({
                        "provider_key": edge.provider_key,
                        "source_id": edge.source_id,
                        "history_record_id": history_record_id,
                        "metadata": edge.metadata,
                    }),
                ),
            };
            store.upsert_session_edge(&session_edge)?;
            if edge.edge_type == SessionEdgeType::ParentChild {
                let mut child = store.get_session(to_session_id)?;
                child.parent_session_id = Some(from_session_id);
                if child.root_session_id.is_none() {
                    child.root_session_id = Some(from_session_id);
                }
                store.upsert_session(&child)?;
            }
            if was_present {
                summary.skipped_edges += 1;
                summary.skipped += 1;
            } else {
                summary.imported_edges += 1;
                summary.imported += 1;
            }
            summary.accepted_content_records += 1;
            Ok(())
        })();
        if let Err(error) = persist {
            transaction.rollback(store);
            if transaction.record_interruption_after_commit(&error) {
                transaction.apply_maintenance_warning(summary);
                return Ok(());
            }
            return Err(error);
        }
        if transaction.record_unit(store, edge_bytes)? == ProviderImportTransactionStep::Halted {
            transaction.apply_maintenance_warning(summary);
            return Ok(());
        }
    }
    if let Err(error) = transaction.commit(store) {
        if transaction.record_interruption_after_commit(&error) {
            transaction.apply_maintenance_warning(summary);
            return Ok(());
        }
        return Err(error);
    }
    Ok(())
}

fn custom_edge_estimated_bytes(edge: &CustomHistoryJsonlV1EdgeImport) -> usize {
    edge.provider_key
        .len()
        .saturating_add(edge.source_id.len())
        .saturating_add(edge.source_format.len())
        .saturating_add(edge.raw_source_path.as_deref().map_or(0, str::len))
        .saturating_add(edge.from_provider_session_id.len())
        .saturating_add(edge.to_provider_session_id.len())
        .saturating_add(edge.edge_id.as_deref().map_or(0, str::len))
        .saturating_add(edge.metadata.to_string().len())
        .saturating_add(256)
}

pub(crate) fn import_custom_history_source_cursors(
    store: &mut Store,
    cursors: &[CustomHistoryJsonlV1SourceCursorImport],
) -> Result<()> {
    for cursor in cursors {
        store.upsert_sync_cursor(&SyncCursor {
            id: stable_capture_uuid(
                &format!(
                    "provider-cursor:{}:{}:{}",
                    CaptureProvider::Custom.as_str(),
                    cursor.machine_id,
                    cursor.checkpoint.stream
                ),
                "provider-sync-cursor",
            ),
            team_id: None,
            device_id: cursor.machine_id.clone(),
            stream: cursor.checkpoint.stream.clone(),
            cursor: cursor.checkpoint.cursor.clone(),
            last_synced_at: Some(cursor.checkpoint.observed_at),
            timestamps: timestamps(cursor.checkpoint.observed_at),
        })?;
    }
    Ok(())
}
