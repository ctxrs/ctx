use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    EventHydrationRequest, EventIdentityInput, EventRole, EventType, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, PositionStability,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceFrontier, SourceKey,
    SourceObservation, SourceRecordLocator, StableEntityId, SubrecordSelector, TypedKey,
};
use ctx_history_index::{LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    complete_content::{
        jsonl::{JsonlCompleteContentResolver, EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND},
        verified_content_address_supported, verified_content_profile, AuthorizedSourceRoute,
        CompleteContentBodyDigest, CompleteContentError, CompleteContentErrorKind,
        CompleteContentHashAuthority, CompleteContentResolver, CompleteContentSourceFamily,
        CompleteMessageRequest, SourceAccessBroker, SourceSnapshot, VerifiedContentLocatorV1,
        VerifiedContentRole,
    },
    CaptureError, Result, CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    cursor_complete_content_source_revision, discover_cursor_transcripts, freeze_cursor_source,
    projection::{
        CursorEventBody, CursorNativeEvent, CursorNativeOrder, CursorPublicationPage,
        CursorPublicationSink, CURSOR_PUBLICATION_PAGE_MAX_BYTES, CURSOR_PUBLICATION_PAGE_MAX_ROWS,
    },
    source::{CursorSourceGeneration, CursorSourceMutation},
    CursorNativeSession, CursorReadOutcome, CursorSourceObservation, CursorTranscriptPath,
};

const CURSOR_SOURCE_ANCHOR_NAMESPACE: &str = "cursor.session";
const CURSOR_NATIVE_SESSION_NAMESPACE: &str = "cursor.session";
const CURSOR_NATIVE_EVENT_POSITION_KIND: &str = "cursor.semantic-ordinal";
const CURSOR_NATIVE_SUBRECORD_POSITION_KIND: &str = "cursor.part-ordinal";
const CURSOR_LOGICAL_SESSION_KIND: &str = "cursor-session";
const CURSOR_LOGICAL_EVENT_KIND: &str = "cursor-event";
const CURSOR_SOURCE_SCHEMA_VARIANT: &str = "cursor-agent-transcript-jsonl-v1";
const CURSOR_SOURCE_REVISION_KIND: &str = "cursor-exact-jsonl-source-v1";
const CURSOR_FRONTIER_KIND: &str = "cursor-nativepath-checkpoint-v1";
const CURSOR_PARSER_REVISION: &str = "cursor-nativepath-source-backed-v1";
const CURSOR_SOURCE_REVISION_DIGEST_DOMAIN: &[u8] =
    b"ctx.cursor.source-backed.source-revision.v1\0";
const CURSOR_EXACT_SOURCE_REVISION_DIGEST_DOMAIN: &[u8] =
    b"ctx-complete-content-source-revision-v1\0";
const CURSOR_EXACT_PATH_IDENTITY_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-path-identity-v1\0";
const CURSOR_SOURCE_BACKED_PAGE_ENVELOPE_BYTES: usize = 1_024;
const CURSOR_SOURCE_BACKED_RECORD_ENVELOPE_BYTES: usize = 8_192;
const CURSOR_EVENT_SEQUENCE_PARTS: u64 = u16::MAX as u64 + 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorSourceBackedRecord {
    pub(crate) event_id: StableEntityId,
    pub(crate) session_id: StableEntityId,
    pub(crate) locator: SourceRecordLocator,
    pub(crate) native_order: CursorNativeOrder,
    pub(crate) event_sequence: u64,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
    pub(crate) event_type: EventType,
    pub(crate) role: EventRole,
    pub(crate) lexical_preview: Option<String>,
    pub(crate) touched_files: Vec<String>,
    pub(crate) provider_event_hash: String,
    pub(crate) provider_session_id: String,
    pub(crate) source_path: String,
    pub(crate) verified_content_locator: Option<VerifiedContentLocatorV1>,
    pub(crate) verified_content_indexed_text: Option<String>,
}

impl CursorSourceBackedRecord {
    pub(crate) fn lexical_document(&self) -> Option<LexicalDocument> {
        Some(LexicalDocument {
            event_id: self.event_id,
            session_id: self.session_id,
            parent_session_id: None,
            root_session_id: self.session_id,
            source: self.locator.source().clone(),
            locator: self.locator.clone(),
            provider_session_id: Some(self.provider_session_id.clone()),
            branch: None,
            source_path: Some(self.source_path.clone()),
            agent_type: AgentType::Primary.as_str().to_owned(),
            is_primary: true,
            event_sequence: self.event_sequence,
            occurred_at_unix_ms: self.occurred_at.map(|value| value.timestamp_millis()),
            event_type: self.event_type.as_str().to_owned(),
            role: Some(self.role.as_str().to_owned()),
            body: self.lexical_preview.clone()?,
            workspace: None,
            cwd: None,
            touched_files: self.touched_files.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorSourceBackedPage {
    pub(crate) source_id: StableEntityId,
    pub(crate) page_ordinal: u64,
    pub(crate) records: Vec<CursorSourceBackedRecord>,
    pub(crate) estimated_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorSourceBackedSourcePlan {
    pub(crate) projects_root: PathBuf,
    pub(crate) source_path: PathBuf,
    pub(crate) project: PathBuf,
    pub(crate) native_session_id: String,
    pub(crate) source: SourceKey,
    pub(crate) session_id: StableEntityId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorSourceBackedTerminal {
    pub(crate) plan: CursorSourceBackedSourcePlan,
    pub(crate) session: Option<CursorNativeSession>,
    pub(crate) certified_source: CertifiedSource,
    pub(crate) terminal: bool,
    pub(crate) physical_records: u64,
    pub(crate) projected_records: u64,
    pub(crate) indexed_documents: u64,
    pub(crate) rejected_records: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CursorSourceBackedSummary {
    pub(crate) projects_root: PathBuf,
    pub(crate) discovered_sources: u64,
    pub(crate) projected_records: u64,
    pub(crate) indexed_documents: u64,
    pub(crate) rejected_records: u64,
    pub(crate) certified_bytes: u64,
}

/// Provider-local acquisition sink. Implementations stage these pages into the
/// shared generation transaction; this adapter deliberately owns no lifecycle
/// state machine or publication commit.
pub(crate) trait CursorSourceBackedSink {
    fn begin_cursor_source(&mut self, plan: &CursorSourceBackedSourcePlan) -> Result<()>;

    fn stage_cursor_source_page(&mut self, page: CursorSourceBackedPage) -> Result<()>;

    fn finish_cursor_source(&mut self, terminal: CursorSourceBackedTerminal) -> Result<()>;

    fn abort_cursor_source(&mut self);
}

pub(crate) fn extract_cursor_source_backed_cold(
    selected_root: &Path,
    sink: &mut dyn CursorSourceBackedSink,
) -> Result<CursorSourceBackedSummary> {
    let projects_root = cursor_projects_root(selected_root);
    let inventory = discover_cursor_transcripts(&projects_root);
    if !inventory.completed {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: selected_root.to_path_buf(),
            reason: "Cursor source-backed transcript inventory could not be completed",
        });
    }
    if inventory
        .transcripts
        .iter()
        .any(|transcript| transcript.projects_root() != projects_root)
    {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed discovery escaped the selected projects root".to_owned(),
        ));
    }

    let mut native_session_ids = BTreeSet::new();
    let mut plans = Vec::with_capacity(inventory.transcripts.len());
    for transcript in inventory.transcripts {
        let native_session_id = transcript.native_session_id().to_owned();
        if !native_session_ids.insert(native_session_id.clone()) {
            return Err(CaptureError::InvalidPayload(format!(
                "Cursor native session ID {native_session_id:?} resolves to more than one transcript in the selected projects root"
            )));
        }
        plans.push(source_plan(&projects_root, transcript)?);
    }

    let mut summary = CursorSourceBackedSummary {
        projects_root: projects_root.clone(),
        discovered_sources: u64::try_from(plans.len()).map_err(|_| {
            CaptureError::InvalidPayload(
                "Cursor source-backed source count is not representable".to_owned(),
            )
        })?,
        ..CursorSourceBackedSummary::default()
    };
    for (transcript, mut plan) in plans {
        let frozen = freeze_cursor_source(&transcript)?;
        plan.source_path = frozen.observation().path.clone();
        let revision_digest = source_revision_digest(frozen.observation())?;
        let mut bridge =
            CursorProjectionBridge::new(sink, &plan, frozen.observation(), revision_digest);
        let outcome = super::scan_cursor_source_into(&frozen, None, &mut bridge);
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => return Err(error),
        };
        let bridge_counts = bridge.counts();
        drop(bridge);
        let CursorReadOutcome::Generation(generation) = outcome else {
            sink.abort_cursor_source();
            return Err(CaptureError::SystemInvariant(
                "cold Cursor source-backed extraction returned an unchanged source",
            ));
        };
        if generation.mutation != CursorSourceMutation::NewPathScopedSource {
            sink.abort_cursor_source();
            return Err(CaptureError::SystemInvariant(
                "cold Cursor source-backed extraction classified a prior mutation",
            ));
        }
        let terminal = match source_terminal(plan, *generation, bridge_counts) {
            Ok(terminal) => terminal,
            Err(error) => {
                sink.abort_cursor_source();
                return Err(error);
            }
        };
        summary.projected_records = checked_add(
            summary.projected_records,
            terminal.projected_records,
            "Cursor projected-record count overflowed",
        )?;
        summary.indexed_documents = checked_add(
            summary.indexed_documents,
            terminal.indexed_documents,
            "Cursor indexed-document count overflowed",
        )?;
        summary.rejected_records = checked_add(
            summary.rejected_records,
            terminal.rejected_records,
            "Cursor rejected-record count overflowed",
        )?;
        summary.certified_bytes = checked_add(
            summary.certified_bytes,
            terminal.certified_source.counts().certified_bytes,
            "Cursor certified-byte count overflowed",
        )?;
        if let Err(error) = sink.finish_cursor_source(terminal) {
            sink.abort_cursor_source();
            return Err(error);
        }
    }
    Ok(summary)
}

pub(crate) fn hydrate_cursor_source_backed_message(
    selected_root: &Path,
    record: &CursorSourceBackedRecord,
) -> Result<String> {
    if record.event_type != EventType::Message {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed message hydration received a non-message event".to_owned(),
        ));
    }
    record.lexical_preview.as_deref().ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Cursor source-backed message hydration is missing its lexical preview".to_owned(),
        )
    })?;
    let verified_locator = record.verified_content_locator.as_ref().ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Cursor source-backed message has no verified complete-content locator".to_owned(),
        )
    })?;
    let indexed_text = record
        .verified_content_indexed_text
        .as_ref()
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Cursor source-backed message has no verified indexed-text witness".to_owned(),
            )
        })?;
    EventHydrationRequest::new(record.event_id, record.locator.clone())
        .map_err(|error| contract_error("event hydration request", error))?;
    let (native_session_id, byte_offset, byte_length, physical_ordinal, part_ordinal) =
        validate_locator(record)?;
    let projects_root = cursor_projects_root(selected_root);
    let transcript = unique_transcript(&projects_root, &native_session_id)?;
    let expected_source = cursor_source_key(&native_session_id)?;
    if !expected_source.exact_descriptor_eq(record.locator.source()) {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed locator source descriptor no longer matches discovery".to_owned(),
        ));
    }
    let frozen = freeze_cursor_source(&transcript)?;
    let current_revision_digest = source_revision_digest(frozen.observation())?;
    if record.locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
        || record.locator.certified_source_revision_digest() != Some(&current_revision_digest)
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let current_source_path = frozen.observation().path.to_str().ok_or_else(|| {
        CaptureError::InvalidProviderTranscriptPath {
            path: frozen.observation().path.clone(),
            reason: "Cursor source-backed transcript path is not UTF-8",
        }
    })?;
    if current_source_path != record.source_path
        || record.provider_session_id != native_session_id
        || verified_locator.record_sha256().as_str() != hex_digest(record.locator.record_digest())
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let expected_native_record_id = format!("cursor-line-v1:{physical_ordinal}:{part_ordinal}");
    let source_locator = verified_locator.source_locator().ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Cursor source-backed verified-content locator is malformed".to_owned(),
        )
    })?;
    let expected_byte_end = byte_offset.checked_add(byte_length).ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Cursor source-backed JSONL locator range overflowed".to_owned(),
        )
    })?;
    if verified_locator.role() != VerifiedContentRole::MessageBody
        || verified_locator.family() != CompleteContentSourceFamily::Jsonl
        || verified_locator.kind() != EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND
        || verified_locator.native_record_id() != expected_native_record_id
        || source_locator.value().len() != 80
        || source_locator.value()[..8] != byte_offset.to_be_bytes()
        || source_locator.value()[8..16] != expected_byte_end.to_be_bytes()
    {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed verified-content locator does not match its event locator"
                .to_owned(),
        ));
    }
    let event_id = record.event_id.as_uuid();
    let route = AuthorizedSourceRoute {
        source_id: record.locator.source().identity().as_uuid(),
        provider: CaptureProvider::Cursor,
        source_format: CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT.to_owned(),
        family: CompleteContentSourceFamily::Jsonl,
        raw_source_path: frozen.observation().path.clone(),
        source_root: Some(projects_root),
        source_identity: Some(frozen.observation().proposed_source_identity.clone()),
        source_snapshot: SourceSnapshot::default(),
    };
    let source_access = SourceAccessBroker::new()
        .admit_for_source_locators(route, std::slice::from_ref(&source_locator), event_id)
        .map_err(complete_content_error)?;
    let request = CompleteMessageRequest {
        event_id,
        provider: CaptureProvider::Cursor,
        source_format: CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT.to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        content_profile: verified_locator.content_profile().to_owned(),
        source_locator: Some(source_locator),
        provider_session_id: Some(record.provider_session_id.clone()),
        source_record_ordinal: physical_ordinal,
        source_record_subrecord_index: part_ordinal,
        expected_provider_event_hash: record.provider_event_hash.clone(),
        expected_hash_authority: CompleteContentHashAuthority::NormalizedPayloadFallback,
        expected_native_record_id: Some(expected_native_record_id),
        expected_record_digest: Some(verified_locator.record_sha256().clone()),
        expected_content_ref: Some(verified_locator.content_ref().clone()),
        indexed_text: indexed_text.clone(),
        indexed_limit_chars: PROVIDER_MAX_TEXT_CHARS,
    };
    let resolved = JsonlCompleteContentResolver::new()
        .resolve(&[request])
        .map_err(complete_content_error)?;
    let message = resolved
        .into_iter()
        .next()
        .ok_or(CaptureError::SystemInvariant(
            "Cursor exact complete-content route returned no message",
        ))?;
    Ok(message.text)
}

fn cursor_projects_root(selected_root: &Path) -> PathBuf {
    let nested = selected_root.join("projects");
    if selected_root.is_dir() && nested.is_dir() {
        nested
    } else {
        selected_root.to_path_buf()
    }
}

fn source_plan(
    projects_root: &Path,
    transcript: CursorTranscriptPath,
) -> Result<(CursorTranscriptPath, CursorSourceBackedSourcePlan)> {
    let native_session_id = transcript.native_session_id().to_owned();
    let source = cursor_source_key(&native_session_id)?;
    let native_session_key = NativeSessionKey::native_id(
        CURSOR_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id.clone())
            .map_err(|error| contract_error("native session key", error))?,
    )
    .map_err(|error| contract_error("native session key", error))?;
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: CURSOR_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })
    .map_err(|error| contract_error("session identity", error))?;
    let plan = CursorSourceBackedSourcePlan {
        projects_root: projects_root.to_path_buf(),
        source_path: transcript.path().to_path_buf(),
        project: transcript.project().to_path_buf(),
        native_session_id,
        source,
        session_id,
    };
    Ok((transcript, plan))
}

fn cursor_source_key(native_session_id: &str) -> Result<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        CURSOR_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id.to_owned())
            .map_err(|error| contract_error("source anchor", error))?,
    )
    .map_err(|error| contract_error("source anchor", error))?;
    SourceKey::derive(
        CaptureProvider::Cursor.as_str(),
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        CURSOR_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )
    .map_err(|error| contract_error("source identity", error))
}

#[derive(Serialize)]
struct CursorExactSourceRevision<'a> {
    version: u32,
    complete_content_revision: String,
    locator_identity: &'a str,
    content_sha256: [u8; 32],
}

fn source_revision_bytes(observation: &CursorSourceObservation) -> Result<Vec<u8>> {
    serde_json::to_vec(&CursorExactSourceRevision {
        version: 1,
        complete_content_revision: cursor_complete_content_source_revision(observation),
        locator_identity: &observation.locator_identity,
        content_sha256: observation.content_sha256,
    })
    .map_err(Into::into)
}

fn source_revision_digest(observation: &CursorSourceObservation) -> Result<[u8; 32]> {
    let revision = source_revision_bytes(observation)?;
    let mut digest = Sha256::new();
    digest.update(CURSOR_SOURCE_REVISION_DIGEST_DOMAIN);
    digest.update((revision.len() as u64).to_be_bytes());
    digest.update(revision);
    Ok(digest.finalize().into())
}

fn source_observation(
    source: &SourceKey,
    observation: &CursorSourceObservation,
) -> Result<SourceObservation> {
    SourceObservation::new(
        source.clone(),
        CURSOR_SOURCE_REVISION_KIND,
        source_revision_bytes(observation)?,
    )
    .map_err(|error| contract_error("source observation", error))
}

#[derive(Debug, Clone, Copy, Default)]
struct BridgeCounts {
    projected_records: u64,
    projected_native_records: u64,
    indexed_documents: u64,
}

struct CursorProjectionBridge<'a> {
    sink: &'a mut dyn CursorSourceBackedSink,
    plan: &'a CursorSourceBackedSourcePlan,
    observation: &'a CursorSourceObservation,
    revision_digest: [u8; 32],
    records: Vec<CursorSourceBackedRecord>,
    estimated_bytes: usize,
    page_ordinal: u64,
    counts: BridgeCounts,
}

impl<'a> CursorProjectionBridge<'a> {
    fn new(
        sink: &'a mut dyn CursorSourceBackedSink,
        plan: &'a CursorSourceBackedSourcePlan,
        observation: &'a CursorSourceObservation,
        revision_digest: [u8; 32],
    ) -> Self {
        Self {
            sink,
            plan,
            observation,
            revision_digest,
            records: Vec::new(),
            estimated_bytes: CURSOR_SOURCE_BACKED_PAGE_ENVELOPE_BYTES,
            page_ordinal: 0,
            counts: BridgeCounts::default(),
        }
    }

    fn counts(&self) -> BridgeCounts {
        self.counts
    }

    fn push(&mut self, event: CursorNativeEvent) -> Result<()> {
        let first_native_part = event.native_order.part_ordinal == 0;
        let record =
            source_backed_record(self.plan, self.observation, self.revision_digest, event)?;
        let record_bytes = source_backed_record_upper_bound(&record);
        let separator_bytes = usize::from(!self.records.is_empty());
        if !self.records.is_empty()
            && (self.records.len() >= CURSOR_PUBLICATION_PAGE_MAX_ROWS
                || self
                    .estimated_bytes
                    .saturating_add(separator_bytes)
                    .saturating_add(record_bytes)
                    > CURSOR_PUBLICATION_PAGE_MAX_BYTES)
        {
            self.flush()?;
        }
        if self.records.is_empty()
            && self.estimated_bytes.saturating_add(record_bytes) > CURSOR_PUBLICATION_PAGE_MAX_BYTES
        {
            return Err(CaptureError::SystemInvariant(
                "Cursor source-backed record exceeds the bounded projection page",
            ));
        }
        self.estimated_bytes = self
            .estimated_bytes
            .saturating_add(usize::from(!self.records.is_empty()))
            .saturating_add(record_bytes);
        self.counts.projected_records = checked_add(
            self.counts.projected_records,
            1,
            "Cursor source-backed projected-record count overflowed",
        )?;
        if first_native_part {
            self.counts.projected_native_records = checked_add(
                self.counts.projected_native_records,
                1,
                "Cursor source-backed native-record count overflowed",
            )?;
        }
        if record.lexical_preview.is_some() {
            self.counts.indexed_documents = checked_add(
                self.counts.indexed_documents,
                1,
                "Cursor source-backed indexed-document count overflowed",
            )?;
        }
        self.records.push(record);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.records.is_empty() {
            return Ok(());
        }
        let page = CursorSourceBackedPage {
            source_id: self.plan.source.identity(),
            page_ordinal: self.page_ordinal,
            records: std::mem::take(&mut self.records),
            estimated_bytes: std::mem::replace(
                &mut self.estimated_bytes,
                CURSOR_SOURCE_BACKED_PAGE_ENVELOPE_BYTES,
            ),
        };
        self.sink.stage_cursor_source_page(page)?;
        self.page_ordinal = checked_add(
            self.page_ordinal,
            1,
            "Cursor source-backed page count overflowed",
        )?;
        Ok(())
    }
}

impl CursorPublicationSink for CursorProjectionBridge<'_> {
    fn begin_cursor_publication(&mut self) -> Result<()> {
        self.sink.begin_cursor_source(self.plan)
    }

    fn stage_cursor_page(&mut self, page: CursorPublicationPage) -> Result<()> {
        for event in page.events {
            self.push(event)?;
        }
        Ok(())
    }

    fn abort_cursor_publication(&mut self) {
        self.records.clear();
        self.sink.abort_cursor_source();
    }

    fn commit_cursor_publication(&mut self) -> Result<()> {
        self.flush()
    }
}

fn source_backed_record(
    plan: &CursorSourceBackedSourcePlan,
    observation: &CursorSourceObservation,
    revision_digest: [u8; 32],
    event: CursorNativeEvent,
) -> Result<CursorSourceBackedRecord> {
    let verified_content = cursor_verified_content_locator(observation, &event)?;
    let CursorNativeEvent {
        native_order,
        event_type,
        role,
        occurred_at,
        body,
        record_byte_start,
        record_byte_end_exclusive,
        record_sha256,
        provider_event_hash,
        ..
    } = event;
    let native_item_key = NativeItemKey::certified_position(
        CURSOR_NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(native_order.semantic_ordinal),
        PositionStability::AppendStable,
    )
    .map_err(|error| contract_error("native event position", error))?;
    let subrecord = SubrecordSelector::certified_position(
        CURSOR_NATIVE_SUBRECORD_POSITION_KIND,
        TypedKey::U64(u64::from(native_order.part_ordinal)),
        PositionStability::StableSlot,
    )
    .map_err(|error| contract_error("native subrecord position", error))?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &plan.source,
        session_id: plan.session_id,
        logical_item_kind: CURSOR_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: Some(&subrecord),
    })
    .map_err(|error| contract_error("event identity", error))?;
    let byte_length = record_byte_end_exclusive
        .checked_sub(record_byte_start)
        .filter(|length| *length > 0)
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Cursor source-backed event has an invalid JSONL byte range".to_owned(),
            )
        })?;
    let native_event_key = TypedKey::composite(vec![
        TypedKey::U64(native_order.semantic_ordinal),
        TypedKey::U64(u64::from(native_order.part_ordinal)),
    ])
    .map_err(|error| contract_error("native locator event key", error))?;
    if native_order.part_ordinal > u32::from(u16::MAX) {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed record exceeds the stable event-sequence part bound".to_owned(),
        ));
    }
    let locator = SourceRecordLocator::new(
        plan.source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: record_byte_start,
            byte_length,
            physical_ordinal: native_order.physical_ordinal,
            native_session_key: Some(
                TypedKey::utf8(plan.native_session_id.clone())
                    .map_err(|error| contract_error("native locator session key", error))?,
            ),
            native_event_key: Some(native_event_key),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(revision_digest),
        record_sha256,
    )
    .map_err(|error| contract_error("exact JSONL locator", error))?;
    let event_sequence = native_order
        .semantic_ordinal
        .checked_mul(CURSOR_EVENT_SEQUENCE_PARTS)
        .and_then(|base| base.checked_add(u64::from(native_order.part_ordinal)))
        .ok_or(CaptureError::SystemInvariant(
            "Cursor source-backed event sequence overflowed",
        ))?;
    let (lexical_preview, touched_files) = lexical_projection(body);
    let source_path = plan
        .source_path
        .to_str()
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: plan.source_path.clone(),
            reason: "Cursor source-backed transcript path is not UTF-8",
        })?
        .to_owned();
    let (verified_content_locator, verified_content_indexed_text) = verified_content
        .map(|(locator, indexed_text)| (Some(locator), Some(indexed_text)))
        .unwrap_or((None, None));
    Ok(CursorSourceBackedRecord {
        event_id,
        session_id: plan.session_id,
        locator,
        native_order,
        event_sequence,
        occurred_at,
        event_type,
        role,
        lexical_preview,
        touched_files,
        provider_event_hash,
        provider_session_id: plan.native_session_id.clone(),
        source_path,
        verified_content_locator,
        verified_content_indexed_text,
    })
}

fn cursor_verified_content_locator(
    observation: &CursorSourceObservation,
    event: &CursorNativeEvent,
) -> Result<Option<(VerifiedContentLocatorV1, String)>> {
    let Some(content_ref) = event.complete_content_ref.clone() else {
        return Ok(None);
    };
    let CursorEventBody::Text { text: indexed_text } = &event.body else {
        return Err(CaptureError::SystemInvariant(
            "Cursor complete-message reference is attached to a non-text event",
        ));
    };
    if event.event_type != EventType::Message {
        return Err(CaptureError::SystemInvariant(
            "Cursor complete-message reference is attached to a non-message event",
        ));
    }
    if !verified_content_address_supported(
        CaptureProvider::Cursor,
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
        EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
    ) {
        return Err(CaptureError::SystemInvariant(
            "Cursor complete messages require the verified exact JSONL route",
        ));
    }
    let profile = verified_content_profile(
        CaptureProvider::Cursor,
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Cursor exact JSONL route has no verified-content profile",
    ))?;
    let mut range = [0_u8; 80];
    range[..8].copy_from_slice(&event.record_byte_start.to_be_bytes());
    range[8..16].copy_from_slice(&event.record_byte_end_exclusive.to_be_bytes());
    range[16..48].copy_from_slice(&complete_content_digest(
        CURSOR_EXACT_SOURCE_REVISION_DIGEST_DOMAIN,
        &cursor_complete_content_source_revision(observation),
    ));
    range[48..].copy_from_slice(&complete_content_digest(
        CURSOR_EXACT_PATH_IDENTITY_DIGEST_DOMAIN,
        &observation.locator_identity,
    ));
    let native_record_id = format!(
        "cursor-line-v1:{}:{}",
        event.native_order.physical_ordinal, event.native_order.part_ordinal
    );
    let record_digest = CompleteContentBodyDigest::parse(hex_digest(&event.record_sha256)).ok_or(
        CaptureError::SystemInvariant("Cursor source-backed record digest is malformed"),
    )?;
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &range,
        native_record_id,
        record_digest,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Cursor source-backed exact JSONL locator exceeds its bounded schema",
    ))?;
    Ok(Some((locator, indexed_text.clone())))
}

fn lexical_projection(body: CursorEventBody) -> (Option<String>, Vec<String>) {
    match body {
        CursorEventBody::None => (None, Vec::new()),
        CursorEventBody::Text { text } => (bounded_preview(&text), Vec::new()),
        CursorEventBody::ToolCall {
            call_id,
            tool_name,
            input_paths,
        } => {
            let mut preview = String::new();
            if let Some(tool_name) = tool_name.as_deref() {
                append_preview_component(&mut preview, tool_name);
            }
            if let Some(call_id) = call_id.as_deref() {
                append_preview_component(&mut preview, call_id);
            }
            for path in &input_paths {
                append_preview_component(&mut preview, path);
            }
            ((!preview.is_empty()).then_some(preview), input_paths)
        }
    }
}

fn bounded_preview(value: &str) -> Option<String> {
    let preview = value
        .chars()
        .take(MAX_BODY_PREVIEW_CHARS)
        .collect::<String>();
    (!preview.is_empty()).then_some(preview)
}

fn append_preview_component(preview: &mut String, value: &str) {
    if value.is_empty() || preview.chars().count() >= MAX_BODY_PREVIEW_CHARS {
        return;
    }
    if !preview.is_empty() {
        preview.push(' ');
    }
    let remaining = MAX_BODY_PREVIEW_CHARS.saturating_sub(preview.chars().count());
    preview.extend(value.chars().take(remaining));
}

fn source_backed_record_upper_bound(record: &CursorSourceBackedRecord) -> usize {
    let string_bytes = record
        .provider_event_hash
        .len()
        .saturating_add(record.provider_session_id.len())
        .saturating_add(record.source_path.len())
        .saturating_add(record.lexical_preview.as_deref().map_or(0, str::len))
        .saturating_add(
            record
                .verified_content_indexed_text
                .as_deref()
                .map_or(0, str::len),
        )
        .saturating_add(record.touched_files.iter().map(String::len).sum::<usize>());
    CURSOR_SOURCE_BACKED_RECORD_ENVELOPE_BYTES.saturating_add(string_bytes.saturating_mul(6))
}

fn source_terminal(
    plan: CursorSourceBackedSourcePlan,
    generation: CursorSourceGeneration,
    bridge: BridgeCounts,
) -> Result<CursorSourceBackedTerminal> {
    if bridge.projected_records != generation.stats.nativepath_publication_rows {
        return Err(CaptureError::SystemInvariant(
            "Cursor source-backed page rows do not reconcile with the parser",
        ));
    }
    let rejected_records = generation.rejections.total;
    let ignored_records = generation
        .stats
        .complete_records
        .checked_sub(rejected_records)
        .and_then(|value| value.checked_sub(bridge.projected_native_records))
        .ok_or(CaptureError::SystemInvariant(
            "Cursor source-backed physical record counts do not reconcile",
        ))?;
    let complete_records = bridge
        .projected_records
        .checked_add(rejected_records)
        .and_then(|value| value.checked_add(ignored_records))
        .ok_or(CaptureError::SystemInvariant(
            "Cursor source-backed certified record count overflowed",
        ))?;
    let counts = ScannedSourceCounts {
        complete_records,
        retained_records: bridge.projected_records,
        rejected_records,
        ignored_records,
        indexed_documents: bridge.indexed_documents,
        certified_bytes: generation.checkpoint.next_byte_offset,
    };
    let observation = source_observation(&plan.source, &generation.observation)?;
    let checkpoint_bytes = serde_json::to_vec(&generation.checkpoint)?;
    let frontier = SourceFrontier::new(
        CURSOR_FRONTIER_KIND,
        TypedKey::bytes(checkpoint_bytes)
            .map_err(|error| contract_error("source checkpoint", error))?,
        generation.checkpoint.next_byte_offset,
        generation.checkpoint.prefix.content_sha256,
    )
    .map_err(|error| contract_error("source frontier", error))?;
    let certified_source = CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        CURSOR_PARSER_REVISION,
        generation.checkpoint.prefix.content_sha256,
        counts,
        Some(frontier),
    )
    .map_err(|error| contract_error("source certification", error))?;
    Ok(CursorSourceBackedTerminal {
        plan,
        session: generation.session,
        certified_source,
        terminal: generation.checkpoint.terminal,
        physical_records: generation.stats.complete_records,
        projected_records: bridge.projected_records,
        indexed_documents: bridge.indexed_documents,
        rejected_records,
    })
}

fn unique_transcript(
    projects_root: &Path,
    native_session_id: &str,
) -> Result<CursorTranscriptPath> {
    let inventory = discover_cursor_transcripts(projects_root);
    if !inventory.completed {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: projects_root.to_path_buf(),
            reason: "Cursor source-backed hydration inventory could not be completed",
        });
    }
    let mut matches = inventory
        .transcripts
        .into_iter()
        .filter(|transcript| transcript.native_session_id() == native_session_id);
    let transcript = matches.next().ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "Cursor source-backed locator session {native_session_id:?} is absent from the selected projects root"
        ))
    })?;
    if matches.next().is_some() {
        return Err(CaptureError::InvalidPayload(format!(
            "Cursor source-backed locator session {native_session_id:?} is ambiguous in the selected projects root"
        )));
    }
    Ok(transcript)
}

fn validate_locator(record: &CursorSourceBackedRecord) -> Result<(String, u64, u64, u64, u32)> {
    let source = record.locator.source();
    if source.provider() != CaptureProvider::Cursor.as_str()
        || source.source_format() != CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
        || source.schema_variant() != CURSOR_SOURCE_SCHEMA_VARIANT
        || source.provider_identity_version() != 1
    {
        return Err(CaptureError::InvalidPayload(
            "locator is not a Cursor source-backed JSONL record".to_owned(),
        ));
    }
    let SourceAnchor::ProviderNative { namespace, key } = source.anchor() else {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed locator has no native session anchor".to_owned(),
        ));
    };
    let TypedKey::Utf8(native_session_id) = key else {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed locator has a malformed native session anchor".to_owned(),
        ));
    };
    if namespace != CURSOR_SOURCE_ANCHOR_NAMESPACE {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed locator uses an unsupported source namespace".to_owned(),
        ));
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = record.locator.coordinate()
    else {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed locator is not a JSONL byte range".to_owned(),
        ));
    };
    let expected_event_key = TypedKey::composite(vec![
        TypedKey::U64(record.native_order.semantic_ordinal),
        TypedKey::U64(u64::from(record.native_order.part_ordinal)),
    ])
    .map_err(|error| contract_error("native locator event key", error))?;
    if native_session_key.as_ref() != Some(&TypedKey::Utf8(native_session_id.clone()))
        || native_event_key.as_ref() != Some(&expected_event_key)
        || *physical_ordinal != record.native_order.physical_ordinal
    {
        return Err(CaptureError::InvalidPayload(
            "Cursor source-backed locator coordinates do not match the projected event".to_owned(),
        ));
    }
    Ok((
        native_session_id.clone(),
        *byte_offset,
        *byte_length,
        *physical_ordinal,
        record.native_order.part_ordinal,
    ))
}

fn checked_add(left: u64, right: u64, message: &'static str) -> Result<u64> {
    left.checked_add(right)
        .ok_or(CaptureError::SystemInvariant(message))
}

fn complete_content_digest(domain: &[u8], value: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
    digest.finalize().into()
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn complete_content_error(error: CompleteContentError) -> CaptureError {
    match error.kind {
        CompleteContentErrorKind::SourceMissing
        | CompleteContentErrorKind::SourceUnreadable
        | CompleteContentErrorKind::SourceChanged
        | CompleteContentErrorKind::SourceRecordMissing
        | CompleteContentErrorKind::ContentVerificationFailed => {
            CaptureError::SourceChangedDuringCapture
        }
        CompleteContentErrorKind::HydrationUnsupported => CaptureError::InvalidPayload(
            "Cursor source-backed complete-message hydration is unsupported".to_owned(),
        ),
        CompleteContentErrorKind::ContentTooLarge => CaptureError::InvalidPayload(
            "Cursor source-backed complete message exceeds the hydration bound".to_owned(),
        ),
    }
}

fn contract_error(context: &'static str, error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(format!(
        "Cursor source-backed {context} is invalid: {error}"
    ))
}
