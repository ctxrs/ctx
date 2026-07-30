use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, ContentSourceResolver, EventHydrationRequest,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, SessionHydrationRequest,
    SourceKey, SourceRecordLocator,
};
use serde_json::Value;

use super::{
    canonical_row_digest, kiro_source_key, phase_is_present, require_legacy_sqlite_format,
    validate_kiro_locator, KiroSourceBackedErrorV0, KiroSourceBackedResultV0,
};
use crate::{
    provider::providers::kiro::{
        history::{
            kiro_history_events, kiro_provider_session_id, kiro_session_started_at,
            KiroConversationRow,
        },
        native_path::{
            absolute_kiro_path, scan::load_key_batch, KiroPhase, KiroSqliteDatabase, KiroTables,
        },
    },
    CaptureError,
};

const HYDRATION_NATIVE_KEY_BATCH: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KiroHydratedRecordV0 {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: String,
}

#[derive(Debug)]
pub(crate) struct KiroLocatorResolverV0 {
    data_root: PathBuf,
    source_path: PathBuf,
    source: SourceKey,
    snapshot_opens: Cell<u64>,
    native_key_batches: Cell<u64>,
}

impl KiroLocatorResolverV0 {
    pub(crate) fn discover(
        data_root: impl Into<PathBuf>,
        source_path: impl AsRef<Path>,
        source_format: &str,
    ) -> KiroSourceBackedResultV0<Self> {
        let source_path = absolute_kiro_path(source_path.as_ref())?;
        require_legacy_sqlite_format(&source_path, source_format)?;
        Self::new(data_root, source_path)
    }

    pub(super) fn new(
        data_root: impl Into<PathBuf>,
        source_path: impl Into<PathBuf>,
    ) -> KiroSourceBackedResultV0<Self> {
        Ok(Self {
            data_root: data_root.into(),
            source_path: source_path.into(),
            source: kiro_source_key()?,
            snapshot_opens: Cell::new(0),
            native_key_batches: Cell::new(0),
        })
    }

    #[cfg(test)]
    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    #[cfg(test)]
    pub(crate) fn hydration_counters(&self) -> (u64, u64) {
        (self.snapshot_opens.get(), self.native_key_batches.get())
    }

    pub(crate) fn hydrate(
        &self,
        locator: &SourceRecordLocator,
    ) -> KiroSourceBackedResultV0<KiroHydratedRecordV0> {
        self.hydrate_locators(&[locator])?
            .into_iter()
            .next()
            .ok_or(KiroSourceBackedErrorV0::InvalidLocator)
    }

    fn hydrate_locators(
        &self,
        locators: &[&SourceRecordLocator],
    ) -> KiroSourceBackedResultV0<Vec<KiroHydratedRecordV0>> {
        if locators.is_empty() {
            return Ok(Vec::new());
        }
        let coordinates = locators
            .iter()
            .map(|locator| {
                locator.validate_contract()?;
                let (phase, key, event_key) = validate_kiro_locator(&self.source, locator)?;
                Ok(KiroHydrationCoordinate {
                    phase,
                    key,
                    event_key,
                    record_digest: *locator.record_digest(),
                })
            })
            .collect::<KiroSourceBackedResultV0<Vec<_>>>()?;

        let database = KiroSqliteDatabase::open(&self.data_root, &self.source_path)?;
        self.snapshot_opens
            .set(self.snapshot_opens.get().saturating_add(1));
        let resolved = (|| {
            let connection = database.connection(&self.source_path)?;
            let tables = KiroTables::probe(connection)?;
            if coordinates
                .iter()
                .any(|coordinate| !phase_is_present(tables, coordinate.phase))
            {
                return Err(KiroSourceBackedErrorV0::InvalidLocator);
            }
            let mut requested = BTreeMap::<KiroPhase, BTreeSet<String>>::new();
            for coordinate in &coordinates {
                requested
                    .entry(coordinate.phase)
                    .or_default()
                    .insert(coordinate.key.clone());
            }
            let mut rows = BTreeMap::<(KiroPhase, String), KiroConversationRow>::new();
            for (phase, keys) in requested {
                let keys = keys.into_iter().collect::<Vec<_>>();
                for batch in keys.chunks(HYDRATION_NATIVE_KEY_BATCH) {
                    self.native_key_batches
                        .set(self.native_key_batches.get().saturating_add(1));
                    for row in load_key_batch(connection, phase, batch)? {
                        let identity = (phase, row.key.clone());
                        if rows.insert(identity, row).is_some() {
                            return Err(KiroSourceBackedErrorV0::AmbiguousConversationKey {
                                relation: phase.table(),
                            });
                        }
                    }
                }
            }
            let mut hydrated_rows = BTreeMap::new();
            for (identity, row) in rows {
                hydrated_rows.insert(identity, hydrate_row(row)?);
            }
            coordinates
                .iter()
                .map(|coordinate| {
                    let row = hydrated_rows
                        .get(&(coordinate.phase, coordinate.key.clone()))
                        .ok_or(KiroSourceBackedErrorV0::MissingConversationRow)?;
                    if row.record_digest != coordinate.record_digest {
                        return Err(KiroSourceBackedErrorV0::ConversationRowDigestMismatch);
                    }
                    let decoded_display_text = row
                        .events
                        .get(&coordinate.event_key)
                        .cloned()
                        .ok_or(KiroSourceBackedErrorV0::MissingConversationEvent)?;
                    Ok(KiroHydratedRecordV0 {
                        provider_bytes: decoded_display_text.as_bytes().to_vec(),
                        decoded_display_text,
                    })
                })
                .collect()
        })();
        let finished = database.finish(&self.source_path);
        finished?;
        resolved
    }
}

impl ContentSourceResolver for KiroLocatorResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let hydrated = self
            .hydrate(request.locator())
            .map_err(hydration_failure_from_error)?;
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: hydrated.provider_bytes,
        })
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let locators = request
            .events()
            .iter()
            .map(EventHydrationRequest::locator)
            .collect::<Vec<_>>();
        let hydrated = self
            .hydrate_locators(&locators)
            .map_err(hydration_failure_from_error)?;
        let records = request
            .events()
            .iter()
            .zip(hydrated)
            .map(|(event, hydrated)| HydratedProviderRecord {
                event_id: event.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
            .collect();
        let result = BatchHydrationResult::new(records).map_err(|error| HydrationFailure {
            kind: HydrationFailureKind::InvalidLocator,
            detail: error.to_string(),
        })?;
        result.validate_for_request(request)?;
        Ok(result)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_batch(request.batch())
            .map(BatchHydrationResult::into_records)
    }
}

struct KiroHydrationCoordinate {
    phase: KiroPhase,
    key: String,
    event_key: String,
    record_digest: [u8; 32],
}

struct HydratedKiroRow {
    record_digest: [u8; 32],
    events: BTreeMap<String, String>,
}

fn hydrate_row(row: KiroConversationRow) -> KiroSourceBackedResultV0<HydratedKiroRow> {
    let (record_digest, _) = canonical_row_digest(&row)?;
    let value: Value = serde_json::from_str(&row.value)?;
    let provider_session_id = kiro_provider_session_id(&row, &value);
    let started_at = kiro_session_started_at(&row, &value, DateTime::<Utc>::UNIX_EPOCH);
    let events = kiro_history_events(&row, &provider_session_id, &value, started_at)
        .map(|decoded| {
            let text = decoded.complete_text();
            (decoded.event.cursor, text)
        })
        .collect();
    Ok(HydratedKiroRow {
        record_digest,
        events,
    })
}

pub(super) fn hydration_failure_from_error(error: KiroSourceBackedErrorV0) -> HydrationFailure {
    let kind = match &error {
        KiroSourceBackedErrorV0::InvalidLocator | KiroSourceBackedErrorV0::ResolverContract(_) => {
            HydrationFailureKind::InvalidLocator
        }
        KiroSourceBackedErrorV0::MissingConversationRow
        | KiroSourceBackedErrorV0::MissingConversationEvent => HydrationFailureKind::MissingRecord,
        KiroSourceBackedErrorV0::ConversationRowDigestMismatch
        | KiroSourceBackedErrorV0::AmbiguousConversationKey { .. }
        | KiroSourceBackedErrorV0::UncertifiableRow { .. }
        | KiroSourceBackedErrorV0::Json(_) => HydrationFailureKind::StaleRecordEvidence,
        KiroSourceBackedErrorV0::Capture(CaptureError::UnsupportedSchema(_))
        | KiroSourceBackedErrorV0::UnsupportedFormat(_) => {
            HydrationFailureKind::UnsupportedParserRevision
        }
        KiroSourceBackedErrorV0::Capture(_)
        | KiroSourceBackedErrorV0::Io(_)
        | KiroSourceBackedErrorV0::Sqlite(_)
        | KiroSourceBackedErrorV0::Projection(_)
        | KiroSourceBackedErrorV0::Route(_)
        | KiroSourceBackedErrorV0::CountOverflow => HydrationFailureKind::TemporarilyUnavailable,
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}
