use std::collections::BTreeSet;

use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::native_source::{NativeLocator, NativeSqliteValue};
use crate::{
    CaptureError, OutputAssociations, OutputCommandContext, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputSourceLocator,
    ProOutputObservation, Result, OPENCODE_SQLITE_SOURCE_FORMAT,
};

mod json;
mod lifecycle;
mod model;
mod query;
mod schema;
mod source;
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
}

impl OpenCodeNativePathReader {
    pub(super) fn acquire(selection: OpenCodeNativeSourceSelection) -> Result<Self> {
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
        let schema = OpenCodeNativeSchema::probe(snapshot.connection())?;
        let authority = OpenCodeNativeSourceAuthority::ExactDispatchedDatabase {
            path: snapshot.observation().source_path().to_path_buf(),
            inventory_observation_token: selection.inventory_observation_token,
        };
        Ok(Self {
            snapshot,
            schema,
            authority,
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
    ) -> Result<(NativeLocator, Vec<NativeSqliteValue>, String)> {
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
        let (session_id, message_id, complete_text) =
            super::opencode_complete_message(&values, dialect)?;
        if session_id != event.session_identity || message_id != event.message_identity {
            return Err(CaptureError::InvalidPayload(
                "OpenCode complete-message locator resolved to the wrong native record".to_owned(),
            ));
        }
        Ok((locator, values, complete_text))
    }

    #[cfg(test)]
    pub(super) fn snapshot_path(&self) -> &std::path::Path {
        self.snapshot.snapshot_path()
    }
}

pub(super) struct OpenCodeNativeScanner<'reader> {
    schema: &'reader OpenCodeNativeSchema,
    snapshot: &'reader OpenCodeSnapshotGeneration,
    index: OpenCodeScanIndex,
    authority: OpenCodeNativeSourceAuthority,
    limits: OpenCodeNativePageLimits,
    profile: OpenCodeNativeProfile,
    session_keyset: SessionKeyset,
    event_keyset: EventKeyset,
    position: OpenCodeNativeScanPosition,
    pro_keyset: ProKeyset,
    pro_frontier: OpenCodeNativeProFrontier,
    pending_core_page: Option<OpenCodeNativePage>,
    core_exhausted: bool,
    semantic_hasher: Sha256,
    metrics: OpenCodeNativeMetrics,
}

impl<'reader> OpenCodeNativeScanner<'reader> {
    fn new(
        conn: &'reader Connection,
        schema: &'reader OpenCodeNativeSchema,
        snapshot: &'reader OpenCodeSnapshotGeneration,
        authority: OpenCodeNativeSourceAuthority,
        limits: OpenCodeNativePageLimits,
        profile: OpenCodeNativeProfile,
        prior: Option<&OpenCodeNativePersistedState>,
    ) -> Result<Self> {
        let mut semantic_hasher = Sha256::new();
        semantic_hasher.update(OPENCODE_SEMANTIC_DIGEST_DOMAIN);
        hash_str(&mut semantic_hasher, schema.family.label());
        let index = OpenCodeScanIndex::build(
            conn,
            schema,
            OPENCODE_CORE_SESSION_INDEX_PAGE_BYTES,
            profile,
            prior.map(|state| &state.ordered_prefix_evidence),
        )?;
        let build_metrics = index.build_metrics();
        Ok(Self {
            schema,
            snapshot,
            index,
            authority,
            limits,
            profile,
            session_keyset: SessionKeyset::default(),
            event_keyset: EventKeyset::default(),
            position: OpenCodeNativeScanPosition::default(),
            pro_keyset: ProKeyset::default(),
            pro_frontier: OpenCodeNativeProFrontier::default(),
            pending_core_page: None,
            core_exhausted: false,
            semantic_hasher,
            metrics: OpenCodeNativeMetrics {
                snapshot_attempts: snapshot.attempts(),
                source_session_rows_scanned: build_metrics.source_session_rows_scanned,
                source_event_rows_scanned: build_metrics.source_event_rows_scanned,
                snapshot_session_rows_indexed: build_metrics.snapshot_session_rows_indexed,
                snapshot_event_rows_indexed: build_metrics.snapshot_event_rows_indexed,
                snapshot_ordering_passes: build_metrics.snapshot_ordering_passes,
                prefix_session_rows_read: build_metrics.prefix_session_rows_read,
                prefix_event_rows_read: build_metrics.prefix_event_rows_read,
                prefix_pro_rows_read: build_metrics.prefix_pro_rows_read,
                json_records_visited: build_metrics.json_records_visited,
                json_bytes_visited: build_metrics.json_bytes_visited,
                ..OpenCodeNativeMetrics::default()
            },
        })
    }

    pub(super) fn next_page(&mut self) -> Result<Option<OpenCodeNativePage>> {
        if self.core_exhausted {
            return Ok(None);
        }
        loop {
            match self.next_unbuffered_core_page()? {
                Some(page) if page_logical_units(&page) != 0 => {
                    if let Some(mut previous) = self.pending_core_page.replace(page) {
                        if let Some(pending) = self.pending_core_page.as_mut() {
                            pending.expected_frontier = previous.next_frontier;
                        }
                        finalize_core_page(&mut previous, false)?;
                        return Ok(Some(previous));
                    }
                }
                Some(empty) => {
                    if let Some(pending) = self.pending_core_page.as_mut() {
                        pending.next_frontier = empty.next_frontier;
                        pending.position = empty.position;
                    }
                }
                None => {
                    self.core_exhausted = true;
                    if let Some(mut final_page) = self.pending_core_page.take() {
                        final_page.next_frontier = OpenCodeNativeFrontier {
                            phase: OpenCodeNativeScanPhase::Complete,
                            scan_ordinal: self.position.native_events_seen,
                        };
                        final_page.position = self.position.clone();
                        finalize_core_page(&mut final_page, true)?;
                        return Ok(Some(final_page));
                    }
                    return Ok(None);
                }
            }
        }
    }

    fn next_unbuffered_core_page(&mut self) -> Result<Option<OpenCodeNativePage>> {
        loop {
            match self.position.phase {
                OpenCodeNativeScanPhase::Sessions => {
                    self.metrics.session_page_queries =
                        self.metrics.session_page_queries.saturating_add(1);
                    let scanned = fetch_session_page(
                        self.index.connection(),
                        self.session_keyset,
                        self.limits.rows,
                        self.limits
                            .retained_bytes
                            .min(OPENCODE_CORE_SESSION_INDEX_PAGE_BYTES),
                    )?;
                    self.metrics.indexed_session_rows_read = self
                        .metrics
                        .indexed_session_rows_read
                        .saturating_add(scanned.len() as u64);
                    if scanned.is_empty() {
                        self.position.phase = OpenCodeNativeScanPhase::Events;
                        continue;
                    }
                    let mut page = self.empty_page();
                    for scanned_session in scanned {
                        self.session_keyset = SessionKeyset {
                            scan_ordinal: scanned_session.scan_ordinal,
                            metadata_prefix_bytes: scanned_session.metadata_prefix_bytes,
                        };
                        let session = scanned_session.row;
                        self.position.native_sessions_seen =
                            self.position.native_sessions_seen.saturating_add(1);
                        self.metrics.native_sessions =
                            self.metrics.native_sessions.saturating_add(1);
                        self.hash_session(&session);
                        page.sessions.push(session);
                    }
                    page.position = self.position.clone();
                    page.next_frontier = frontier_for_position(&self.position);
                    return Ok(Some(page));
                }
                OpenCodeNativeScanPhase::Events => {
                    self.metrics.event_metadata_page_queries =
                        self.metrics.event_metadata_page_queries.saturating_add(1);
                    let metadata = fetch_event_metadata_page(
                        self.index.connection(),
                        self.event_keyset,
                        self.limits.rows,
                        self.limits
                            .retained_bytes
                            .min(OPENCODE_CORE_EVENT_PROJECTION_PAGE_BYTES),
                        self.schema.family,
                    )?;
                    self.metrics.indexed_event_rows_read = self
                        .metrics
                        .indexed_event_rows_read
                        .saturating_add(metadata.len() as u64);
                    if metadata.is_empty() {
                        self.position.phase = OpenCodeNativeScanPhase::Complete;
                        return Ok(None);
                    }
                    return self.project_event_page(metadata).map(Some);
                }
                OpenCodeNativeScanPhase::Complete => return Ok(None),
            }
        }
    }

    pub(super) fn next_pro_output_page(&mut self) -> Result<Option<OpenCodeNativeProOutputPage>> {
        if self.profile == OpenCodeNativeProfile::CoreOnly || self.pro_frontier.terminal {
            return Ok(None);
        }
        let metadata = fetch_pro_metadata_page(
            self.index.connection(),
            self.pro_keyset,
            self.limits.rows,
            OPENCODE_NATIVE_PAGE_MAX_BYTES,
            self.schema.family,
        )?;
        if metadata.is_empty() {
            self.pro_frontier.terminal = true;
            return Ok(None);
        }
        let expected_frontier = self.pro_frontier;
        let mut observations = Vec::new();
        let mut rejections = Vec::new();
        for record in &metadata {
            if let Some(observation) = project_pro_output(record) {
                self.metrics.output_content_cells_transferred = self
                    .metrics
                    .output_content_cells_transferred
                    .saturating_add(1);
                self.metrics.output_content_bytes_transferred = self
                    .metrics
                    .output_content_bytes_transferred
                    .saturating_add(observation.content.len() as u64);
                observations.push(observation);
            } else {
                rejections.push(project_pro_rejection(record)?);
            }
        }
        let last = metadata.last().ok_or(CaptureError::SystemInvariant(
            "OpenCode nonempty Pro metadata page lost its final unit",
        ))?;
        self.pro_keyset = ProKeyset {
            pro_ordinal: last.pro_ordinal,
            output_prefix_bytes: last.output_prefix_bytes,
        };
        let terminal = !has_pro_metadata_after(self.index.connection(), last.pro_ordinal)?;
        self.pro_frontier = OpenCodeNativeProFrontier {
            source_event_ordinal: last.source_event_ordinal,
            subrecord_index: last.subrecord_index,
            terminal,
        };
        let mut page = OpenCodeNativeProOutputPage {
            identity: OpenCodeNativeProPageIdentity([0; 32]),
            source_authority: self.authority.clone(),
            expected_frontier,
            next_frontier: self.pro_frontier,
            terminal,
            accounting: OpenCodeNativePageAccounting::default(),
            observations,
            rejections,
        };
        finalize_pro_page(&mut page)?;
        Ok(Some(page))
    }

    pub(super) fn resume_pro_from(&mut self, frontier: OpenCodeNativeProFrontier) -> Result<()> {
        if self.profile != OpenCodeNativeProfile::CoreAndPro {
            return Err(CaptureError::InvalidPayload(
                "OpenCode Pro replay requires the CoreAndPro profile".to_owned(),
            ));
        }
        if self.pro_keyset != ProKeyset::default()
            || self.pro_frontier != OpenCodeNativeProFrontier::default()
        {
            return Err(CaptureError::InvalidPayload(
                "OpenCode Pro replay frontier must be installed before reading output pages"
                    .to_owned(),
            ));
        }
        self.pro_keyset = pro_keyset_for_frontier(self.index.connection(), frontier)?;
        self.pro_frontier = frontier;
        Ok(())
    }

    pub(super) fn finish_pro_replay(self) -> Result<OpenCodeNativeProReplaySummary> {
        if self.profile != OpenCodeNativeProfile::CoreAndPro || !self.pro_frontier.terminal {
            return Err(CaptureError::InvalidPayload(
                "OpenCode Pro replay must exhaust its exact output frontier before finish"
                    .to_owned(),
            ));
        }
        if !self.snapshot.revalidate_live()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(OpenCodeNativeProReplaySummary {
            source_authority: self.authority,
            source_generation_digest: self.snapshot.observation().generation_digest(),
            capability_digest: self.schema.capability_digest.clone(),
            frontier: self.pro_frontier,
            complete: true,
        })
    }

    pub(super) fn finish(self) -> Result<OpenCodeNativeScanSummary> {
        if self.position.phase != OpenCodeNativeScanPhase::Complete {
            return Err(CaptureError::InvalidPayload(
                "OpenCode NativePath scan must be exhausted before finish".to_owned(),
            ));
        }
        if !self.snapshot.revalidate_live()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let semantic_digest: [u8; 32] = self.semantic_hasher.finalize().into();
        Ok(OpenCodeNativeScanSummary {
            source_authority: self.authority,
            source_generation_digest: self.snapshot.observation().generation_digest(),
            physical_source_identity: self.snapshot.observation().physical_source_identity(),
            capability_digest: self.schema.capability_digest.clone(),
            semantic_digest: hex_digest(semantic_digest),
            schema_family: self.schema.family,
            identity_semantics: self.schema.family.identity_semantics(),
            ordering_semantics: self.schema.family.ordering_semantics(),
            complete: true,
            profile: self.profile,
            core_frontier: OpenCodeNativeFrontier {
                phase: OpenCodeNativeScanPhase::Complete,
                scan_ordinal: self.position.native_events_seen,
            },
            pro_frontier: self.pro_frontier,
            ordered_prefix_evidence: Box::new(self.index.ordered_prefix_evidence().clone()),
            restart_prefix_comparison: self
                .index
                .restart_prefix_comparison()
                .cloned()
                .map(Box::new),
            metrics: self.metrics,
        })
    }

    fn project_event_page(
        &mut self,
        mut metadata: Vec<RecordMetadata>,
    ) -> Result<OpenCodeNativePage> {
        let mut page = self.empty_page();
        for mut record in metadata.drain(..) {
            self.event_keyset = EventKeyset {
                scan_ordinal: record.scan_ordinal,
                retained_prefix_bytes: record.retained_prefix_bytes,
            };
            self.position.native_events_seen = self.position.native_events_seen.saturating_add(1);
            self.metrics.native_events = self.metrics.native_events.saturating_add(1);
            let projection = std::mem::replace(
                &mut record.projection,
                OpenCodeJsonProjection::Rejected(
                    OpenCodeNativeRejectionKind::RetainedParseMismatch,
                ),
            );
            match projection {
                OpenCodeJsonProjection::Retained(retained) => {
                    let sparse_output_diagnostic = retained.body.get("result_outcome").is_some();
                    if sparse_output_diagnostic {
                        self.metrics.excluded_outputs =
                            self.metrics.excluded_outputs.saturating_add(1);
                        self.metrics.output_previews_built =
                            self.metrics.output_previews_built.saturating_add(1);
                    }
                    self.metrics.retained_content_cells_transferred = self
                        .metrics
                        .retained_content_cells_transferred
                        .saturating_add(1);
                    self.metrics.retained_content_bytes_transferred = self
                        .metrics
                        .retained_content_bytes_transferred
                        .saturating_add(record.content_bytes);
                    match normalize_retained_event(&record, retained) {
                        Ok(event) => {
                            self.hash_event(&event);
                            self.metrics.retained_events =
                                self.metrics.retained_events.saturating_add(1);
                            page.events.push(event);
                        }
                        Err(error) => {
                            let rejection = rejection(
                                &record,
                                OpenCodeNativeRejectionKind::RetainedParseMismatch,
                                error.to_string(),
                            );
                            self.hash_rejection(&rejection);
                            self.metrics.rejected_records =
                                self.metrics.rejected_records.saturating_add(1);
                            page.rejections.push(rejection);
                        }
                    }
                }
                OpenCodeJsonProjection::Output(output) => {
                    self.metrics.excluded_outputs = self.metrics.excluded_outputs.saturating_add(1);
                    if let Some(diagnostic) = output.diagnostic {
                        match normalize_retained_event(&record, diagnostic) {
                            Ok(event) => {
                                self.hash_event(&event);
                                self.metrics.retained_events =
                                    self.metrics.retained_events.saturating_add(1);
                                page.events.push(event);
                            }
                            Err(error) => {
                                let rejection = rejection(
                                    &record,
                                    OpenCodeNativeRejectionKind::RetainedParseMismatch,
                                    error.to_string(),
                                );
                                self.hash_rejection(&rejection);
                                self.metrics.rejected_records =
                                    self.metrics.rejected_records.saturating_add(1);
                                page.rejections.push(rejection);
                            }
                        }
                    }
                }
                OpenCodeJsonProjection::ExcludedOutput => {
                    self.metrics.excluded_outputs = self.metrics.excluded_outputs.saturating_add(1);
                }
                OpenCodeJsonProjection::Rejected(kind) => {
                    let rejection = rejection(
                        &record,
                        kind,
                        format!(
                            "OpenCode native record {} rejected as {}",
                            record.native_identity,
                            kind.label()
                        ),
                    );
                    self.hash_rejection(&rejection);
                    self.metrics.rejected_records = self.metrics.rejected_records.saturating_add(1);
                    page.rejections.push(rejection);
                }
                OpenCodeJsonProjection::RejectedWithReason(kind, reason) => {
                    let rejection = rejection(&record, kind, reason);
                    self.hash_rejection(&rejection);
                    self.metrics.rejected_records = self.metrics.rejected_records.saturating_add(1);
                    page.rejections.push(rejection);
                }
            }
        }
        page.position = self.position.clone();
        page.next_frontier = frontier_for_position(&self.position);
        Ok(page)
    }

    fn empty_page(&self) -> OpenCodeNativePage {
        let frontier = frontier_for_position(&self.position);
        OpenCodeNativePage {
            identity: OpenCodeNativePageIdentity([0; 32]),
            source_authority: self.authority.clone(),
            expected_frontier: frontier,
            next_frontier: frontier,
            terminal: false,
            accounting: OpenCodeNativePageAccounting::default(),
            position: self.position.clone(),
            sessions: Vec::new(),
            events: Vec::new(),
            excluded_outputs: Vec::new(),
            rejections: Vec::new(),
        }
    }

    fn hash_session(&mut self, session: &OpenCodeNativeSession) {
        self.semantic_hasher.update(b"session");
        hash_str(&mut self.semantic_hasher, &session.native_identity);
        hash_str(&mut self.semantic_hasher, &session.content_digest);
    }

    fn hash_event(&mut self, event: &OpenCodeNativeEvent) {
        self.semantic_hasher.update(b"event");
        hash_str(&mut self.semantic_hasher, &event.native_identity);
        hash_order(&mut self.semantic_hasher, &event.native_order);
        hash_str(&mut self.semantic_hasher, &event.content_digest);
    }

    fn hash_rejection(&mut self, rejection: &OpenCodeNativeRejection) {
        self.semantic_hasher.update(b"rejection");
        hash_str(&mut self.semantic_hasher, &rejection.native_identity);
        hash_str(&mut self.semantic_hasher, rejection.kind.label());
    }
}

fn frontier_for_position(position: &OpenCodeNativeScanPosition) -> OpenCodeNativeFrontier {
    OpenCodeNativeFrontier {
        phase: position.phase,
        scan_ordinal: match position.phase {
            OpenCodeNativeScanPhase::Sessions => position.native_sessions_seen,
            OpenCodeNativeScanPhase::Events | OpenCodeNativeScanPhase::Complete => {
                position.native_events_seen
            }
        },
    }
}

fn page_logical_units(page: &OpenCodeNativePage) -> usize {
    page.sessions
        .len()
        .saturating_add(page.events.len())
        .saturating_add(page.rejections.len())
}

fn finalize_core_page(page: &mut OpenCodeNativePage, terminal: bool) -> Result<()> {
    if !page.excluded_outputs.is_empty() {
        return Err(CaptureError::SystemInvariant(
            "OpenCode Core page contains an output marker",
        ));
    }
    page.terminal = terminal;
    let logical_units = page_logical_units(page);
    let conservative_serialized_bytes = core_page_encoded_bytes(page)?;
    validate_page_bounds(logical_units, conservative_serialized_bytes, "Core")?;
    page.accounting = OpenCodeNativePageAccounting {
        logical_units,
        conservative_serialized_bytes,
    };
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-nativepath-core-page-v1\0");
    hash_frontier(&mut hasher, page.expected_frontier);
    hash_frontier(&mut hasher, page.next_frontier);
    hasher.update([u8::from(page.terminal)]);
    for session in &page.sessions {
        hasher.update(b"session");
        hash_str(&mut hasher, &session.native_identity);
        hash_str(&mut hasher, &session.content_digest);
    }
    for event in &page.events {
        hasher.update(b"event");
        hash_str(&mut hasher, &event.native_identity);
        hash_str(&mut hasher, &event.content_digest);
        hash_str(&mut hasher, &event.locator.kind);
        hash_bytes(&mut hasher, &event.locator.payload);
    }
    for rejection in &page.rejections {
        hasher.update(b"rejection");
        hash_str(&mut hasher, &rejection.native_identity);
        if let Some(session_identity) = rejection.session_identity.as_deref() {
            hash_str(&mut hasher, session_identity);
        }
        if let Some(order) = rejection.native_order.as_ref() {
            hash_order(&mut hasher, order);
        }
        hash_str(&mut hasher, rejection.kind.label());
        hash_str(&mut hasher, &rejection.reason);
    }
    page.identity = OpenCodeNativePageIdentity(hasher.finalize().into());
    Ok(())
}

fn finalize_pro_page(page: &mut OpenCodeNativeProOutputPage) -> Result<()> {
    let logical_units = page
        .observations
        .len()
        .saturating_add(page.rejections.len());
    let conservative_serialized_bytes = pro_page_encoded_bytes(page)?;
    validate_page_bounds(logical_units, conservative_serialized_bytes, "Pro")?;
    page.accounting = OpenCodeNativePageAccounting {
        logical_units,
        conservative_serialized_bytes,
    };
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-nativepath-pro-page-v1\0");
    hash_pro_frontier(&mut hasher, page.expected_frontier);
    hash_pro_frontier(&mut hasher, page.next_frontier);
    hasher.update([u8::from(page.terminal)]);
    for output in &page.observations {
        hasher.update([match output.kind {
            OutputObservationKind::Command => 1,
            OutputObservationKind::Tool => 2,
        }]);
        hash_str(&mut hasher, &output.coordinate.unit_key);
        hasher.update(output.coordinate.native_sequence.to_le_bytes());
        hash_optional_str(&mut hasher, output.coordinate.native_record_id.as_deref());
        hash_optional_u64(&mut hasher, output.coordinate.source_record_ordinal);
        hash_optional_u32(&mut hasher, output.coordinate.source_record_subrecord_index);
        hash_optional_u64(&mut hasher, output.coordinate.byte_start);
        hash_optional_u64(&mut hasher, output.coordinate.byte_end_exclusive);
        hash_optional_i64(&mut hasher, output.occurred_at_unix_ms);
        hash_str(&mut hasher, &output.associations.direct_session_id);
        hash_str(&mut hasher, &output.associations.root_session_id);
        hash_optional_str(
            &mut hasher,
            output.associations.parent_session_id.as_deref(),
        );
        hash_optional_str(
            &mut hasher,
            output.associations.provider_session_id.as_deref(),
        );
        hash_optional_str(&mut hasher, output.associations.agent_id.as_deref());
        match output.associations.repository.as_ref() {
            Some(repository) => {
                hasher.update([1]);
                hash_str(&mut hasher, &repository.repository_id);
                hash_optional_str(&mut hasher, repository.checkout_id.as_deref());
                hash_optional_str(&mut hasher, repository.worktree_id.as_deref());
                hash_optional_str(&mut hasher, repository.object_format.as_deref());
            }
            None => hasher.update([0]),
        }
        hash_optional_str(&mut hasher, output.call_id.as_deref());
        match output.command.as_ref() {
            Some(command) => {
                hasher.update([1]);
                hash_str(&mut hasher, &command.tool_name);
                hash_str(&mut hasher, &command.command);
                hash_optional_str(&mut hasher, command.working_directory.as_deref());
            }
            None => hasher.update([0]),
        }
        hasher.update([match output.outcome.outcome {
            OutputOutcome::Success => 1,
            OutputOutcome::Failure => 2,
            OutputOutcome::Timeout => 3,
            OutputOutcome::Unknown => 4,
        }]);
        match output.outcome.exit_code {
            Some(value) => {
                hasher.update([1]);
                hasher.update(value.to_le_bytes());
            }
            None => hasher.update([0]),
        }
        hash_optional_u64(&mut hasher, output.outcome.duration_ms);
        hasher.update(output.locator.version.to_le_bytes());
        hash_str(&mut hasher, &output.locator.kind);
        hash_bytes(&mut hasher, &output.locator.payload);
        hasher.update((output.content.len() as u64).to_le_bytes());
        hasher.update(&output.content);
    }
    for rejection in &page.rejections {
        hash_str(&mut hasher, &rejection.native_identity);
        hasher.update(rejection.source_event_ordinal.to_le_bytes());
        match rejection.subrecord_index {
            Some(value) => {
                hasher.update([1]);
                hasher.update(value.to_le_bytes());
            }
            None => hasher.update([0]),
        }
        hasher.update([match rejection.kind {
            OpenCodeNativeProRejectionKind::MalformedOutput => 1,
            OpenCodeNativeProRejectionKind::OversizedOutput => 2,
            OpenCodeNativeProRejectionKind::TooManySubrecords => 3,
        }]);
        hash_str(&mut hasher, &rejection.reason);
        hash_str(&mut hasher, &rejection.locator.kind);
        hash_bytes(&mut hasher, &rejection.locator.payload);
    }
    page.identity = OpenCodeNativeProPageIdentity(hasher.finalize().into());
    Ok(())
}

fn validate_page_bounds(units: usize, bytes: usize, lane: &str) -> Result<()> {
    if units == 0 || units > model::OPENCODE_NATIVE_PAGE_MAX_UNITS {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {lane} page has {units} logical units"
        )));
    }
    if bytes > OPENCODE_NATIVE_PAGE_MAX_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {lane} page has {bytes} conservatively encoded bytes"
        )));
    }
    Ok(())
}

fn project_pro_output(record: &ProRecordMetadata) -> Option<ProOutputObservation> {
    let draft = record.draft.as_ref()?;
    let kind = if draft.kind == 1 {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    let command = (kind == OutputObservationKind::Command).then(|| OutputCommandContext {
        tool_name: draft
            .tool_name
            .clone()
            .unwrap_or_else(|| "shell".to_owned()),
        command: draft.command.clone().unwrap_or_else(|| "shell".to_owned()),
        working_directory: draft.working_directory.clone(),
    });
    Some(ProOutputObservation {
        kind,
        coordinate: OutputNativeCoordinate {
            unit_key: if draft.subrecord_index == 0 {
                format!(
                    "{OPENCODE_SQLITE_SOURCE_FORMAT}:{}:{}:output",
                    record.session_identity, record.source_native_identity
                )
            } else {
                format!(
                    "{OPENCODE_SQLITE_SOURCE_FORMAT}:{}:{}:output:subrecord:{}",
                    record.session_identity, record.source_native_identity, draft.subrecord_index
                )
            },
            native_sequence: record.native_record_ordinal,
            native_record_id: Some(record.source_native_identity.clone()),
            source_record_ordinal: Some(record.native_record_ordinal),
            source_record_subrecord_index: Some(draft.subrecord_index),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: Some(record.time_created),
        associations: OutputAssociations {
            direct_session_id: record.session_identity.clone(),
            root_session_id: record.root_session_identity.clone(),
            parent_session_id: record.parent_session_identity.clone(),
            provider_session_id: Some(record.session_identity.clone()),
            agent_id: record.agent_identity.clone(),
            repository: None,
        },
        call_id: draft.call_id.clone(),
        command,
        outcome: OutputOutcomeMetadata {
            outcome: match draft.outcome {
                1 => OutputOutcome::Success,
                2 => OutputOutcome::Failure,
                3 => OutputOutcome::Timeout,
                _ => OutputOutcome::Unknown,
            },
            exit_code: draft.exit_code,
            duration_ms: draft.duration_ms,
        },
        locator: OutputSourceLocator {
            version: record.locator.version,
            kind: record.locator.kind.clone(),
            payload: record.locator.payload.clone(),
        },
        content: draft.content.as_bytes().to_vec(),
    })
}

fn project_pro_rejection(record: &ProRecordMetadata) -> Result<OpenCodeNativeProRejection> {
    let reason = record
        .rejection
        .clone()
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode Pro record has neither output nor rejection",
        ))?;
    let kind = if reason.contains("subrecords; maximum") {
        OpenCodeNativeProRejectionKind::TooManySubrecords
    } else if reason.contains("encoded bytes") {
        OpenCodeNativeProRejectionKind::OversizedOutput
    } else {
        OpenCodeNativeProRejectionKind::MalformedOutput
    };
    Ok(OpenCodeNativeProRejection {
        source_event_ordinal: record.source_event_ordinal,
        native_identity: record.native_identity.clone(),
        subrecord_index: (record.subrecord_index != u32::MAX).then_some(record.subrecord_index),
        kind,
        reason,
        locator: record.locator.clone(),
    })
}

#[derive(Default)]
struct EncodedByteCounter {
    bytes: usize,
}

impl EncodedByteCounter {
    fn fixed(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes);
    }

    fn bytes(&mut self, value: &[u8]) {
        self.fixed(8);
        self.fixed(value.len());
    }

    fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn optional_string(&mut self, value: Option<&str>) {
        self.fixed(1);
        if let Some(value) = value {
            self.string(value);
        }
    }

    fn locator(&mut self, locator: &OpenCodeNativeLocator) {
        self.fixed(4);
        self.string(&locator.kind);
        self.bytes(&locator.payload);
    }
}

fn core_page_encoded_bytes(page: &OpenCodeNativePage) -> Result<usize> {
    let mut counter = EncodedByteCounter::default();
    counter.fixed(32 + 1 + 4 * 8);
    counter.string(&page.source_authority.selected_path().to_string_lossy());
    let OpenCodeNativeSourceAuthority::ExactDispatchedDatabase {
        inventory_observation_token,
        ..
    } = &page.source_authority;
    counter.optional_string(inventory_observation_token.as_deref());
    for session in &page.sessions {
        counter.string(&session.native_identity);
        counter.optional_string(session.parent_identity.as_deref());
        counter.string(&session.root_identity);
        counter.optional_string(session.title.as_deref());
        counter.optional_string(session.directory.as_deref());
        counter.optional_string(session.model_identity.as_deref());
        counter.optional_string(session.agent_identity.as_deref());
        counter.fixed(16);
        counter.string(&session.content_digest);
    }
    for event in &page.events {
        counter.string(&event.native_identity);
        counter.string(&event.message_identity);
        counter.string(&event.session_identity);
        count_order(&mut counter, &event.native_order);
        counter.fixed(1 + 16);
        counter.string(&event.role);
        counter.string(&event.searchable_text);
        let body = serde_json::to_vec(&event.body).map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "OpenCode Core page body cannot be encoded: {error}"
            ))
        })?;
        counter.bytes(&body);
        counter.string(&event.content_digest);
        counter.fixed(8);
        for touch in &event.file_touches {
            counter.string(&touch.path);
        }
        counter.locator(&event.locator);
    }
    for rejection in &page.rejections {
        counter.string(&rejection.native_identity);
        counter.optional_string(rejection.session_identity.as_deref());
        counter.fixed(1);
        if let Some(order) = rejection.native_order.as_ref() {
            count_order(&mut counter, order);
        }
        counter.fixed(1);
        counter.string(&rejection.reason);
    }
    Ok(counter.bytes)
}

fn pro_page_encoded_bytes(page: &OpenCodeNativeProOutputPage) -> Result<usize> {
    let mut counter = EncodedByteCounter::default();
    counter.fixed(32 + 1 + 6 * 8);
    counter.string(&page.source_authority.selected_path().to_string_lossy());
    let OpenCodeNativeSourceAuthority::ExactDispatchedDatabase {
        inventory_observation_token,
        ..
    } = &page.source_authority;
    counter.optional_string(inventory_observation_token.as_deref());
    for output in &page.observations {
        counter.fixed(1);
        counter.string(&output.coordinate.unit_key);
        counter.fixed(8);
        counter.optional_string(output.coordinate.native_record_id.as_deref());
        counter.fixed(1 + 8);
        counter.fixed(1 + 4);
        counter.fixed(2 * (1 + 8));
        counter.fixed(1 + 8);
        counter.string(&output.associations.direct_session_id);
        counter.string(&output.associations.root_session_id);
        counter.optional_string(output.associations.parent_session_id.as_deref());
        counter.optional_string(output.associations.provider_session_id.as_deref());
        counter.optional_string(output.associations.agent_id.as_deref());
        counter.fixed(1);
        if let Some(repository) = output.associations.repository.as_ref() {
            counter.string(&repository.repository_id);
            counter.optional_string(repository.checkout_id.as_deref());
            counter.optional_string(repository.worktree_id.as_deref());
            counter.optional_string(repository.object_format.as_deref());
        }
        counter.optional_string(output.call_id.as_deref());
        counter.fixed(1);
        if let Some(command) = output.command.as_ref() {
            counter.string(&command.tool_name);
            counter.string(&command.command);
            counter.optional_string(command.working_directory.as_deref());
        }
        counter.fixed(1 + 1 + 4 + 1 + 8);
        counter.fixed(4);
        counter.string(&output.locator.kind);
        counter.bytes(&output.locator.payload);
        counter.bytes(&output.content);
    }
    for rejection in &page.rejections {
        counter.fixed(8);
        counter.string(&rejection.native_identity);
        counter.fixed(1 + 4 + 1);
        counter.string(&rejection.reason);
        counter.locator(&rejection.locator);
    }
    Ok(counter.bytes)
}

fn count_order(counter: &mut EncodedByteCounter, order: &OpenCodeNativeOrder) {
    counter.fixed(1);
    match order {
        OpenCodeNativeOrder::ExplicitSequence {
            session_id,
            message_id,
            ..
        }
        | OpenCodeNativeOrder::SynthesizedSequence {
            session_id,
            message_id,
            ..
        } => {
            counter.string(session_id);
            counter.fixed(8);
            counter.string(message_id);
        }
        OpenCodeNativeOrder::MessagePart {
            session_id,
            message_id,
            part_id,
            ..
        } => {
            counter.string(session_id);
            counter.fixed(16);
            counter.string(message_id);
            counter.string(part_id);
        }
    }
}

fn hash_frontier(hasher: &mut Sha256, frontier: OpenCodeNativeFrontier) {
    hasher.update([match frontier.phase {
        OpenCodeNativeScanPhase::Sessions => 1,
        OpenCodeNativeScanPhase::Events => 2,
        OpenCodeNativeScanPhase::Complete => 3,
    }]);
    hasher.update(frontier.scan_ordinal.to_le_bytes());
}

fn hash_pro_frontier(hasher: &mut Sha256, frontier: OpenCodeNativeProFrontier) {
    hasher.update(frontier.source_event_ordinal.to_le_bytes());
    hasher.update(frontier.subrecord_index.to_le_bytes());
    hasher.update([u8::from(frontier.terminal)]);
}

fn normalize_retained_event(
    record: &RecordMetadata,
    retained: OpenCodeRetainedJson,
) -> Result<OpenCodeNativeEvent> {
    let OpenCodeRetainedJson {
        effective_type,
        role,
        mut body,
    } = retained;
    let kind = retained_event_kind(&effective_type, &role, &body);
    let searchable_text = retained_searchable_text(kind, &effective_type, &body);
    let time_created = body
        .pointer("/time/created")
        .and_then(Value::as_i64)
        .unwrap_or(record.time_created);
    let (file_touches, file_touch_count) = retained_file_touches(kind, &body);
    if file_touch_count > file_touches.len() {
        if let Some(object) = body.as_object_mut() {
            object.insert(
                "file_touch_retention".to_owned(),
                serde_json::json!({
                    "observed": file_touch_count,
                    "retained": file_touches.len(),
                    "truncated": true,
                }),
            );
        }
    }
    let content_digest = record
        .content_digest
        .clone()
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode retained projection is missing its snapshot-local digest",
        ))?;
    Ok(OpenCodeNativeEvent {
        native_identity: record.native_identity.clone(),
        message_identity: record.message_identity.clone(),
        session_identity: record.source_session_identity.clone(),
        native_order: record.native_order.clone(),
        kind,
        role,
        provider_event_index: record.native_ordinal,
        time_created,
        time_updated: record.time_updated,
        searchable_text,
        body,
        content_digest,
        file_touches,
        locator: record.locator.clone(),
    })
}

fn retained_event_kind(effective_type: &str, role: &str, body: &Value) -> OpenCodeNativeEventKind {
    if body.get("result_outcome").is_some() {
        if effective_type == "shell" || body.get("command").is_some() {
            return OpenCodeNativeEventKind::CommandOutput;
        }
        return OpenCodeNativeEventKind::ToolOutput;
    }
    if matches!(
        effective_type,
        "tool" | "tool_call" | "tool-call" | "tool_use" | "tooluse"
    ) || json_contains_tool_call(body)
    {
        OpenCodeNativeEventKind::ToolCall
    } else if matches!(effective_type, "reasoning" | "summary") {
        OpenCodeNativeEventKind::Summary
    } else if matches!(role, "user" | "assistant")
        || matches!(effective_type, "user" | "assistant" | "text")
    {
        OpenCodeNativeEventKind::Message
    } else {
        OpenCodeNativeEventKind::Notice
    }
}

fn json_contains_tool_call(body: &Value) -> bool {
    body.get("tool_calls").is_some()
        || body.get("toolCall").is_some()
        || body.get("tool_call").is_some()
        || body
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    matches!(
                        block.get("type").and_then(Value::as_str),
                        Some("tool" | "tool_use" | "toolCall" | "tool_call")
                    )
                })
            })
}

fn retained_searchable_text(
    kind: OpenCodeNativeEventKind,
    effective_type: &str,
    body: &Value,
) -> String {
    if let Some(text) = body.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = body.get("summary").and_then(Value::as_str) {
        return text.to_owned();
    }
    if kind == OpenCodeNativeEventKind::ToolCall {
        let tool = body
            .get("tool")
            .or_else(|| body.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let command = body
            .pointer("/state/input/command")
            .or_else(|| body.pointer("/input/command"))
            .or_else(|| body.get("command"))
            .and_then(Value::as_str);
        return command.map_or_else(
            || format!("tool call: {tool}"),
            |command| format!("{tool}\n{command}"),
        );
    }
    if let Some(content) = body.get("content") {
        let text = collect_text(content);
        if !text.is_empty() {
            return text;
        }
    }
    format!("OpenCode {effective_type} event")
}

fn collect_text(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .filter_map(|value| value.get("text").and_then(Value::as_str).map(str::to_owned))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn retained_file_touches(
    kind: OpenCodeNativeEventKind,
    body: &Value,
) -> (Vec<OpenCodeNativeFileTouch>, usize) {
    if !matches!(
        kind,
        OpenCodeNativeEventKind::ToolCall | OpenCodeNativeEventKind::Notice
    ) {
        return (Vec::new(), 0);
    }
    let mut paths = BTreeSet::new();
    for pointer in [
        "/path",
        "/file_path",
        "/filePath",
        "/input/path",
        "/input/file_path",
        "/state/input/path",
        "/state/input/file_path",
    ] {
        if let Some(path) = body.pointer(pointer).and_then(Value::as_str) {
            if !path.trim().is_empty() {
                paths.insert(path.to_owned());
            }
        }
    }
    if let Some(files) = body.get("files").and_then(Value::as_array) {
        for file in files {
            let path = file
                .as_str()
                .or_else(|| file.get("path").and_then(Value::as_str));
            if let Some(path) = path.filter(|path| !path.trim().is_empty()) {
                paths.insert(path.to_owned());
            }
        }
    }
    let observed = paths.len();
    let retained = paths
        .into_iter()
        .take(OPENCODE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT)
        .map(|path| OpenCodeNativeFileTouch { path })
        .collect();
    (retained, observed)
}

fn rejection(
    record: &RecordMetadata,
    kind: OpenCodeNativeRejectionKind,
    reason: String,
) -> OpenCodeNativeRejection {
    OpenCodeNativeRejection {
        native_identity: record.native_identity.clone(),
        session_identity: Some(record.source_session_identity.clone()),
        native_order: Some(record.native_order.clone()),
        kind,
        reason,
    }
}

fn hash_order(hasher: &mut Sha256, order: &OpenCodeNativeOrder) {
    match order {
        OpenCodeNativeOrder::ExplicitSequence {
            session_id,
            sequence,
            message_id,
        } => {
            hasher.update([1]);
            hash_str(hasher, session_id);
            hasher.update(sequence.to_le_bytes());
            hash_str(hasher, message_id);
        }
        OpenCodeNativeOrder::SynthesizedSequence {
            session_id,
            time_created,
            message_id,
        } => {
            hasher.update([2]);
            hash_str(hasher, session_id);
            hasher.update(time_created.to_le_bytes());
            hash_str(hasher, message_id);
        }
        OpenCodeNativeOrder::MessagePart {
            session_id,
            message_time_created,
            message_id,
            part_time_created,
            part_id,
        } => {
            hasher.update([3]);
            hash_str(hasher, session_id);
            hasher.update(message_time_created.to_le_bytes());
            hash_str(hasher, message_id);
            hasher.update(part_time_created.to_le_bytes());
            hash_str(hasher, part_id);
        }
    }
}

fn hash_str(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn hash_optional_str(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_str(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_u32(hasher: &mut Sha256, value: Option<u32>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}
