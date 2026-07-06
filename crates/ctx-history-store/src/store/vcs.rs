#[allow(unused_imports)]
use super::*;

pub(crate) fn vcs_change_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VcsChange> {
    Ok(VcsChange {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        vcs_workspace_id: parse_uuid(row.get::<_, String>(1)?)?,
        kind: parse_text_enum::<ctx_history_core::VcsChangeKind>(row.get::<_, String>(2)?)?,
        change_id: row.get(3)?,
        parent_change_ids: serde_json::from_str(&row.get::<_, String>(4)?)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        branch_or_bookmark: row.get(5)?,
        tree_hash: row.get(6)?,
        author_time: optional_ms_to_time(row.get(7)?)?,
        confidence: parse_text_enum::<ctx_history_core::Confidence>(row.get::<_, String>(8)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(9)?)?,
            updated_at: ms_to_time(row.get(10)?)?,
        },
        source_id: parse_optional_uuid(row.get(11)?)?,
        sync: sync_metadata_from_row(row, 12, 13, 14, 15, 16, 17)?,
    })
}
