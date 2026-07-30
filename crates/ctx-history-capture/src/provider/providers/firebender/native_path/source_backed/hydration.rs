use std::{collections::BTreeMap, path::PathBuf};

#[cfg(test)]
use std::cell::Cell;

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, ContentSourceResolver, EventHydrationRequest,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy,
};

use super::{
    decode_locator_coordinate,
    direct::{firebender_database_path_and_source, open_database_leaf, OpenDatabaseLeaf},
    load_exact_row,
};
use crate::provider::providers::firebender::firebender_message_text;
use crate::provider::providers::firebender::native_path::{
    firebender_raw_row_digest, validate_schema,
};

#[derive(Debug)]
pub(crate) struct FirebenderExactResolver {
    data_root: PathBuf,
    explicit_path: PathBuf,
    #[cfg(test)]
    snapshot_opens: Cell<u64>,
    #[cfg(test)]
    native_row_reads: Cell<u64>,
}

impl FirebenderExactResolver {
    pub(super) fn new(data_root: impl Into<PathBuf>, explicit_path: impl Into<PathBuf>) -> Self {
        Self {
            data_root: data_root.into(),
            explicit_path: explicit_path.into(),
            #[cfg(test)]
            snapshot_opens: Cell::new(0),
            #[cfg(test)]
            native_row_reads: Cell::new(0),
        }
    }

    #[cfg(test)]
    pub(crate) fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        ContentSourceResolver::hydrate_event(self, request)
    }

    pub(crate) fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        ContentSourceResolver::hydrate_batch(self, request)
    }

    #[cfg(test)]
    pub(crate) fn counters(&self) -> (u64, u64) {
        (self.snapshot_opens.get(), self.native_row_reads.get())
    }
}

impl ContentSourceResolver for FirebenderExactResolver {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let batch = BatchHydrationRequest::new(vec![request.clone()])
            .map_err(|error| invalid_locator(error.to_string()))?;
        self.hydrate_batch(&batch)?
            .into_records()
            .into_iter()
            .next()
            .ok_or_else(|| invalid_locator("Firebender event hydration returned no record"))
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        if request.is_empty() {
            return BatchHydrationResult::new(Vec::new())
                .map_err(|error| invalid_locator(error.to_string()));
        }
        let (database_path, source) = firebender_database_path_and_source(&self.explicit_path)
            .map_err(|error| unavailable(error.to_string()))?;
        let mut positions_by_row = BTreeMap::<i64, Vec<(usize, String, i64, u64)>>::new();
        for (position, event) in request.events().iter().enumerate() {
            event
                .validate_contract()
                .map_err(|error| invalid_locator(error.to_string()))?;
            let locator = event.locator();
            if !source.exact_descriptor_eq(locator.source())
                || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
                || locator.certified_source_revision_digest().is_some()
            {
                return Err(invalid_locator(
                    "locator does not belong to the selected Firebender source",
                ));
            }
            let (rowid, session_id, updated_at, message_index) = decode_locator_coordinate(locator)
                .map_err(|error| invalid_locator(error.to_string()))?;
            positions_by_row.entry(rowid).or_default().push((
                position,
                session_id,
                updated_at,
                message_index,
            ));
        }

        let snapshot = match open_database_leaf(&self.data_root, &database_path) {
            Ok(OpenDatabaseLeaf::Present(snapshot)) => *snapshot,
            Ok(OpenDatabaseLeaf::Missing(fence)) if fence.revalidate() => {
                return Err(hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "Firebender SQLite database leaf is absent from the retained provider root",
                ));
            }
            Ok(OpenDatabaseLeaf::Missing(_)) => {
                return Err(unavailable(
                    "Firebender provider root changed while database absence was checked",
                ));
            }
            Err(error) => return Err(unavailable(error.to_string())),
        };
        #[cfg(test)]
        self.snapshot_opens
            .set(self.snapshot_opens.get().saturating_add(1));
        let resolved = (|| {
            let connection = snapshot
                .connection()
                .map_err(|error| unavailable(error.to_string()))?;
            validate_schema(connection, &database_path)
                .map_err(|error| unsupported_parser(error.to_string()))?;
            let mut provider_bytes = (0..request.len())
                .map(|_| None)
                .collect::<Vec<Option<Vec<u8>>>>();
            for (rowid, positions) in positions_by_row {
                #[cfg(test)]
                self.native_row_reads
                    .set(self.native_row_reads.get().saturating_add(1));
                let row = load_exact_row(connection, rowid)
                    .map_err(|error| unavailable(error.to_string()))?
                    .ok_or_else(|| {
                        hydration_failure(
                            HydrationFailureKind::MissingRecord,
                            "Firebender chat-session row no longer exists",
                        )
                    })?;
                let row_digest = firebender_raw_row_digest(&row.logical_values());
                for (position, expected_session, expected_updated_at, message_index) in positions {
                    let locator = request.events()[position].locator();
                    if row.id != expected_session
                        || row.updated_at != expected_updated_at
                        || &row_digest != locator.record_digest()
                    {
                        return Err(stale(
                            "Firebender row identity, version, or digest no longer matches",
                        ));
                    }
                    let message_index = usize::try_from(message_index).map_err(|_| {
                        invalid_locator("Firebender message index exceeds platform limits")
                    })?;
                    let message = row.messages.get(message_index).ok_or_else(|| {
                        hydration_failure(
                            HydrationFailureKind::MissingRecord,
                            "Firebender nested message no longer exists",
                        )
                    })?;
                    let text = firebender_message_text(message).ok_or_else(|| {
                        unsupported_parser("Firebender message has no exact decoded display text")
                    })?;
                    provider_bytes[position] = Some(text.into_bytes());
                }
            }
            provider_bytes
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::MissingRecord,
                        "one or more Firebender rows were not hydrated",
                    )
                })
        })();
        snapshot
            .finish()
            .map_err(|error| unavailable(error.to_string()))?;
        let records = request
            .events()
            .iter()
            .zip(resolved?)
            .map(|(event, provider_bytes)| HydratedProviderRecord {
                event_id: event.event_id(),
                provider_bytes,
            })
            .collect();
        let result = BatchHydrationResult::new(records)
            .map_err(|error| invalid_locator(error.to_string()))?;
        result.validate_for_request(request)?;
        Ok(result)
    }
}

fn invalid_locator(detail: impl Into<String>) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::InvalidLocator, detail)
}

fn stale(detail: impl Into<String>) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::StaleRecordEvidence, detail)
}

fn unavailable(detail: impl Into<String>) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, detail)
}

fn unsupported_parser(detail: impl Into<String>) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::UnsupportedParserRevision, detail)
}

fn hydration_failure(kind: HydrationFailureKind, detail: impl Into<String>) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.into(),
    }
}

#[cfg(test)]
pub(crate) fn resolver_for_test(path: impl Into<PathBuf>) -> FirebenderExactResolver {
    FirebenderExactResolver::new(crate::test_provider_sqlite_data_root(), path)
}
