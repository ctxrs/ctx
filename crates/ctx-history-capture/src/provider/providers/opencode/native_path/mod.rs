use std::collections::BTreeSet;

use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::native_source::{NativeLocator, NativeSqliteValue};
use crate::{
    CaptureError, OutputAssociations, OutputCommandContext, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputSourceLocator,
    ProOutputObservation, Result,
};

mod json;
mod lifecycle;
mod model;
mod query;
mod scanner;
mod schema;
mod source;
pub(crate) mod source_backed;
pub(super) mod vertical;

#[cfg(test)]
mod tests;

use lifecycle::{
    classify_opencode_native_lifecycle, OpenCodeNativeGenerationChange,
    OpenCodeNativePriorGeneration, OpenCodeNativePublicationMode,
};
use model::{
    OpenCodeNativeEvent, OpenCodeNativeEventKind, OpenCodeNativeFileTouch, OpenCodeNativeFrontier,
    OpenCodeNativeLocator, OpenCodeNativeMetrics, OpenCodeNativeOrder, OpenCodeNativePage,
    OpenCodeNativePageAccounting, OpenCodeNativePageIdentity, OpenCodeNativePageLimits,
    OpenCodeNativePersistedState, OpenCodeNativePhysicalSourceIdentity, OpenCodeNativeProFrontier,
    OpenCodeNativeProOutputPage, OpenCodeNativeProPageIdentity, OpenCodeNativeProRejection,
    OpenCodeNativeProRejectionKind, OpenCodeNativeProReplaySummary, OpenCodeNativeProfile,
    OpenCodeNativeRejection, OpenCodeNativeRejectionKind, OpenCodeNativeScanPhase,
    OpenCodeNativeScanPosition, OpenCodeNativeScanSummary, OpenCodeNativeSchemaFamily,
    OpenCodeNativeSession, OpenCodeNativeSourceAuthority, OpenCodeNativeSourceSelection,
    OPENCODE_NATIVE_PAGE_MAX_BYTES,
};

use json::{OpenCodeJsonProjection, OpenCodeRetainedJson};
use query::{
    fetch_event_metadata_page, fetch_pro_metadata_page, fetch_session_page, has_pro_metadata_after,
    pro_keyset_for_frontier, EventKeyset, OpenCodeScanIndex, ProKeyset, ProRecordMetadata,
    RecordMetadata, SessionKeyset,
};
use scanner::OpenCodeNativeScanner;
use schema::{hex_digest, OpenCodeNativeSchema};
use source::OpenCodeSnapshotGeneration;

const OPENCODE_SEMANTIC_DIGEST_DOMAIN: &[u8] = b"ctx-opencode-nativepath-semantic-v1\0";
const OPENCODE_CORE_SESSION_INDEX_PAGE_BYTES: usize = OPENCODE_NATIVE_PAGE_MAX_BYTES - 512 * 1024;
const OPENCODE_CORE_EVENT_PROJECTION_PAGE_BYTES: usize =
    (OPENCODE_NATIVE_PAGE_MAX_BYTES - 1024 * 1024) / 2;
const OPENCODE_INVENTORY_OBSERVATION_TOKEN_MAX_BYTES: usize = 4 * 1024;
const OPENCODE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT: usize = 32;

pub(super) struct OpenCodeNativePathReader {
    snapshot: OpenCodeSnapshotGeneration,
    schema: OpenCodeNativeSchema,
    authority: OpenCodeNativeSourceAuthority,
    dialect: super::OpenCodeSqliteDialect,
}

impl OpenCodeNativePathReader {
    #[cfg(test)]
    pub(super) fn acquire(selection: OpenCodeNativeSourceSelection) -> Result<Self> {
        Self::acquire_for_dialect(selection, &super::OPENCODE_SQLITE_DIALECT)
    }

    pub(super) fn acquire_for_dialect(
        selection: OpenCodeNativeSourceSelection,
        dialect: &super::OpenCodeSqliteDialect,
    ) -> Result<Self> {
        if selection
            .inventory_observation_token
            .as_ref()
            .is_some_and(|token| token.len() > OPENCODE_INVENTORY_OBSERVATION_TOKEN_MAX_BYTES)
        {
            return Err(CaptureError::InvalidPayload(
                "OpenCode inventory observation token exceeds 4 KiB".to_owned(),
            ));
        }
        let snapshot = OpenCodeSnapshotGeneration::acquire(&selection.selected_path)?;
        let schema = OpenCodeNativeSchema::probe(snapshot.connection(), dialect)?;
        let authority = OpenCodeNativeSourceAuthority::ExactDispatchedDatabase {
            path: snapshot.observation().source_path().to_path_buf(),
            inventory_observation_token: selection.inventory_observation_token,
        };
        Ok(Self {
            snapshot,
            schema,
            authority,
            dialect: dialect.clone(),
        })
    }

    pub(super) fn scanner(
        &self,
        limits: OpenCodeNativePageLimits,
    ) -> Result<OpenCodeNativeScanner<'_>> {
        self.scanner_with_profile(OpenCodeNativeProfile::CoreOnly, limits)
    }

    pub(super) fn scanner_with_profile(
        &self,
        profile: OpenCodeNativeProfile,
        limits: OpenCodeNativePageLimits,
    ) -> Result<OpenCodeNativeScanner<'_>> {
        let limits = OpenCodeNativePageLimits::new(limits.rows, limits.retained_bytes)?;
        OpenCodeNativeScanner::new(
            self.snapshot.connection(),
            &self.schema,
            &self.snapshot,
            self.authority.clone(),
            limits,
            profile,
            self.dialect.clone(),
            None,
        )
    }

    pub(super) fn scanner_with_profile_and_prior(
        &self,
        profile: OpenCodeNativeProfile,
        limits: OpenCodeNativePageLimits,
        prior: &OpenCodeNativePersistedState,
    ) -> Result<OpenCodeNativeScanner<'_>> {
        let limits = OpenCodeNativePageLimits::new(limits.rows, limits.retained_bytes)?;
        OpenCodeNativeScanner::new(
            self.snapshot.connection(),
            &self.schema,
            &self.snapshot,
            self.authority.clone(),
            limits,
            profile,
            self.dialect.clone(),
            Some(prior),
        )
    }

    pub(super) fn revalidate_live(&self) -> Result<bool> {
        self.snapshot.revalidate_live()
    }

    fn complete_message_record(
        &self,
        event: &OpenCodeNativeEvent,
        dialect: &super::OpenCodeSqliteDialect,
    ) -> Result<(NativeLocator, Vec<NativeSqliteValue>, String, String)> {
        let locator = NativeLocator::new(event.locator.kind.clone(), event.locator.payload.clone())
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let (shape, rowid) = super::decode_opencode_message_locator(&locator)?;
        let values = super::content_locator::opencode_values_at_rowid(
            self.snapshot.connection(),
            shape,
            rowid,
        )?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "OpenCode complete-message row disappeared from its certified snapshot".to_owned(),
            )
        })?;
        let (session_id, message_id, complete_text, normalized_payload_hash) =
            super::opencode_complete_message_with_normalized_hash(&values, dialect)?;
        let expected_native_record_id =
            if self.schema.family == OpenCodeNativeSchemaFamily::MessagePart {
                format!("{}:{}", event.message_identity, event.native_identity)
            } else {
                event.native_identity.clone()
            };
        if session_id != event.session_identity || message_id != expected_native_record_id {
            return Err(CaptureError::InvalidPayload(
                "OpenCode complete-message locator resolved to the wrong native record".to_owned(),
            ));
        }
        Ok((locator, values, complete_text, normalized_payload_hash))
    }

    #[cfg(test)]
    pub(super) fn snapshot_path(&self) -> &std::path::Path {
        self.snapshot.snapshot_path()
    }
}
