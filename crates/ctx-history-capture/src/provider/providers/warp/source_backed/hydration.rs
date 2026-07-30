use std::collections::{BTreeMap, BTreeSet};

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, LocatorRevisionPolicy, NativeRecordCoordinate, SourceKey,
    SourceRecordLocator, TypedKey,
};
use rusqlite::{params_from_iter, Connection};

use super::super::{schema::WarpSqliteSchema, warp_message_content_at};
#[cfg(test)]
use super::update_warp_source_backed_work;
use super::{
    digest_bytes, warp_source_key, RetainedWarpDirectory, WarpSourceBackedErrorV0,
    WarpSourceBackedResultV0, WarpSourceSelectionV0, WARP_TASK_MESSAGE_RELATION,
};
use crate::{
    complete_content::sqlite::sqlite_logical_record_digest, native_source::NativeSqliteValue,
    provider::source_backed::hydration_failure,
};

const HYDRATION_NATIVE_KEY_BATCH: usize = 256;

pub(super) fn hydrate_warp_group(
    selection: &WarpSourceSelectionV0,
    request: &BatchHydrationRequest,
) -> Result<BatchHydrationResult, HydrationFailure> {
    if request.events().is_empty() {
        return BatchHydrationResult::new(Vec::new())
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error));
    }
    let source = warp_source_key(selection).map_err(warp_hydration_error)?;
    let coordinates = request
        .events()
        .iter()
        .map(|event| {
            validate_warp_locator(&source, event.locator()).map(|(task_id, message_ordinal)| {
                WarpHydrationCoordinate {
                    task_id,
                    message_ordinal,
                    record_digest: *event.locator().record_digest(),
                }
            })
        })
        .collect::<WarpSourceBackedResultV0<Vec<_>>>()
        .map_err(warp_hydration_error)?;
    let retained = RetainedWarpDirectory::open(&selection.data_root, selection.path())
        .map_err(warp_hydration_error)?;
    let snapshot = retained
        .open_snapshot()
        .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))?
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "Warp selected database is missing",
            )
        })?;
    let terminal_revalidate = snapshot.terminal_revalidator();
    let hydrated = (|| {
        let connection = snapshot
            .connection()
            .map_err(warp_snapshot_hydration_error)?;
        WarpSqliteSchema::detect(connection).map_err(warp_hydration_error)?;
        let keys = coordinates
            .iter()
            .map(|coordinate| coordinate.task_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let rows = load_task_value_batches(connection, &keys).map_err(warp_hydration_error)?;
        request
            .events()
            .iter()
            .zip(&coordinates)
            .map(|(event, coordinate)| {
                let values = rows.get(&coordinate.task_id).ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::MissingRecord,
                        "Warp task row is missing",
                    )
                })?;
                let digest = digest_bytes(sqlite_logical_record_digest(values).as_str())
                    .map_err(warp_hydration_error)?;
                if digest != coordinate.record_digest {
                    return Err(hydration_failure(
                        HydrationFailureKind::StaleRecordEvidence,
                        "Warp task row digest changed",
                    ));
                }
                let [
                    NativeSqliteValue::Text(conversation_id),
                    NativeSqliteValue::Text(task_id),
                    NativeSqliteValue::Blob(task),
                    _,
                ] = values.as_slice()
                else {
                    return Err(hydration_failure(
                        HydrationFailureKind::StaleRecordEvidence,
                        "Warp task row changed storage class",
                    ));
                };
                let content = warp_message_content_at(
                    task,
                    conversation_id,
                    task_id,
                    usize::try_from(coordinate.message_ordinal).map_err(|_| {
                        hydration_failure(
                            HydrationFailureKind::InvalidLocator,
                            "Warp message ordinal exceeds usize",
                        )
                    })?,
                )
                .map_err(warp_hydration_error)?
                .ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::MissingRecord,
                        "Warp task message is missing",
                    )
                })?;
                Ok(HydratedProviderRecord {
                    event_id: event.event_id(),
                    provider_bytes: content.text.into_bytes(),
                })
            })
            .collect::<Result<Vec<_>, HydrationFailure>>()
    })();
    snapshot.finish().map_err(warp_snapshot_hydration_error)?;
    retained
        .revalidate()
        .map_err(|error| hydration_failure(HydrationFailureKind::StaleSourceEvidence, error))?;
    terminal_revalidate().map_err(warp_snapshot_hydration_error)?;
    #[cfg(test)]
    retained.record_snapshot_work();
    let result = BatchHydrationResult::new(hydrated?)
        .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
    result.validate_for_request(request)?;
    Ok(result)
}

struct WarpHydrationCoordinate {
    task_id: String,
    message_ordinal: u64,
    record_digest: [u8; 32],
}

fn validate_warp_locator(
    source: &SourceKey,
    locator: &SourceRecordLocator,
) -> WarpSourceBackedResultV0<(String, u64)> {
    locator.validate_contract()?;
    if !source.exact_descriptor_eq(locator.source())
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    }
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    };
    let TypedKey::Composite(parts) = primary_key else {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    };
    let [TypedKey::Utf8(task_id), TypedKey::U64(message_ordinal)] = parts.as_slice() else {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    };
    if logical_relation != WARP_TASK_MESSAGE_RELATION
        || row_version.as_ref() != Some(&TypedKey::Bytes(locator.record_digest().to_vec()))
    {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    }
    Ok((task_id.clone(), *message_ordinal))
}

fn load_task_value_batches(
    connection: &Connection,
    keys: &[String],
) -> WarpSourceBackedResultV0<BTreeMap<String, Vec<NativeSqliteValue>>> {
    let mut loaded = BTreeMap::new();
    for batch in keys.chunks(HYDRATION_NATIVE_KEY_BATCH) {
        let placeholders = std::iter::repeat_n("?", batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "select cast(conversation_id as text), cast(task_id as text), task, \
                    cast(last_modified_at as text) \
             from agent_tasks where task_id in ({placeholders}) order by task_id collate binary"
        );
        let mut statement = connection.prepare(&sql)?;
        #[cfg(test)]
        update_warp_source_backed_work(|work| {
            work.hydration_queries = work.hydration_queries.saturating_add(1);
        });
        let rows = statement.query_map(params_from_iter(batch.iter()), |row| {
            let task_id = row.get::<_, String>(1)?;
            Ok((
                task_id.clone(),
                vec![
                    NativeSqliteValue::Text(row.get(0)?),
                    NativeSqliteValue::Text(task_id),
                    NativeSqliteValue::Blob(row.get(2)?),
                    NativeSqliteValue::Text(row.get(3)?),
                ],
            ))
        })?;
        for row in rows {
            let (task_id, values) = row?;
            if loaded.insert(task_id, values).is_some() {
                return Err(WarpSourceBackedErrorV0::StaleTaskRow);
            }
        }
    }
    Ok(loaded)
}

fn warp_snapshot_hydration_error(error: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::StaleSourceEvidence, error)
}

fn warp_hydration_error(error: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
}
