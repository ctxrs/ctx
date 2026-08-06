use anyhow::{anyhow, Result};
use ctx_history_index::{CopiedEventLineage, CopiedEventLineagePolicy, VerifiedIndex};
use serde_json::{json, Map, Value};
use uuid::Uuid;

pub(super) const COPIED_LINEAGE_MAX_BYTES: usize = 64 * 1024;

pub(super) fn copied_lineage_value(
    index: &VerifiedIndex,
    selected_event_id: Uuid,
    policy: CopiedEventLineagePolicy,
) -> Result<Value> {
    let lineage = index
        .copied_event_lineage(selected_event_id, policy)?
        .ok_or_else(|| {
            anyhow!("event {selected_event_id} disappeared from the pinned Core generation")
        })?;
    copied_lineage_read_model(&lineage)
}

fn copied_lineage_read_model(lineage: &CopiedEventLineage) -> Result<Value> {
    let relationship_counts = lineage
        .relationship_counts
        .iter()
        .map(|count| {
            (
                count.session_relationship.as_str().to_owned(),
                Value::from(count.observed_count),
            )
        })
        .collect::<Map<_, _>>();
    let occurrences = lineage
        .occurrences
        .iter()
        .map(|occurrence| {
            json!({
                "ctx_event_id": occurrence.event_id.as_uuid(),
                "ctx_session_id": occurrence.session_id.as_uuid(),
                "copied_from_ctx_event_id": occurrence.copied_from_event_id.as_uuid(),
                "copied_from_ctx_session_id": occurrence.copied_from_session_id.as_uuid(),
                "parent_ctx_session_id": occurrence.parent_session_id.map(|id| id.as_uuid()),
                "root_ctx_session_id": occurrence.root_session_id.as_uuid(),
                "session_relationship": occurrence.session_relationship,
                "depth": occurrence.depth,
            })
        })
        .collect::<Vec<_>>();
    let value = json!({
        "schema_version": 1,
        "observed_count": lineage.observed_count,
        "returned": lineage.returned,
        "occurrences": occurrences,
        "relationship_counts": relationship_counts,
        "truncated": lineage.truncated,
    });
    let encoded_bytes = serde_json::to_vec(&value)?.len();
    if encoded_bytes > COPIED_LINEAGE_MAX_BYTES {
        return Err(anyhow!(
            "copied lineage for event {} requires {encoded_bytes} bytes; the maximum is {COPIED_LINEAGE_MAX_BYTES}",
            lineage.selected_event_id
        ));
    }
    Ok(value)
}
