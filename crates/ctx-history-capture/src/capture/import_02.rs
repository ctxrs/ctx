#[allow(unused_imports)]
use super::*;

pub(crate) fn resolve_pending_provider_edges(
    store: &mut Store,
    summary: &mut ProviderImportSummary,
    caches: &mut ProviderImportCaches,
) -> Result<()> {
    let pending = std::mem::take(&mut caches.pending_edges);
    for (edge_id, edge) in pending {
        if caches.processed_edges.contains(&edge_id) {
            update_session_parent_if_needed(store, &edge, caches)?;
            continue;
        }
        if !provider_session_exists_cached(
            store,
            edge.parent_session_id,
            &mut caches.session_exists,
        )? {
            summary.skipped_edges += 1;
            summary.skipped += 1;
            continue;
        }
        let root_session_id = resolve_pending_root_session_id(store, &edge, caches)?;
        update_session_parent(store, &edge, root_session_id)?;
        caches.session_exists.insert(edge.session_id, true);

        let was_present = store.session_edge_exists(edge_id)?;
        let session_edge = SessionEdge {
            id: edge_id,
            from_session_id: edge.parent_session_id,
            to_session_id: edge.session_id,
            edge_type: SessionEdgeType::ParentChild,
            confidence: Confidence::Explicit,
            source_id: Some(edge.source_id),
            timestamps: timestamps(edge.imported_at),
            sync: provider_sync_metadata(
                edge.fidelity,
                json!({
                    "provider_session_id": edge.provider_session_id,
                    "parent_provider_session_id": edge.parent_provider_session_id,
                    "source_format": edge.source_format,
                    "fixture_line": edge.line_number,
                    "imported_at": edge.imported_at,
                    "deferred_edge_resolution": true,
                }),
            ),
        };
        store.upsert_session_edge(&session_edge)?;
        caches.processed_edges.insert(edge_id);
        if !was_present && caches.imported_edges.insert(edge_id) {
            summary.imported_edges += 1;
            summary.imported += 1;
        } else {
            summary.skipped_edges += 1;
            summary.skipped += 1;
        }
    }
    Ok(())
}
