use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CaptureError, OutputAssociations, OutputNativeCoordinate, OutputObservationKind, OutputOutcome,
    OutputSourceLocator, ProOutputObservation, Result,
};

use super::{
    lifecycle::GooseNativeInventorySummary,
    metrics::GooseNativeMetrics,
    normalization::{
        goose_normalized_result_content, goose_output_projection, goose_timestamp,
        normalize_goose_native_message, normalize_goose_native_output_diagnostic, GooseNativeEvent,
        GooseNativeExcludedOutput, GooseNativeRejection, GooseNativeRejectionKind,
        GooseNativeSession,
    },
    position::{goose_message_locator, GooseNativeScanPhase, GooseNativeScanPosition},
    schema::{GooseNativeSchema, GooseSessionRow},
    source::{GooseLiveObservation, GooseNativePhysicalSourceIdentity, GooseSnapshotGeneration},
    stream::{
        goose_fetch_native_message_page, goose_fetch_native_output_page,
        goose_fetch_native_session_page, goose_has_any_native_message,
        goose_has_native_message_after, goose_has_native_output_after,
        goose_has_native_session_after, goose_prepare_native_identity_index,
        GooseMessageCellDisposition, GooseNativePageLimits, GooseScannedMessage,
        GooseScannedOutput,
    },
};

const GOOSE_SEMANTIC_DIGEST_DOMAIN: &[u8] = b"ctx-goose-nativepath-semantic-v1\0";
const GOOSE_SESSION_INVENTORY_DIGEST_DOMAIN: &[u8] = b"ctx-goose-nativepath-session-inventory-v1\0";
const GOOSE_SESSION_SAMPLE_DIGEST_DOMAIN: &[u8] = b"ctx-goose-nativepath-session-sample-v1\0";
const GOOSE_SESSION_IDENTITY_SAMPLE_LIMIT: usize = 8;
const GOOSE_CORE_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx-goose-nativepath-core-page-v1\0";
const GOOSE_PRO_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx-goose-nativepath-pro-page-v1\0";
const GOOSE_EVENT_DIGEST_DOMAIN: &[u8] = b"ctx-goose-nativepath-event-v1\0";
const GOOSE_SESSION_DIGEST_DOMAIN: &[u8] = b"ctx-goose-nativepath-session-v1\0";
const GOOSE_PAGE_FIXED_BYTES: usize = 2 * 1024;
const GOOSE_INVENTORY_OBSERVATION_TOKEN_MAX_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum GooseNativeProfile {
    #[default]
    CoreOnly,
    CoreAndPro,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GooseNativeProFrontier {
    pub(super) last_output_rowid: Option<i64>,
    pub(super) output_rows_seen: u64,
    pub(super) terminal: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct GooseNativePageAccounting {
    pub(super) logical_units: usize,
    pub(super) conservative_serialized_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct GooseNativePageIdentity(pub(super) [u8; 32]);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct GooseNativeProPageIdentity(pub(super) [u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GooseNativeProRejectionKind {
    MalformedOutput,
    OversizedOutput,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct GooseNativeProRejection {
    pub(super) sqlite_rowid: i64,
    pub(super) native_identity: String,
    pub(super) kind: GooseNativeProRejectionKind,
    pub(super) reason: String,
    pub(super) locator: OutputSourceLocator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum GooseNativeSourceAuthority {
    ExactDispatchedDatabase {
        path: PathBuf,
        inventory_observation_token: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GooseNativeSourceSelection {
    selected_path: PathBuf,
    inventory_observation_token: Option<String>,
}

impl GooseNativeSourceSelection {
    pub(super) fn exact(selected_path: impl Into<PathBuf>) -> Self {
        Self {
            selected_path: selected_path.into(),
            inventory_observation_token: None,
        }
    }

    pub(super) fn with_inventory_observation_token(
        mut self,
        inventory_observation_token: Option<String>,
    ) -> Self {
        self.inventory_observation_token = inventory_observation_token;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct GooseNativePage {
    pub(super) identity: GooseNativePageIdentity,
    pub(super) source_authority: GooseNativeSourceAuthority,
    pub(super) expected_frontier: GooseNativeScanPosition,
    pub(super) next_frontier: GooseNativeScanPosition,
    pub(super) terminal: bool,
    pub(super) accounting: GooseNativePageAccounting,
    pub(super) position: GooseNativeScanPosition,
    pub(super) sessions: Vec<GooseNativeSession>,
    pub(super) events: Vec<GooseNativeEvent>,
    pub(super) excluded_outputs: Vec<GooseNativeExcludedOutput>,
    pub(super) rejections: Vec<GooseNativeRejection>,
}

#[derive(Debug)]
pub(super) struct GooseNativeProOutputPage {
    pub(super) identity: GooseNativeProPageIdentity,
    pub(super) expected_frontier: GooseNativeProFrontier,
    pub(super) next_frontier: GooseNativeProFrontier,
    pub(super) terminal: bool,
    pub(super) accounting: GooseNativePageAccounting,
    pub(super) observations: Vec<ProOutputObservation>,
    pub(super) rejections: Vec<GooseNativeProRejection>,
}

#[derive(Clone, Debug)]
pub(super) struct GooseNativeScanSummary {
    pub(super) source_authority: GooseNativeSourceAuthority,
    pub(super) raw_generation_digest: String,
    pub(super) capability_digest: String,
    pub(super) semantic_digest: String,
    pub(super) physical_source_identity: GooseNativePhysicalSourceIdentity,
    pub(super) completed_inventory_token: Option<String>,
    pub(super) complete: bool,
    pub(super) profile: GooseNativeProfile,
    pub(super) position: GooseNativeScanPosition,
    pub(super) pro_frontier: GooseNativeProFrontier,
    pub(super) inventory: GooseNativeInventorySummary,
    #[cfg(test)]
    pub(super) metrics: GooseNativeMetrics,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GooseNativeProReplaySummary {
    pub(super) source_authority: GooseNativeSourceAuthority,
    pub(super) raw_generation_digest: String,
    pub(super) capability_digest: String,
    pub(super) frontier: GooseNativeProFrontier,
    pub(super) complete: bool,
}

pub(super) struct GooseNativePathReader {
    snapshot: GooseSnapshotGeneration,
    schema: GooseNativeSchema,
    authority: GooseNativeSourceAuthority,
}

impl GooseNativePathReader {
    pub(super) fn acquire(selection: GooseNativeSourceSelection) -> Result<Self> {
        if selection
            .inventory_observation_token
            .as_ref()
            .is_some_and(|token| token.len() > GOOSE_INVENTORY_OBSERVATION_TOKEN_MAX_BYTES)
        {
            return Err(CaptureError::InvalidPayload(
                "Goose inventory observation token exceeds 4 KiB".to_owned(),
            ));
        }
        let snapshot = GooseSnapshotGeneration::acquire(&selection.selected_path)?;
        let schema = GooseNativeSchema::probe(snapshot.connection())?;
        let authority = GooseNativeSourceAuthority::ExactDispatchedDatabase {
            path: snapshot.observation().source_path().to_path_buf(),
            inventory_observation_token: selection.inventory_observation_token,
        };
        Ok(Self {
            snapshot,
            schema,
            authority,
        })
    }

    #[cfg(test)]
    pub(super) fn scanner(&self, limits: GooseNativePageLimits) -> Result<GooseNativeScanner<'_>> {
        self.scanner_with_profile(GooseNativeProfile::CoreOnly, limits)
    }

    pub(super) fn scanner_with_profile(
        &self,
        profile: GooseNativeProfile,
        limits: GooseNativePageLimits,
    ) -> Result<GooseNativeScanner<'_>> {
        let limits = GooseNativePageLimits::new(limits.rows, limits.retained_bytes)?;
        GooseNativeScanner::new(
            &self.schema,
            &self.snapshot,
            self.authority.clone(),
            limits,
            profile,
        )
    }

    pub(super) fn revalidate_live(&self) -> Result<bool> {
        self.snapshot.revalidate_live()
    }

    pub(super) fn source_observation(&self) -> &GooseLiveObservation {
        self.snapshot.observation()
    }

    #[cfg(test)]
    pub(super) fn snapshot_path(&self) -> &std::path::Path {
        self.snapshot.snapshot_path()
    }

    pub(super) fn schema(&self) -> &GooseNativeSchema {
        &self.schema
    }

    pub(super) fn snapshot_connection(&self) -> &Connection {
        self.snapshot.connection()
    }
}

pub(super) struct GooseNativeScanner<'connection> {
    conn: &'connection Connection,
    schema: &'connection GooseNativeSchema,
    snapshot: &'connection GooseSnapshotGeneration,
    limits: GooseNativePageLimits,
    profile: GooseNativeProfile,
    authority: GooseNativeSourceAuthority,
    raw_generation_digest: String,
    position: GooseNativeScanPosition,
    pro_frontier: GooseNativeProFrontier,
    session_inventory_hasher: Sha256,
    session_identity_samples: Vec<String>,
    semantic_hasher: Sha256,
    metrics: GooseNativeMetrics,
    #[cfg(test)]
    core_pages_emitted: u64,
    #[cfg(test)]
    core_resumed: bool,
    core_certified: bool,
}

impl<'connection> GooseNativeScanner<'connection> {
    fn new(
        schema: &'connection GooseNativeSchema,
        snapshot: &'connection GooseSnapshotGeneration,
        authority: GooseNativeSourceAuthority,
        limits: GooseNativePageLimits,
        profile: GooseNativeProfile,
    ) -> Result<Self> {
        let conn = snapshot.connection();
        goose_prepare_native_identity_index(conn, schema)?;
        let mut semantic_hasher = Sha256::new();
        semantic_hasher.update(GOOSE_SEMANTIC_DIGEST_DOMAIN);
        let mut session_inventory_hasher = Sha256::new();
        session_inventory_hasher.update(GOOSE_SESSION_INVENTORY_DIGEST_DOMAIN);
        Ok(Self {
            conn,
            schema,
            snapshot,
            limits,
            profile,
            authority,
            raw_generation_digest: snapshot.observation().generation_digest(),
            position: GooseNativeScanPosition::initial(),
            pro_frontier: GooseNativeProFrontier::default(),
            session_inventory_hasher,
            session_identity_samples: Vec::with_capacity(GOOSE_SESSION_IDENTITY_SAMPLE_LIMIT),
            semantic_hasher,
            metrics: GooseNativeMetrics {
                snapshot_attempts: snapshot.attempts(),
                identity_prescan_queries: 1,
                ..GooseNativeMetrics::default()
            },
            #[cfg(test)]
            core_pages_emitted: 0,
            #[cfg(test)]
            core_resumed: false,
            core_certified: false,
        })
    }

    pub(super) fn next_page(&mut self) -> Result<Option<GooseNativePage>> {
        if self.core_certified {
            return Ok(None);
        }
        loop {
            match self.position.phase {
                GooseNativeScanPhase::Sessions => {
                    let expected_frontier = self.position;
                    self.metrics.session_page_queries =
                        self.metrics.session_page_queries.saturating_add(1);
                    let rows = goose_fetch_native_session_page(
                        self.conn,
                        self.schema,
                        self.position.keyset,
                        self.limits,
                    )?;
                    if rows.is_empty() {
                        self.position = self.position.start_messages();
                        continue;
                    }
                    let mut page = self.empty_page(expected_frontier);
                    for scanned in rows {
                        self.position = self.position.advance(scanned.sqlite_rowid);
                        self.position.native_rows_seen =
                            self.position.native_rows_seen.saturating_add(1);
                        self.metrics.native_sessions =
                            self.metrics.native_sessions.saturating_add(1);
                        let bounded_identity = scanned
                            .bounded_native_identity
                            .filter(|identity| !identity.trim().is_empty())
                            .unwrap_or_else(|| {
                                format!("goose-session-rowid:{}", scanned.sqlite_rowid)
                            });
                        self.hash_session_inventory(scanned.sqlite_rowid, &bounded_identity);
                        let Some(row) = scanned.row else {
                            let rejection = GooseNativeRejection {
                                sqlite_rowid: scanned.sqlite_rowid,
                                native_order: None,
                                native_identity: bounded_identity.clone(),
                                session_identity: None,
                                kind: GooseNativeRejectionKind::OversizedSession,
                                reason: format!(
                                    "Goose session row {} has {} bytes and exceeds the bounded Core page",
                                    scanned.sqlite_rowid, scanned.observed_bytes
                                ),
                            };
                            self.hash_rejection(&rejection);
                            self.metrics.rejected_records =
                                self.metrics.rejected_records.saturating_add(1);
                            page.rejections.push(rejection);
                            continue;
                        };
                        if row.id.trim().is_empty() {
                            let rejection = GooseNativeRejection {
                                sqlite_rowid: scanned.sqlite_rowid,
                                native_order: None,
                                native_identity: format!("sessions.rowid:{}", scanned.sqlite_rowid),
                                session_identity: None,
                                kind: GooseNativeRejectionKind::EmptySessionIdentity,
                                reason: "Goose session has an empty native identity".to_owned(),
                            };
                            self.hash_rejection(&rejection);
                            self.metrics.rejected_records =
                                self.metrics.rejected_records.saturating_add(1);
                            page.rejections.push(rejection);
                            continue;
                        }
                        let native_identity = row.id.clone();
                        let session = GooseNativeSession {
                            sqlite_rowid: scanned.sqlite_rowid,
                            native_identity: native_identity.clone(),
                            row,
                        };
                        self.hash_session(&session);
                        page.sessions.push(session);
                    }
                    let last_rowid = self.position.keyset.bound();
                    if !goose_has_native_session_after(self.conn, last_rowid)? {
                        self.position = self.position.start_messages();
                        if !goose_has_any_native_message(self.conn)? {
                            self.position = self.position.complete();
                        }
                    }
                    page.position = self.position;
                    page.next_frontier = self.position;
                    finalize_core_page(
                        &mut page,
                        self.raw_generation_digest.as_bytes(),
                        self.limits,
                    )?;
                    #[cfg(test)]
                    {
                        self.core_pages_emitted = self.core_pages_emitted.saturating_add(1);
                    }
                    return Ok(Some(page));
                }
                GooseNativeScanPhase::Messages => {
                    let expected_frontier = self.position;
                    self.metrics.message_page_queries =
                        self.metrics.message_page_queries.saturating_add(1);
                    let rows = goose_fetch_native_message_page(
                        self.conn,
                        self.schema,
                        self.position.keyset,
                        self.limits,
                    )?;
                    if rows.is_empty() {
                        self.position = self.position.complete();
                        return Ok(None);
                    }
                    let mut page = self.empty_page(expected_frontier);
                    for scanned in rows {
                        self.position = self.position.advance(scanned.sqlite_rowid);
                        self.position.native_rows_seen =
                            self.position.native_rows_seen.saturating_add(1);
                        self.metrics.native_messages =
                            self.metrics.native_messages.saturating_add(1);
                        self.project_message(scanned, &mut page)?;
                    }
                    let last_rowid = self.position.keyset.bound();
                    if !goose_has_native_message_after(self.conn, last_rowid)? {
                        self.position = self.position.complete();
                    }
                    page.position = self.position;
                    page.next_frontier = self.position;
                    finalize_core_page(
                        &mut page,
                        self.raw_generation_digest.as_bytes(),
                        self.limits,
                    )?;
                    #[cfg(test)]
                    {
                        self.core_pages_emitted = self.core_pages_emitted.saturating_add(1);
                    }
                    return Ok(Some(page));
                }
                GooseNativeScanPhase::Complete => return Ok(None),
            }
        }
    }

    #[cfg(test)]
    pub(super) fn summary(&self) -> GooseNativeScanSummary {
        self.build_summary(false)
    }

    pub(super) fn finish_core(&mut self) -> Result<GooseNativeScanSummary> {
        if self.position.phase != GooseNativeScanPhase::Complete {
            return Err(CaptureError::InvalidPayload(
                "Goose NativePath Core scan must be exhausted before finish".to_owned(),
            ));
        }
        #[cfg(test)]
        if self.core_resumed {
            return Err(CaptureError::InvalidPayload(
                "Goose NativePath retry scanner cannot certify a partial generation".to_owned(),
            ));
        }
        if !self.snapshot.revalidate_live()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        self.core_certified = true;
        Ok(self.build_summary(true))
    }

    #[cfg(test)]
    pub(super) fn resume_core_from(&mut self, frontier: GooseNativeScanPosition) -> Result<()> {
        if self.position != GooseNativeScanPosition::initial() || self.core_pages_emitted != 0 {
            return Err(CaptureError::InvalidPayload(
                "Goose Core retry frontier must be installed before reading pages".to_owned(),
            ));
        }
        if frontier.phase == GooseNativeScanPhase::Complete
            || (frontier.keyset == super::position::GooseNativeRowKeyset::Unstarted
                && frontier.phase == GooseNativeScanPhase::Sessions
                && frontier.native_rows_seen != 0)
        {
            return Err(CaptureError::InvalidPayload(
                "Goose Core retry frontier is inconsistent".to_owned(),
            ));
        }
        self.position = frontier;
        self.core_resumed = frontier != GooseNativeScanPosition::initial();
        Ok(())
    }

    pub(super) fn next_pro_output_page(&mut self) -> Result<Option<GooseNativeProOutputPage>> {
        if self.profile == GooseNativeProfile::CoreOnly || self.pro_frontier.terminal {
            return Ok(None);
        }
        let expected_frontier = self.pro_frontier;
        let keyset = self
            .pro_frontier
            .last_output_rowid
            .map_or(super::position::GooseNativeRowKeyset::Unstarted, |rowid| {
                super::position::GooseNativeRowKeyset::After(rowid)
            });
        let rows = goose_fetch_native_output_page(self.conn, self.schema, keyset, self.limits)?;
        if rows.is_empty() {
            self.pro_frontier.terminal = true;
            return Ok(None);
        }
        let mut observations = Vec::new();
        let mut rejections = Vec::new();
        for output in rows {
            let rowid = output.sqlite_rowid;
            let native_identity = output.native_identity.clone();
            let locator = goose_output_locator(rowid)?;
            self.pro_frontier.last_output_rowid = Some(rowid);
            self.pro_frontier.output_rows_seen =
                self.pro_frontier.output_rows_seen.saturating_add(1);
            match project_goose_pro_output(output, locator.clone()) {
                Ok(observation) => {
                    self.metrics.output_content_cells_transferred = self
                        .metrics
                        .output_content_cells_transferred
                        .saturating_add(1);
                    self.metrics.output_content_bytes_transferred = self
                        .metrics
                        .output_content_bytes_transferred
                        .saturating_add(observation.content.len() as u64);
                    self.metrics.output_handoffs_built =
                        self.metrics.output_handoffs_built.saturating_add(1);
                    observations.push(observation);
                }
                Err((kind, reason)) => {
                    self.metrics.pro_output_rejections =
                        self.metrics.pro_output_rejections.saturating_add(1);
                    rejections.push(GooseNativeProRejection {
                        sqlite_rowid: rowid,
                        native_identity,
                        kind,
                        reason,
                        locator,
                    });
                }
            }
        }
        let last_rowid =
            self.pro_frontier
                .last_output_rowid
                .ok_or(CaptureError::SystemInvariant(
                    "Goose nonempty Pro page lost its final rowid",
                ))?;
        self.pro_frontier.terminal = !goose_has_native_output_after(self.conn, last_rowid)?;
        let mut page = GooseNativeProOutputPage {
            identity: GooseNativeProPageIdentity::default(),
            expected_frontier,
            next_frontier: self.pro_frontier,
            terminal: self.pro_frontier.terminal,
            accounting: GooseNativePageAccounting::default(),
            observations,
            rejections,
        };
        finalize_pro_page(
            &mut page,
            self.raw_generation_digest.as_bytes(),
            self.limits,
        )?;
        self.metrics.pro_output_pages = self.metrics.pro_output_pages.saturating_add(1);
        Ok(Some(page))
    }

    pub(super) fn resume_pro_from(&mut self, frontier: GooseNativeProFrontier) -> Result<()> {
        if self.profile != GooseNativeProfile::CoreAndPro {
            return Err(CaptureError::InvalidPayload(
                "Goose Pro replay requires the CoreAndPro profile".to_owned(),
            ));
        }
        if self.pro_frontier != GooseNativeProFrontier::default() {
            return Err(CaptureError::InvalidPayload(
                "Goose Pro replay frontier must be installed before reading output pages"
                    .to_owned(),
            ));
        }
        if frontier.last_output_rowid.is_none() && frontier.output_rows_seen != 0 {
            return Err(CaptureError::InvalidPayload(
                "Goose Pro replay frontier is inconsistent".to_owned(),
            ));
        }
        self.pro_frontier = frontier;
        Ok(())
    }

    pub(super) fn finish_pro_replay(&self) -> Result<GooseNativeProReplaySummary> {
        if self.profile != GooseNativeProfile::CoreAndPro || !self.pro_frontier.terminal {
            return Err(CaptureError::InvalidPayload(
                "Goose Pro replay must exhaust its exact output frontier before finish".to_owned(),
            ));
        }
        if !self.snapshot.revalidate_live()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(GooseNativeProReplaySummary {
            source_authority: self.authority.clone(),
            raw_generation_digest: self.raw_generation_digest.clone(),
            capability_digest: self.schema.capability_digest.clone(),
            frontier: self.pro_frontier,
            complete: true,
        })
    }

    fn build_summary(&self, certified: bool) -> GooseNativeScanSummary {
        let semantic_digest: [u8; 32] = self.semantic_hasher.clone().finalize().into();
        let session_identity_digest: [u8; 32] =
            self.session_inventory_hasher.clone().finalize().into();
        let completed_inventory_token = if certified {
            match &self.authority {
                GooseNativeSourceAuthority::ExactDispatchedDatabase {
                    inventory_observation_token,
                    ..
                } => inventory_observation_token
                    .as_deref()
                    .filter(|token| !token.trim().is_empty())
                    .map(str::to_owned),
            }
        } else {
            None
        };
        GooseNativeScanSummary {
            source_authority: self.authority.clone(),
            raw_generation_digest: self.raw_generation_digest.clone(),
            capability_digest: self.schema.capability_digest.clone(),
            semantic_digest: goose_hex_digest(semantic_digest),
            physical_source_identity: self.snapshot.observation().physical_source_identity(),
            completed_inventory_token,
            complete: certified,
            profile: self.profile,
            position: self.position,
            pro_frontier: self.pro_frontier,
            inventory: GooseNativeInventorySummary {
                native_session_rows: self.metrics.native_sessions,
                native_message_rows: self.metrics.native_messages,
                session_identity_digest: goose_hex_digest(session_identity_digest),
                session_identity_samples: self.session_identity_samples.clone(),
            },
            #[cfg(test)]
            metrics: self.metrics.clone(),
        }
    }

    fn project_message(
        &mut self,
        scanned: GooseScannedMessage,
        page: &mut GooseNativePage,
    ) -> Result<()> {
        match scanned.disposition {
            GooseMessageCellDisposition::Retained => {
                self.metrics.retained_content_cells_transferred = self
                    .metrics
                    .retained_content_cells_transferred
                    .saturating_add(1);
                self.metrics.retained_content_bytes_transferred = self
                    .metrics
                    .retained_content_bytes_transferred
                    .saturating_add(scanned.content_bytes);
                let raw_content =
                    scanned
                        .content_json
                        .as_deref()
                        .ok_or(CaptureError::SystemInvariant(
                            "Goose retained SQLite row omitted content_json",
                        ))?;
                self.hash_message_header(&scanned, b"retained");
                goose_hash_bytes(&mut self.semantic_hasher, raw_content.as_bytes());
                let rejection_rowid = scanned.sqlite_rowid;
                let rejection_order = scanned.native_order;
                let rejection_identity = scanned.native_identity.clone();
                let rejection_session = scanned.session_identity.clone();
                match normalize_goose_native_message(scanned.into_retained()?) {
                    Ok(event) => {
                        self.metrics.retained_events =
                            self.metrics.retained_events.saturating_add(1);
                        page.events.push(event);
                    }
                    Err(error) => {
                        let rejection = GooseNativeRejection {
                            sqlite_rowid: rejection_rowid,
                            native_order: Some(rejection_order),
                            native_identity: rejection_identity,
                            session_identity: Some(rejection_session),
                            kind: GooseNativeRejectionKind::RetainedParseMismatch,
                            reason: error.to_string(),
                        };
                        self.hash_rejection(&rejection);
                        self.metrics.rejected_records =
                            self.metrics.rejected_records.saturating_add(1);
                        page.rejections.push(rejection);
                    }
                }
            }
            GooseMessageCellDisposition::OutputSuccess
            | GooseMessageCellDisposition::OutputFailure
            | GooseMessageCellDisposition::OutputTimeout
            | GooseMessageCellDisposition::OutputUnknown => {
                let outcome = scanned.output_outcome.ok_or(CaptureError::SystemInvariant(
                    "Goose output row omitted its SQL-classified outcome",
                ))?;
                self.hash_message_header(&scanned, b"output");
                self.semantic_hasher
                    .update([goose_output_outcome_code(outcome) as u8]);
                self.metrics.excluded_outputs = self.metrics.excluded_outputs.saturating_add(1);
                self.metrics.excluded_output_bytes_observed = self
                    .metrics
                    .excluded_output_bytes_observed
                    .saturating_add(scanned.content_bytes);
                match outcome {
                    OutputOutcome::Success => {
                        self.metrics.outputs_success =
                            self.metrics.outputs_success.saturating_add(1)
                    }
                    OutputOutcome::Failure => {
                        self.metrics.outputs_failure =
                            self.metrics.outputs_failure.saturating_add(1)
                    }
                    OutputOutcome::Timeout => {
                        self.metrics.outputs_timeout =
                            self.metrics.outputs_timeout.saturating_add(1)
                    }
                    OutputOutcome::Unknown => {
                        self.metrics.outputs_unknown =
                            self.metrics.outputs_unknown.saturating_add(1)
                    }
                }
                if matches!(outcome, OutputOutcome::Failure | OutputOutcome::Timeout) {
                    if scanned.content_json.is_some() {
                        self.metrics.output_content_cells_transferred = self
                            .metrics
                            .output_content_cells_transferred
                            .saturating_add(1);
                        self.metrics.output_content_bytes_transferred = self
                            .metrics
                            .output_content_bytes_transferred
                            .saturating_add(scanned.content_bytes);
                    }
                    let event = normalize_goose_native_output_diagnostic(&scanned)?;
                    let digest = goose_event_content_digest(&event);
                    self.metrics.output_hashes_built =
                        self.metrics.output_hashes_built.saturating_add(1);
                    self.metrics.output_previews_built =
                        self.metrics.output_previews_built.saturating_add(1);
                    self.metrics.retained_events = self.metrics.retained_events.saturating_add(1);
                    self.semantic_hasher.update(b"output-diagnostic");
                    goose_hash_str(&mut self.semantic_hasher, &digest);
                    page.events.push(event);
                }
            }
            disposition => {
                let kind = goose_rejection_kind(disposition)?;
                let rejection = GooseNativeRejection {
                    sqlite_rowid: scanned.sqlite_rowid,
                    native_order: Some(scanned.native_order),
                    native_identity: scanned.native_identity,
                    session_identity: Some(scanned.session_identity),
                    kind,
                    reason: format!(
                        "Goose message row {} rejected as {}",
                        scanned.sqlite_rowid,
                        kind.as_str()
                    ),
                };
                self.hash_rejection(&rejection);
                self.metrics.rejected_records = self.metrics.rejected_records.saturating_add(1);
                page.rejections.push(rejection);
            }
        }
        Ok(())
    }

    fn empty_page(&self, frontier: GooseNativeScanPosition) -> GooseNativePage {
        GooseNativePage {
            identity: GooseNativePageIdentity::default(),
            source_authority: self.authority.clone(),
            expected_frontier: frontier,
            next_frontier: frontier,
            terminal: false,
            accounting: GooseNativePageAccounting::default(),
            position: self.position,
            sessions: Vec::new(),
            events: Vec::new(),
            excluded_outputs: Vec::new(),
            rejections: Vec::new(),
        }
    }

    fn hash_session(&mut self, session: &GooseNativeSession) {
        self.semantic_hasher.update(b"session");
        goose_hash_str(&mut self.semantic_hasher, &session.native_identity);
        goose_hash_session_row(&mut self.semantic_hasher, &session.row);
    }

    fn hash_session_inventory(&mut self, sqlite_rowid: i64, native_identity: &str) {
        goose_hash_i64(&mut self.session_inventory_hasher, sqlite_rowid);
        goose_hash_str(&mut self.session_inventory_hasher, native_identity);
        if self.session_identity_samples.len() < GOOSE_SESSION_IDENTITY_SAMPLE_LIMIT {
            let mut sample_hasher = Sha256::new();
            sample_hasher.update(GOOSE_SESSION_SAMPLE_DIGEST_DOMAIN);
            goose_hash_str(&mut sample_hasher, native_identity);
            self.session_identity_samples
                .push(goose_hex_digest(sample_hasher.finalize().into()));
        }
    }

    fn hash_message_header(&mut self, message: &GooseScannedMessage, disposition: &[u8]) {
        self.semantic_hasher.update(b"message");
        goose_hash_bytes(&mut self.semantic_hasher, disposition);
        goose_hash_i64(&mut self.semantic_hasher, message.sqlite_rowid);
        goose_hash_i64(&mut self.semantic_hasher, message.native_order);
        goose_hash_str(&mut self.semantic_hasher, &message.native_identity);
        goose_hash_str(&mut self.semantic_hasher, &message.session_identity);
        goose_hash_str(&mut self.semantic_hasher, &message.role);
    }

    fn hash_rejection(&mut self, rejection: &GooseNativeRejection) {
        self.semantic_hasher.update(b"rejection");
        goose_hash_i64(&mut self.semantic_hasher, rejection.sqlite_rowid);
        goose_hash_str(&mut self.semantic_hasher, &rejection.native_identity);
        goose_hash_str(&mut self.semantic_hasher, rejection.kind.as_str());
    }
}

fn goose_rejection_kind(
    disposition: GooseMessageCellDisposition,
) -> Result<GooseNativeRejectionKind> {
    match disposition {
        GooseMessageCellDisposition::MalformedJson => Ok(GooseNativeRejectionKind::MalformedJson),
        GooseMessageCellDisposition::UnsupportedJsonRoot => {
            Ok(GooseNativeRejectionKind::UnsupportedJsonRoot)
        }
        GooseMessageCellDisposition::NonObjectBlock => Ok(GooseNativeRejectionKind::NonObjectBlock),
        GooseMessageCellDisposition::UnknownBlockType => {
            Ok(GooseNativeRejectionKind::UnknownBlockType)
        }
        GooseMessageCellDisposition::DuplicateBlockType => {
            Ok(GooseNativeRejectionKind::DuplicateBlockType)
        }
        GooseMessageCellDisposition::OversizedRetainedContent => {
            Ok(GooseNativeRejectionKind::OversizedRetainedContent)
        }
        GooseMessageCellDisposition::MissingSession => Ok(GooseNativeRejectionKind::MissingSession),
        GooseMessageCellDisposition::UnsupportedStorageClass => {
            Ok(GooseNativeRejectionKind::UnsupportedStorageClass)
        }
        GooseMessageCellDisposition::Retained
        | GooseMessageCellDisposition::OutputSuccess
        | GooseMessageCellDisposition::OutputFailure
        | GooseMessageCellDisposition::OutputTimeout
        | GooseMessageCellDisposition::OutputUnknown => Err(CaptureError::SystemInvariant(
            "Goose retained/output disposition is not a rejection",
        )),
    }
}

fn project_goose_pro_output(
    output: GooseScannedOutput,
    locator: OutputSourceLocator,
) -> std::result::Result<ProOutputObservation, (GooseNativeProRejectionKind, String)> {
    let Some(raw_content) = output.content_json.as_deref() else {
        return Err((
            GooseNativeProRejectionKind::OversizedOutput,
            format!(
                "Goose output {} exceeds the bounded Pro replay page",
                output.native_identity
            ),
        ));
    };
    let content: serde_json::Value = serde_json::from_str(raw_content).map_err(|error| {
        (
            GooseNativeProRejectionKind::MalformedOutput,
            format!(
                "Goose output {} changed classification while parsing: {error}",
                output.native_identity
            ),
        )
    })?;
    let projection = goose_output_projection(&content);
    if projection.outcome.outcome != output.outcome {
        return Err((
            GooseNativeProRejectionKind::MalformedOutput,
            format!(
                "Goose output {} disagrees between SQLite and Rust outcome classification",
                output.native_identity
            ),
        ));
    }
    let occurred_at_unix_ms = output
        .created_timestamp
        .and_then(|seconds| seconds.checked_mul(1_000))
        .or_else(|| {
            output.timestamp.as_deref().and_then(|timestamp| {
                let timestamp = timestamp.trim();
                (!timestamp.is_empty()).then(|| {
                    goose_timestamp(Some(timestamp), DateTime::<Utc>::UNIX_EPOCH).timestamp_millis()
                })
            })
        });
    let native_record_identity = output.provider_message_identity;
    Ok(ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "goose:{}:{}:output",
                output.session_identity, native_record_identity
            ),
            native_sequence: output.source_record_ordinal,
            native_record_id: Some(native_record_identity),
            source_record_ordinal: Some(output.source_record_ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms,
        associations: OutputAssociations {
            direct_session_id: output.session_identity.clone(),
            root_session_id: output.session_identity.clone(),
            parent_session_id: None,
            provider_session_id: Some(output.session_identity),
            agent_id: None,
            repository: None,
        },
        call_id: projection.call_id,
        command: None,
        outcome: projection.outcome,
        locator,
        content: goose_normalized_result_content(&content)
            .unwrap_or_default()
            .into_bytes(),
    })
}

fn goose_output_locator(sqlite_rowid: i64) -> Result<OutputSourceLocator> {
    let (kind, payload) = goose_message_locator(sqlite_rowid);
    Ok(OutputSourceLocator {
        version: 1,
        kind: kind.to_owned(),
        payload,
    })
}

fn goose_output_outcome_code(outcome: OutputOutcome) -> i64 {
    match outcome {
        OutputOutcome::Success => 1,
        OutputOutcome::Failure => 2,
        OutputOutcome::Timeout => 3,
        OutputOutcome::Unknown => 4,
    }
}

fn goose_session_content_digest(session: &GooseNativeSession) -> String {
    let mut hasher = Sha256::new();
    hasher.update(GOOSE_SESSION_DIGEST_DOMAIN);
    goose_hash_str(&mut hasher, &session.native_identity);
    goose_hash_session_row(&mut hasher, &session.row);
    goose_hex_digest(hasher.finalize().into())
}

fn goose_event_content_digest(event: &GooseNativeEvent) -> String {
    let mut hasher = Sha256::new();
    hasher.update(GOOSE_EVENT_DIGEST_DOMAIN);
    goose_hash_i64(&mut hasher, event.native_order);
    goose_hash_str(&mut hasher, &event.native_identity);
    goose_hash_str(&mut hasher, &event.provider_message_identity);
    goose_hash_str(&mut hasher, &event.session_identity);
    goose_hash_str(&mut hasher, &event.role);
    goose_hash_str(&mut hasher, &event.content.to_string());
    goose_hash_str(&mut hasher, &event.searchable_text);
    goose_hash_optional_i64(&mut hasher, event.created_timestamp);
    goose_hash_optional_str(&mut hasher, event.timestamp.as_deref());
    goose_hash_optional_str(&mut hasher, event.tokens_json.as_deref());
    goose_hash_optional_str(&mut hasher, event.metadata_json.as_deref());
    for touch in &event.file_touches {
        goose_hash_str(&mut hasher, &touch.path);
        goose_hash_optional_str(&mut hasher, touch.old_path.as_deref());
        goose_hash_str(&mut hasher, touch.evidence);
    }
    goose_hex_digest(hasher.finalize().into())
}

fn finalize_core_page(
    page: &mut GooseNativePage,
    generation_digest: &[u8],
    limits: GooseNativePageLimits,
) -> Result<()> {
    if !page.excluded_outputs.is_empty() {
        return Err(CaptureError::SystemInvariant(
            "Goose Core page contains an output marker",
        ));
    }
    page.terminal = page.next_frontier.phase == GooseNativeScanPhase::Complete;
    let logical_units = usize::try_from(
        page.next_frontier
            .native_rows_seen
            .saturating_sub(page.expected_frontier.native_rows_seen),
    )
    .unwrap_or(usize::MAX);
    let conservative_serialized_bytes = goose_core_page_encoded_bytes(page);
    validate_goose_page_bounds(logical_units, conservative_serialized_bytes, limits, "Core")?;
    page.accounting = GooseNativePageAccounting {
        logical_units,
        conservative_serialized_bytes,
    };
    let mut hasher = Sha256::new();
    hasher.update(GOOSE_CORE_PAGE_IDENTITY_DOMAIN);
    goose_hash_bytes(&mut hasher, generation_digest);
    goose_hash_position(&mut hasher, page.expected_frontier);
    goose_hash_position(&mut hasher, page.next_frontier);
    hasher.update([u8::from(page.terminal)]);
    for session in &page.sessions {
        hasher.update(b"session");
        goose_hash_str(&mut hasher, &session.native_identity);
        goose_hash_str(&mut hasher, &goose_session_content_digest(session));
    }
    for event in &page.events {
        hasher.update(b"event");
        goose_hash_str(&mut hasher, &event.native_identity);
        goose_hash_str(&mut hasher, &goose_event_content_digest(event));
    }
    for rejection in &page.rejections {
        hasher.update(b"rejection");
        goose_hash_i64(&mut hasher, rejection.sqlite_rowid);
        goose_hash_str(&mut hasher, &rejection.native_identity);
        goose_hash_str(&mut hasher, rejection.kind.as_str());
        goose_hash_str(&mut hasher, &rejection.reason);
    }
    page.identity = GooseNativePageIdentity(hasher.finalize().into());
    Ok(())
}

fn finalize_pro_page(
    page: &mut GooseNativeProOutputPage,
    generation_digest: &[u8],
    limits: GooseNativePageLimits,
) -> Result<()> {
    let logical_units = page
        .observations
        .len()
        .saturating_add(page.rejections.len());
    let conservative_serialized_bytes = goose_pro_page_encoded_bytes(page);
    validate_goose_page_bounds(logical_units, conservative_serialized_bytes, limits, "Pro")?;
    let mut hasher = Sha256::new();
    hasher.update(GOOSE_PRO_PAGE_IDENTITY_DOMAIN);
    goose_hash_bytes(&mut hasher, generation_digest);
    goose_hash_pro_frontier(&mut hasher, page.expected_frontier);
    goose_hash_pro_frontier(&mut hasher, page.next_frontier);
    hasher.update([u8::from(page.terminal)]);
    for output in &page.observations {
        hasher.update(b"output");
        goose_hash_str(&mut hasher, &output.coordinate.unit_key);
        goose_hash_bytes(&mut hasher, &output.content);
        goose_hash_str(&mut hasher, &output.locator.kind);
        goose_hash_bytes(&mut hasher, &output.locator.payload);
        hasher.update([goose_output_outcome_code(output.outcome.outcome) as u8]);
    }
    for rejection in &page.rejections {
        hasher.update(b"rejection");
        goose_hash_i64(&mut hasher, rejection.sqlite_rowid);
        goose_hash_str(&mut hasher, &rejection.native_identity);
        goose_hash_str(&mut hasher, &rejection.reason);
    }
    page.identity = GooseNativeProPageIdentity(hasher.finalize().into());
    page.accounting = GooseNativePageAccounting {
        logical_units,
        conservative_serialized_bytes,
    };
    Ok(())
}

fn validate_goose_page_bounds(
    logical_units: usize,
    bytes: usize,
    limits: GooseNativePageLimits,
    lane: &str,
) -> Result<()> {
    if logical_units == 0 || logical_units > limits.rows {
        return Err(CaptureError::InvalidPayload(format!(
            "Goose NativePath {lane} page has {logical_units} logical units"
        )));
    }
    let byte_limit = usize::try_from(limits.retained_bytes).unwrap_or(usize::MAX);
    if bytes > byte_limit {
        return Err(CaptureError::InvalidPayload(format!(
            "Goose NativePath {lane} page has {bytes} conservatively encoded bytes"
        )));
    }
    Ok(())
}

#[derive(Default)]
struct GooseEncodedByteCounter {
    bytes: usize,
}

impl GooseEncodedByteCounter {
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
}

fn goose_core_page_encoded_bytes(page: &GooseNativePage) -> usize {
    let mut counter = GooseEncodedByteCounter {
        bytes: GOOSE_PAGE_FIXED_BYTES,
    };
    for session in &page.sessions {
        counter.fixed(8);
        counter.string(&session.native_identity);
        counter.string(&format!("{:?}", session.row));
    }
    for event in &page.events {
        counter.fixed(8 * 3 + 4);
        counter.string(&event.native_identity);
        counter.string(&event.session_identity);
        counter.string(&event.role);
        counter.string(&event.content.to_string());
        counter.string(&event.searchable_text);
        counter.optional_string(event.timestamp.as_deref());
        counter.optional_string(event.tokens_json.as_deref());
        counter.optional_string(event.metadata_json.as_deref());
        for touch in &event.file_touches {
            counter.string(&touch.path);
            counter.optional_string(touch.old_path.as_deref());
            counter.string(touch.evidence);
        }
    }
    for rejection in &page.rejections {
        counter.fixed(8 * 2 + 4);
        counter.string(&rejection.native_identity);
        counter.optional_string(rejection.session_identity.as_deref());
        counter.string(&rejection.reason);
    }
    counter.bytes
}

fn goose_pro_page_encoded_bytes(page: &GooseNativeProOutputPage) -> usize {
    let mut counter = GooseEncodedByteCounter {
        bytes: GOOSE_PAGE_FIXED_BYTES,
    };
    for output in &page.observations {
        counter.fixed(8 * 5 + 4 * 3);
        counter.string(&output.coordinate.unit_key);
        counter.optional_string(output.coordinate.native_record_id.as_deref());
        counter.string(&output.associations.direct_session_id);
        counter.string(&output.associations.root_session_id);
        counter.optional_string(output.associations.provider_session_id.as_deref());
        counter.optional_string(output.call_id.as_deref());
        counter.string(&output.locator.kind);
        counter.bytes(&output.locator.payload);
        counter.bytes(&output.content);
    }
    for rejection in &page.rejections {
        counter.fixed(8 + 4);
        counter.string(&rejection.native_identity);
        counter.string(&rejection.reason);
        counter.string(&rejection.locator.kind);
        counter.bytes(&rejection.locator.payload);
    }
    counter.bytes
}

fn goose_hash_position(hasher: &mut Sha256, position: GooseNativeScanPosition) {
    hasher.update([match position.phase {
        GooseNativeScanPhase::Sessions => 1,
        GooseNativeScanPhase::Messages => 2,
        GooseNativeScanPhase::Complete => 3,
    }]);
    match position.keyset {
        super::position::GooseNativeRowKeyset::Unstarted => hasher.update([0]),
        super::position::GooseNativeRowKeyset::After(rowid) => {
            hasher.update([1]);
            goose_hash_i64(hasher, rowid);
        }
    }
    hasher.update(position.native_rows_seen.to_le_bytes());
}

fn goose_hash_pro_frontier(hasher: &mut Sha256, frontier: GooseNativeProFrontier) {
    goose_hash_optional_i64(hasher, frontier.last_output_rowid);
    hasher.update(frontier.output_rows_seen.to_le_bytes());
    hasher.update([u8::from(frontier.terminal)]);
}

fn goose_hash_session_row(hasher: &mut Sha256, row: &GooseSessionRow) {
    goose_hash_str(hasher, &row.id);
    goose_hash_optional_str(hasher, row.name.as_deref());
    goose_hash_optional_str(hasher, row.description.as_deref());
    hasher.update([u8::from(row.user_set_name)]);
    goose_hash_optional_str(hasher, row.session_type.as_deref());
    goose_hash_optional_str(hasher, row.working_dir.as_deref());
    goose_hash_optional_str(hasher, row.created_at.as_deref());
    goose_hash_optional_str(hasher, row.updated_at.as_deref());
    goose_hash_optional_str(hasher, row.extension_data.as_deref());
    for value in [
        row.total_tokens,
        row.input_tokens,
        row.output_tokens,
        row.accumulated_total_tokens,
        row.accumulated_input_tokens,
        row.accumulated_output_tokens,
    ] {
        goose_hash_optional_i64(hasher, value);
    }
    match row.accumulated_cost {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_bits().to_le_bytes());
        }
        None => hasher.update([0]),
    }
    goose_hash_optional_str(hasher, row.provider_name.as_deref());
    goose_hash_optional_str(hasher, row.model_config_json.as_deref());
    goose_hash_optional_str(hasher, row.goose_mode.as_deref());
    goose_hash_optional_str(hasher, row.archived_at.as_deref());
    goose_hash_optional_str(hasher, row.project_id.as_deref());
}

fn goose_hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            goose_hash_i64(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn goose_hash_optional_str(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            goose_hash_str(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn goose_hash_i64(hasher: &mut Sha256, value: i64) {
    hasher.update(value.to_le_bytes());
}

fn goose_hash_str(hasher: &mut Sha256, value: &str) {
    goose_hash_bytes(hasher, value.as_bytes());
}

fn goose_hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn goose_hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
