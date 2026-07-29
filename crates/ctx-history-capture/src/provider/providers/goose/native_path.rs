use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{provider_sources::SqliteSourceReadSnapshot, CaptureError, OutputOutcome, Result};

use super::{
    lifecycle::GooseNativeInventorySummary,
    metrics::GooseNativeMetrics,
    normalization::{
        normalize_goose_native_message, normalize_goose_native_output_diagnostic, GooseNativeEvent,
        GooseNativeExcludedOutput, GooseNativeRejection, GooseNativeRejectionKind,
        GooseNativeSession,
    },
    position::{GooseNativeScanPhase, GooseNativeScanPosition},
    schema::{GooseNativeSchema, GooseSessionRow},
    source::{GooseLiveObservation, GooseNativePhysicalSourceIdentity, GooseSnapshotGeneration},
    stream::{
        goose_fetch_native_message_page, goose_fetch_native_session_page,
        goose_has_any_native_message, goose_has_native_message_after,
        goose_has_native_session_after, goose_prepare_native_identity_index,
        GooseMessageCellDisposition, GooseNativePageLimits, GooseScannedMessage,
    },
};

mod page;
mod projection;

use page::*;
use projection::*;

const GOOSE_SEMANTIC_DIGEST_DOMAIN: &[u8] = b"ctx-goose-nativepath-semantic-v1\0";
const GOOSE_SESSION_INVENTORY_DIGEST_DOMAIN: &[u8] = b"ctx-goose-nativepath-session-inventory-v1\0";
const GOOSE_SESSION_SAMPLE_DIGEST_DOMAIN: &[u8] = b"ctx-goose-nativepath-session-sample-v1\0";
const GOOSE_SESSION_IDENTITY_SAMPLE_LIMIT: usize = 8;
const GOOSE_CORE_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx-goose-nativepath-core-page-v1\0";
const GOOSE_EVENT_DIGEST_DOMAIN: &[u8] = b"ctx-goose-nativepath-event-v1\0";
const GOOSE_SESSION_DIGEST_DOMAIN: &[u8] = b"ctx-goose-nativepath-session-v1\0";
const GOOSE_PAGE_FIXED_BYTES: usize = 2 * 1024;
const GOOSE_INVENTORY_OBSERVATION_TOKEN_MAX_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum GooseNativeProfile {
    #[default]
    CoreOnly,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct GooseNativePageAccounting {
    pub(super) logical_units: usize,
    pub(super) conservative_serialized_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct GooseNativePageIdentity(pub(super) [u8; 32]);

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

#[derive(Clone, Debug)]
pub(super) struct GooseNativeScanSummary {
    pub(super) source_authority: GooseNativeSourceAuthority,
    // Generation, capability, semantic, and physical-position evidence is
    // retained for exact rebuild/Pro consumers beyond the Core summary.
    #[allow(dead_code)]
    pub(super) raw_generation_digest: String,
    #[allow(dead_code)]
    pub(super) capability_digest: String,
    #[allow(dead_code)]
    pub(super) semantic_digest: String,
    #[allow(dead_code)]
    pub(super) physical_source_identity: GooseNativePhysicalSourceIdentity,
    pub(super) completed_inventory_token: Option<String>,
    pub(super) complete: bool,
    #[allow(dead_code)]
    pub(super) position: GooseNativeScanPosition,
    pub(super) inventory: GooseNativeInventorySummary,
    #[cfg(test)]
    pub(super) metrics: GooseNativeMetrics,
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
        let connection = snapshot.connection()?;
        let schema = GooseNativeSchema::probe(snapshot.connection_ref(&connection)?)?;
        if !snapshot.finish_connection(connection)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
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
        _profile: GooseNativeProfile,
        limits: GooseNativePageLimits,
    ) -> Result<GooseNativeScanner<'_>> {
        let limits = GooseNativePageLimits::new(limits.rows, limits.retained_bytes)?;
        GooseNativeScanner::new(&self.schema, &self.snapshot, self.authority.clone(), limits)
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

    pub(super) fn snapshot_connection(&self) -> Result<SqliteSourceReadSnapshot> {
        self.snapshot.connection()
    }

    pub(super) fn snapshot_connection_ref<'a>(
        &self,
        connection: &'a SqliteSourceReadSnapshot,
    ) -> Result<&'a rusqlite::Connection> {
        self.snapshot.connection_ref(connection)
    }

    pub(super) fn finish_snapshot_connection(
        &self,
        connection: SqliteSourceReadSnapshot,
    ) -> Result<bool> {
        self.snapshot.finish_connection(connection)
    }
}

pub(super) struct GooseNativeScanner<'connection> {
    conn: Option<SqliteSourceReadSnapshot>,
    schema: &'connection GooseNativeSchema,
    snapshot: &'connection GooseSnapshotGeneration,
    limits: GooseNativePageLimits,
    authority: GooseNativeSourceAuthority,
    raw_generation_digest: String,
    position: GooseNativeScanPosition,
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
    ) -> Result<Self> {
        let conn = snapshot.connection()?;
        let preparation =
            goose_prepare_native_identity_index(snapshot.connection_ref(&conn)?, schema, limits);
        let finished = snapshot.finish_connection(conn)?;
        if !finished {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        preparation?;
        let mut semantic_hasher = Sha256::new();
        semantic_hasher.update(GOOSE_SEMANTIC_DIGEST_DOMAIN);
        let mut session_inventory_hasher = Sha256::new();
        session_inventory_hasher.update(GOOSE_SESSION_INVENTORY_DIGEST_DOMAIN);
        Ok(Self {
            conn: None,
            schema,
            snapshot,
            limits,
            authority,
            raw_generation_digest: snapshot.observation().generation_digest(),
            position: GooseNativeScanPosition::initial(),
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

    fn connection(&self) -> Result<&rusqlite::Connection> {
        let connection = self.conn.as_ref().ok_or(CaptureError::SystemInvariant(
            "Goose scanner queried its SQLite snapshot after finish",
        ))?;
        self.snapshot.connection_ref(connection)
    }

    pub(super) fn next_page(&mut self) -> Result<Option<GooseNativePage>> {
        self.with_query_snapshot(Self::next_page_inner)
    }

    fn next_page_inner(&mut self) -> Result<Option<GooseNativePage>> {
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
                        self.connection()?,
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
                            let (kind, reason) = if scanned.storage_class_supported {
                                (
                                    GooseNativeRejectionKind::OversizedSession,
                                    format!(
                                        "Goose session row {} has {} bytes and exceeds the bounded Core page",
                                        scanned.sqlite_rowid, scanned.observed_bytes
                                    ),
                                )
                            } else {
                                (
                                    GooseNativeRejectionKind::UnsupportedStorageClass,
                                    format!(
                                        "Goose session row {} has unsupported SQLite storage classes",
                                        scanned.sqlite_rowid
                                    ),
                                )
                            };
                            let rejection = GooseNativeRejection {
                                sqlite_rowid: scanned.sqlite_rowid,
                                native_order: None,
                                native_identity: bounded_identity.clone(),
                                session_identity: None,
                                kind,
                                reason,
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
                    if !goose_has_native_session_after(self.connection()?, last_rowid)? {
                        self.position = self.position.start_messages();
                        if !goose_has_any_native_message(self.connection()?)? {
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
                        self.connection()?,
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
                    if !goose_has_native_message_after(self.connection()?, last_rowid)? {
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
        if self.conn.is_some() {
            return Err(CaptureError::SystemInvariant(
                "Goose Core scanner retained a SQLite guard between pages",
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

    fn with_query_snapshot<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        if self.conn.is_some() {
            return Err(CaptureError::SystemInvariant(
                "Goose scanner opened overlapping SQLite guards",
            ));
        }
        self.conn = Some(self.snapshot.connection()?);
        let result = operation(self);
        let connection = self.conn.take().ok_or(CaptureError::SystemInvariant(
            "Goose scanner lost its SQLite guard before page certification",
        ))?;
        let finished = self.snapshot.finish_connection(connection);
        match (result, finished) {
            (_, Ok(false)) => Err(CaptureError::SourceChangedDuringCapture),
            (Ok(value), Ok(true)) => Ok(value),
            (Err(error), Ok(true)) => Err(error),
            (_, Err(error)) => Err(error),
        }
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
            position: self.position,
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
}
