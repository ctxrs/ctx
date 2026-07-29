use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    EventIdentityInput, EventType, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, PositionStability, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceKey,
    SourceObservation as ProjectionSourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    complete_content, discover_inventory, normalization, open_pages_from_admitted,
    openclaw_output_metadata, Checkpoint, CoreEvent, PageReader, SourceChange,
    SourceObservation as LegacySourceObservation, OPENCLAW_SOURCE_FORMAT,
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider_sources::{provider_source_for_path, ProviderSourceStatus},
    CaptureError, OutputObservationKind, MAX_OPENCLAW_SESSION_INDEX_BYTES,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "openclaw.legacy-session";
const NATIVE_SESSION_NAMESPACE: &str = "openclaw.legacy-session";
const NATIVE_EVENT_NAMESPACE: &str = "openclaw.legacy-event";
const NATIVE_EVENT_POSITION_KIND: &str = "openclaw.legacy-jsonl.raw-ordinal";
const LOGICAL_SESSION_KIND: &str = "openclaw-legacy-session";
const LOGICAL_EVENT_KIND: &str = "openclaw-legacy-event";
const SOURCE_SCHEMA_VARIANT: &str = "openclaw-legacy-jsonl-v1";
const SOURCE_REVISION_KIND: &str = "openclaw-legacy-exact-observation-v0";
const FRONTIER_KIND: &str = "openclaw-nativepath-checkpoint-v1";
const PARSER_REVISION: &str = "openclaw-source-backed-v0";
const MAX_HYDRATED_RECORD_BYTES: u64 = MAX_PROVIDER_JSONL_LINE_BYTES as u64 + 2;

#[derive(Debug, Error)]
pub(crate) enum OpenClawSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("selected OpenClaw source {path:?} is unsupported: {reason}")]
    UnsupportedSelectedSource { path: PathBuf, reason: &'static str },
    #[error("OpenClaw source-backed frontier is missing or incompatible")]
    InvalidFrontier,
    #[error("OpenClaw source-backed reader has not reached a certified frontier")]
    ReaderNotFinished,
    #[error("OpenClaw source-backed scan counts do not reconcile")]
    CountMismatch,
    #[error("OpenClaw source-backed count overflow")]
    CountOverflow,
    #[error("locator is not an exact OpenClaw legacy JSONL record")]
    InvalidLocator,
    #[error("OpenClaw locator source revision no longer matches the selected source")]
    LocatorSourceRevisionMismatch,
    #[error("OpenClaw locator byte range is invalid or exceeds the record bound")]
    LocatorRangeInvalid,
    #[error("OpenClaw locator byte range is no longer present")]
    LocatorRangeMissing,
    #[error("OpenClaw locator record digest no longer matches provider bytes")]
    LocatorDigestMismatch,
    #[error("OpenClaw locator record identity no longer matches provider bytes")]
    LocatorRecordIdentityMismatch,
}

pub(crate) type OpenClawSourceBackedResultV0<T> = Result<T, OpenClawSourceBackedErrorV0>;

/// Provider-local registration hook for the central source-backed coordinator.
///
/// The hook deliberately accepts only an already selected root. OpenClaw's
/// product discovery policy remains outside this adapter, so legacy JSONL
/// cannot become an automatic source through this API.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct OpenClawSourceBackedAdapterV0;

pub(crate) const fn openclaw_source_backed_adapter_v0() -> OpenClawSourceBackedAdapterV0 {
    OpenClawSourceBackedAdapterV0
}

impl OpenClawSourceBackedAdapterV0 {
    pub(crate) fn discover_selected(
        self,
        selected_root: impl AsRef<Path>,
    ) -> OpenClawSourceBackedResultV0<Vec<OpenClawSourceBackedSourceV0>> {
        let selected_root = selected_root.as_ref();
        let selected =
            provider_source_for_path(CaptureProvider::OpenClaw, selected_root.to_path_buf());
        if selected.status == ProviderSourceStatus::Unsupported {
            return Err(OpenClawSourceBackedErrorV0::UnsupportedSelectedSource {
                path: selected_root.to_path_buf(),
                reason: selected
                    .unsupported_reason
                    .unwrap_or("unsupported OpenClaw history format"),
            });
        }

        let inventory = discover_inventory(selected_root)?;
        let canonical_selected = std::fs::canonicalize(selected_root)?;
        let authority_path = if std::fs::metadata(selected_root)?.is_file() {
            canonical_selected
                .parent()
                .ok_or(OpenClawSourceBackedErrorV0::UnsupportedSelectedSource {
                    path: canonical_selected.clone(),
                    reason: "selected transcript has no parent authority directory",
                })?
                .to_path_buf()
        } else {
            canonical_selected
        };
        let authority = ProviderSourceRoot::open(&authority_path)?;
        inventory
            .paths
            .into_iter()
            .map(|path| OpenClawSourceBackedSourceV0::from_canonical_path(authority.clone(), path))
            .collect()
    }

    pub(crate) fn open_source(
        self,
        source: &OpenClawSourceBackedSourceV0,
        imported_at: DateTime<Utc>,
        previous: Option<&CertifiedSource>,
    ) -> OpenClawSourceBackedResultV0<OpenClawSourceBackedReaderV0> {
        OpenClawSourceBackedReaderV0::open(source, imported_at, previous)
    }

    pub(crate) fn hydrate(
        self,
        source: &OpenClawSourceBackedSourceV0,
        locator: &SourceRecordLocator,
    ) -> OpenClawSourceBackedResultV0<OpenClawHydratedRecordV0> {
        hydrate_locator(source, locator)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OpenClawSourceBackedSourceV0 {
    authority: ProviderSourceRoot,
    path: PathBuf,
    transcript_relative_path: PathBuf,
    index_relative_path: PathBuf,
    source: SourceKey,
    native_session_id: String,
}

impl OpenClawSourceBackedSourceV0 {
    fn from_canonical_path(
        authority: ProviderSourceRoot,
        path: PathBuf,
    ) -> OpenClawSourceBackedResultV0<Self> {
        let transcript_relative_path = path
            .strip_prefix(authority.named_path())
            .map(Path::to_path_buf)
            .map_err(|_| OpenClawSourceBackedErrorV0::UnsupportedSelectedSource {
                path: path.clone(),
                reason: "selected transcripts must share one authority root",
            })?;
        let index_relative_path = transcript_relative_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("sessions.json");
        let native_session_id = native_session_id(&path);
        let source = source_key(&native_session_id)?;
        Ok(Self {
            authority,
            path,
            transcript_relative_path,
            index_relative_path,
            source,
            native_session_id,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn source_key(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn native_session_id(&self) -> &str {
        &self.native_session_id
    }

    fn admit(&self) -> OpenClawSourceBackedResultV0<AdmittedOpenClawSource> {
        let transcript = self.authority.open_file(&self.transcript_relative_path)?;
        let index = match self.authority.open_file(&self.index_relative_path) {
            Ok(index) => Some(index),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let index_bytes = index
            .as_ref()
            .map(|index| index.read_all_bounded(MAX_OPENCLAW_SESSION_INDEX_BYTES))
            .transpose()?;
        let observation = super::OpenClawSessionObservation::from_admitted(
            self.path.clone(),
            transcript.metadata(),
            index
                .as_ref()
                .zip(index_bytes.as_deref())
                .map(|(index, bytes)| (index.metadata(), bytes)),
        )?;
        Ok(AdmittedOpenClawSource {
            observation,
            transcript,
            index,
        })
    }
}

#[derive(Debug)]
struct AdmittedOpenClawSource {
    observation: super::OpenClawSessionObservation,
    transcript: OpenedProviderSourceFile,
    index: Option<OpenedProviderSourceFile>,
}

impl AdmittedOpenClawSource {
    fn revalidate(&self, authority: &ProviderSourceRoot) -> OpenClawSourceBackedResultV0<()> {
        self.transcript.revalidate()?;
        if let Some(index) = &self.index {
            index.revalidate()?;
        }
        authority.revalidate()?;
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct OpenClawSourceBackedPageV0 {
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) complete_records: u64,
    pub(crate) retained_records: u64,
    pub(crate) rejected_records: u64,
    pub(crate) certified_prefix_bytes: u64,
    pub(crate) terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenClawSourceBackedDispositionV0 {
    Cold,
    Noop,
    Append,
    Replacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpenClawSourceBackedVerifiedPrefixV0 {
    pub(crate) bytes: u64,
    pub(crate) digest: [u8; 32],
}

#[derive(Debug)]
pub(crate) struct OpenClawSourceBackedScanV0 {
    pub(crate) certified_source: CertifiedSource,
    pub(crate) disposition: OpenClawSourceBackedDispositionV0,
    pub(crate) verified_base_prefix: Option<OpenClawSourceBackedVerifiedPrefixV0>,
}

pub(crate) struct OpenClawSourceBackedReaderV0 {
    inner: PageReader,
    selected: OpenClawSourceBackedSourceV0,
    opening_index: Option<OpenedProviderSourceFile>,
    source: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
    opening: ProjectionSourceObservation,
    certified_revision_digest: [u8; 32],
    indexed_documents: u64,
    disposition: OpenClawSourceBackedDispositionV0,
    verified_base_prefix: Option<OpenClawSourceBackedVerifiedPrefixV0>,
}

impl OpenClawSourceBackedReaderV0 {
    fn open(
        source: &OpenClawSourceBackedSourceV0,
        imported_at: DateTime<Utc>,
        previous: Option<&CertifiedSource>,
    ) -> OpenClawSourceBackedResultV0<Self> {
        let admitted = source.admit()?;
        let opening_live = admitted.observation.clone();
        let previous_checkpoint = previous
            .map(|previous| decode_previous(previous, &source.source))
            .transpose()?;
        let inner = open_pages_from_admitted(
            source.path.clone(),
            imported_at,
            false,
            None,
            false,
            previous_checkpoint.as_ref(),
            admitted.observation,
            admitted.transcript,
        )?;
        let disposition = disposition(inner.source_change);
        let indexed_documents = match disposition {
            OpenClawSourceBackedDispositionV0::Append | OpenClawSourceBackedDispositionV0::Noop => {
                previous
                    .map(|previous| previous.counts().indexed_documents)
                    .unwrap_or_default()
            }
            OpenClawSourceBackedDispositionV0::Cold
            | OpenClawSourceBackedDispositionV0::Replacement => 0,
        };
        let verified_base_prefix = if disposition == OpenClawSourceBackedDispositionV0::Append {
            previous
                .and_then(CertifiedSource::frontier)
                .map(|frontier| OpenClawSourceBackedVerifiedPrefixV0 {
                    bytes: frontier.certified_prefix_bytes(),
                    digest: *frontier.certified_prefix_digest(),
                })
        } else {
            None
        };
        let opening = projection_observation(&source.source, &opening_live)?;
        let certified_revision_digest =
            complete_content::exact_source_revision_digest(&inner.source_revision);
        let session_key = NativeSessionKey::native_id(
            NATIVE_SESSION_NAMESPACE,
            TypedKey::utf8(source.native_session_id.clone())?,
        )?;
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source.source,
            logical_session_kind: LOGICAL_SESSION_KIND,
            native_session_key: &session_key,
        })?;
        Ok(Self {
            inner,
            selected: source.clone(),
            opening_index: admitted.index,
            source: source.source.clone(),
            native_session_id: source.native_session_id.clone(),
            session_id,
            opening,
            certified_revision_digest,
            indexed_documents,
            disposition,
            verified_base_prefix,
        })
    }

    pub(crate) fn next_page(
        &mut self,
    ) -> OpenClawSourceBackedResultV0<Option<OpenClawSourceBackedPageV0>> {
        let Some(page) = self.inner.next_page()? else {
            return Ok(None);
        };
        let complete_records = page
            .next_checkpoint
            .next_raw_ordinal
            .checked_sub(page.expected_checkpoint.next_raw_ordinal)
            .ok_or(OpenClawSourceBackedErrorV0::CountMismatch)?;
        let retained_records = u64::try_from(page.events.len())
            .map_err(|_| OpenClawSourceBackedErrorV0::CountOverflow)?;
        let rejected_records = u64::try_from(page.rejections.len())
            .map_err(|_| OpenClawSourceBackedErrorV0::CountOverflow)?;
        let session = self.lexical_session(&page.session)?;
        let mut touched_by_event = BTreeMap::<u64, BTreeSet<String>>::new();
        for touch in page.touches {
            if let Some(event_ordinal) = touch.event_ordinal {
                touched_by_event
                    .entry(event_ordinal)
                    .or_default()
                    .insert(touch.path);
            }
        }

        let mut documents = Vec::with_capacity(page.events.len());
        for event in page.events {
            let touched_files = touched_by_event
                .remove(&event.raw_ordinal)
                .map(|paths| paths.into_iter().collect())
                .unwrap_or_default();
            if let Some(document) = self.lexical_document(event, &session, touched_files)? {
                documents.push(document);
            }
        }
        let added_documents = u64::try_from(documents.len())
            .map_err(|_| OpenClawSourceBackedErrorV0::CountOverflow)?;
        self.indexed_documents = self
            .indexed_documents
            .checked_add(added_documents)
            .ok_or(OpenClawSourceBackedErrorV0::CountOverflow)?;
        Ok(Some(OpenClawSourceBackedPageV0 {
            documents,
            complete_records,
            retained_records,
            rejected_records,
            certified_prefix_bytes: page.next_checkpoint.complete_prefix_end,
            terminal: page.terminal,
        }))
    }

    pub(crate) fn finish(self) -> OpenClawSourceBackedResultV0<OpenClawSourceBackedScanV0> {
        let outcome = self
            .inner
            .outcome
            .as_ref()
            .ok_or(OpenClawSourceBackedErrorV0::ReaderNotFinished)?;
        let checkpoint = &outcome.checkpoint;
        let classified = checkpoint
            .accepted_events
            .checked_add(checkpoint.rejected_records)
            .ok_or(OpenClawSourceBackedErrorV0::CountOverflow)?;
        let ignored_records = checkpoint
            .next_raw_ordinal
            .checked_sub(classified)
            .ok_or(OpenClawSourceBackedErrorV0::CountMismatch)?;
        if self.indexed_documents > checkpoint.accepted_events {
            return Err(OpenClawSourceBackedErrorV0::CountMismatch);
        }
        self.inner.revalidate_admitted_transcript()?;
        if let Some(index) = &self.opening_index {
            index.revalidate()?;
        }
        self.selected.authority.revalidate()?;
        let closing_admitted = self.selected.admit()?;
        closing_admitted.revalidate(&self.selected.authority)?;
        let closing = projection_observation(&self.source, &closing_admitted.observation)?;
        let frontier = SourceFrontier::new(
            FRONTIER_KIND,
            TypedKey::bytes(serde_json::to_vec(checkpoint)?)?,
            checkpoint.complete_prefix_end,
            checkpoint.complete_prefix_sha256,
        )?;
        let certified_source = CertifiedSource::certify_with_frontier(
            self.opening,
            closing,
            PARSER_REVISION,
            checkpoint.complete_prefix_sha256,
            ScannedSourceCounts {
                complete_records: checkpoint.next_raw_ordinal,
                retained_records: checkpoint.accepted_events,
                rejected_records: checkpoint.rejected_records,
                ignored_records,
                indexed_documents: self.indexed_documents,
                certified_bytes: checkpoint.complete_prefix_end,
            },
            Some(frontier),
        )?;
        Ok(OpenClawSourceBackedScanV0 {
            certified_source,
            disposition: self.disposition,
            verified_base_prefix: self.verified_base_prefix,
        })
    }

    fn lexical_document(
        &self,
        event: CoreEvent,
        session: &OpenClawLexicalSessionV0,
        touched_files: Vec<String>,
    ) -> OpenClawSourceBackedResultV0<Option<LexicalDocument>> {
        let body = openclaw_lexical_body(event.event_type, &event.lexical_text);
        if body.trim().is_empty() {
            return Ok(None);
        }
        let (native_item_key, native_event_key) = match event.native_record_id.as_deref() {
            Some(native_record_id) => {
                let key = TypedKey::utf8(native_record_id)?;
                (
                    NativeItemKey::native_id(NATIVE_EVENT_NAMESPACE, key.clone())?,
                    key,
                )
            }
            None => (
                NativeItemKey::certified_position(
                    NATIVE_EVENT_POSITION_KIND,
                    TypedKey::U64(event.raw_ordinal),
                    PositionStability::AppendStable,
                )?,
                TypedKey::U64(event.raw_ordinal),
            ),
        };
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })?;
        let byte_length = event
            .byte_end_exclusive
            .checked_sub(event.byte_start)
            .ok_or(OpenClawSourceBackedErrorV0::LocatorRangeInvalid)?;
        let locator = SourceRecordLocator::new(
            self.source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: event.byte_start,
                byte_length,
                physical_ordinal: event.raw_ordinal,
                native_session_key: Some(TypedKey::utf8(self.native_session_id.clone())?),
                native_event_key: Some(native_event_key),
            },
            LocatorRevisionPolicy::ExactSourceRevision,
            Some(self.certified_revision_digest),
            event.record_digest,
        )?;
        Ok(Some(LexicalDocument {
            event_id,
            session_id: self.session_id,
            parent_session_id: session.parent_session_id,
            root_session_id: session.root_session_id,
            source: self.source.clone(),
            locator,
            provider_session_id: Some(session.provider_session_id.clone()),
            branch: session.branch.clone(),
            source_path: Some(self.inner.path.display().to_string()),
            agent_type: AgentType::Primary.as_str().to_owned(),
            is_primary: true,
            event_sequence: event.provider_event_sequence_index,
            occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
            event_type: event.event_type.as_str().to_owned(),
            role: event.role.map(|role| role.as_str().to_owned()),
            body,
            workspace: None,
            cwd: session.cwd.clone(),
            touched_files,
        }))
    }

    fn lexical_session(
        &self,
        session: &super::SessionFact,
    ) -> OpenClawSourceBackedResultV0<OpenClawLexicalSessionV0> {
        let provider_session_id = session.cursor.provider_session_id.clone();
        let parent_session_id = session
            .cursor
            .parent_provider_session_id
            .as_deref()
            .map(|parent_provider_session_id| {
                related_session_identity(
                    parent_provider_session_id,
                    &provider_session_id,
                    self.session_id,
                )
            })
            .transpose()?;
        let root_session_id = session
            .cursor
            .root_provider_session_id
            .as_deref()
            .map(|root_provider_session_id| {
                related_session_identity(
                    root_provider_session_id,
                    &provider_session_id,
                    self.session_id,
                )
            })
            .transpose()?
            .or(parent_session_id)
            .unwrap_or(self.session_id);
        Ok(OpenClawLexicalSessionV0 {
            provider_session_id,
            parent_session_id,
            root_session_id,
            branch: explicit_branch(&session.index).or_else(|| explicit_branch(&session.header)),
            cwd: session.cursor.cwd.clone(),
        })
    }
}

fn openclaw_lexical_body(event_type: EventType, text: &str) -> String {
    if text.trim().is_empty() {
        format!("OpenClaw {}", event_type.as_str())
    } else {
        text.to_owned()
    }
}

struct OpenClawLexicalSessionV0 {
    provider_session_id: String,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    branch: Option<String>,
    cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenClawHydratedRecordV0 {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: Option<String>,
}

fn hydrate_locator(
    selected: &OpenClawSourceBackedSourceV0,
    locator: &SourceRecordLocator,
) -> OpenClawSourceBackedResultV0<OpenClawHydratedRecordV0> {
    let (byte_offset, byte_length, physical_ordinal, native_event_key) =
        validate_locator(selected, locator)?;
    let admitted = selected.admit()?;
    let observation = &admitted.observation;
    let expected_revision =
        complete_content::exact_source_revision_digest(&observation.source_revision());
    if locator.certified_source_revision_digest() != Some(&expected_revision) {
        return Err(OpenClawSourceBackedErrorV0::LocatorSourceRevisionMismatch);
    }
    let range_end = byte_offset
        .checked_add(byte_length)
        .ok_or(OpenClawSourceBackedErrorV0::LocatorRangeInvalid)?;
    if byte_length == 0 || byte_length > MAX_HYDRATED_RECORD_BYTES {
        return Err(OpenClawSourceBackedErrorV0::LocatorRangeInvalid);
    }
    if range_end > observation.transcript.length {
        return Err(OpenClawSourceBackedErrorV0::LocatorRangeMissing);
    }
    if byte_offset > 0 {
        let boundary = admitted
            .transcript
            .read_exact_range(byte_offset - 1, 1, 1)?;
        if boundary != b"\n" {
            return Err(OpenClawSourceBackedErrorV0::LocatorRangeMissing);
        }
    }
    let record_length = usize::try_from(byte_length)
        .map_err(|_| OpenClawSourceBackedErrorV0::LocatorRangeInvalid)?;
    let source_bytes = admitted.transcript.read_exact_range(
        byte_offset,
        record_length,
        MAX_HYDRATED_RECORD_BYTES as usize,
    )?;
    let first_newline = source_bytes.iter().position(|byte| *byte == b'\n');
    if first_newline.is_some_and(|position| position + 1 != source_bytes.len())
        || (first_newline.is_none() && range_end != observation.transcript.length)
    {
        return Err(OpenClawSourceBackedErrorV0::LocatorRangeMissing);
    }
    let record = strip_jsonl_terminator(&source_bytes);
    let observed_record_digest: [u8; 32] = Sha256::digest(record).into();
    if &observed_record_digest != locator.record_digest() {
        return Err(OpenClawSourceBackedErrorV0::LocatorDigestMismatch);
    }
    let value: Value = serde_json::from_slice(record)?;
    let line_number = usize::try_from(physical_ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .ok_or(OpenClawSourceBackedErrorV0::InvalidLocator)?;
    let observed_native_id = value.get("id").and_then(Value::as_str);
    match (&native_event_key, observed_native_id) {
        (TypedKey::Utf8(expected), Some(observed)) if expected == observed => {}
        (TypedKey::Utf8(_), _) => {
            return Err(OpenClawSourceBackedErrorV0::LocatorRecordIdentityMismatch);
        }
        (TypedKey::U64(expected), _) if *expected == physical_ordinal => {}
        _ => return Err(OpenClawSourceBackedErrorV0::InvalidLocator),
    }
    admitted.revalidate(&selected.authority)?;
    let closing = selected.admit()?;
    closing.revalidate(&selected.authority)?;
    let closing_revision =
        complete_content::exact_source_revision_digest(&closing.observation.source_revision());
    if closing_revision != expected_revision {
        return Err(OpenClawSourceBackedErrorV0::LocatorSourceRevisionMismatch);
    }
    let mut event = normalization::event_fact(
        physical_ordinal,
        line_number,
        &value,
        DateTime::<Utc>::UNIX_EPOCH,
    );
    if let Some(output) = openclaw_output_metadata(&value, line_number, None) {
        if output.kind == OutputObservationKind::Command {
            event.event_type = EventType::CommandOutput;
        }
    }
    let decoded_display_text = openclaw_lexical_body(event.event_type, &event.lexical_text);
    Ok(OpenClawHydratedRecordV0 {
        provider_bytes: decoded_display_text.as_bytes().to_vec(),
        decoded_display_text: Some(decoded_display_text),
    })
}

fn validate_locator(
    selected: &OpenClawSourceBackedSourceV0,
    locator: &SourceRecordLocator,
) -> OpenClawSourceBackedResultV0<(u64, u64, u64, TypedKey)> {
    locator.validate_contract()?;
    selected
        .source
        .validate_exact_descriptor(locator.source())?;
    if locator.source().provider() != CaptureProvider::OpenClaw.as_str()
        || locator.source().source_format() != OPENCLAW_SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(OpenClawSourceBackedErrorV0::InvalidLocator);
    }
    let SourceAnchor::ProviderNative { namespace, key } = locator.source().anchor() else {
        return Err(OpenClawSourceBackedErrorV0::InvalidLocator);
    };
    if namespace != SOURCE_ANCHOR_NAMESPACE
        || key != &TypedKey::Utf8(selected.native_session_id.clone())
    {
        return Err(OpenClawSourceBackedErrorV0::InvalidLocator);
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = locator.coordinate()
    else {
        return Err(OpenClawSourceBackedErrorV0::InvalidLocator);
    };
    if native_session_key.as_ref() != Some(&TypedKey::Utf8(selected.native_session_id.clone())) {
        return Err(OpenClawSourceBackedErrorV0::InvalidLocator);
    }
    let native_event_key = native_event_key
        .clone()
        .ok_or(OpenClawSourceBackedErrorV0::InvalidLocator)?;
    let valid_event_key = match &native_event_key {
        TypedKey::Utf8(_) => true,
        TypedKey::U64(value) => *value == *physical_ordinal,
        _ => false,
    };
    if !valid_event_key {
        return Err(OpenClawSourceBackedErrorV0::InvalidLocator);
    }
    Ok((
        *byte_offset,
        *byte_length,
        *physical_ordinal,
        native_event_key,
    ))
}

fn decode_previous(
    previous: &CertifiedSource,
    source: &SourceKey,
) -> OpenClawSourceBackedResultV0<Checkpoint> {
    previous.validate_contract()?;
    source.validate_exact_descriptor(previous.observation().source())?;
    if previous.parser_revision() != PARSER_REVISION {
        return Err(OpenClawSourceBackedErrorV0::InvalidFrontier);
    }
    let frontier = previous
        .frontier()
        .ok_or(OpenClawSourceBackedErrorV0::InvalidFrontier)?;
    if frontier.checkpoint_kind() != FRONTIER_KIND {
        return Err(OpenClawSourceBackedErrorV0::InvalidFrontier);
    }
    let TypedKey::Bytes(encoded) = frontier.checkpoint() else {
        return Err(OpenClawSourceBackedErrorV0::InvalidFrontier);
    };
    let checkpoint: Checkpoint = serde_json::from_slice(encoded)?;
    if !checkpoint.supported()
        || checkpoint.complete_prefix_end != frontier.certified_prefix_bytes()
        || checkpoint.complete_prefix_sha256 != *frontier.certified_prefix_digest()
    {
        return Err(OpenClawSourceBackedErrorV0::InvalidFrontier);
    }
    Ok(checkpoint)
}

fn projection_observation(
    source: &SourceKey,
    observation: &super::OpenClawSessionObservation,
) -> OpenClawSourceBackedResultV0<ProjectionSourceObservation> {
    Ok(ProjectionSourceObservation::new(
        source.clone(),
        SOURCE_REVISION_KIND,
        serde_json::to_vec(&LegacySourceObservation::from_live(observation))?,
    )?)
}

fn source_key(native_session_id: &str) -> OpenClawSourceBackedResultV0<SourceKey> {
    let anchor =
        SourceAnchor::provider_native(SOURCE_ANCHOR_NAMESPACE, TypedKey::utf8(native_session_id)?)?;
    Ok(SourceKey::derive(
        CaptureProvider::OpenClaw.as_str(),
        OPENCLAW_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn related_session_identity(
    related_provider_session_id: &str,
    direct_provider_session_id: &str,
    direct_session_id: StableEntityId,
) -> OpenClawSourceBackedResultV0<StableEntityId> {
    if related_provider_session_id == direct_provider_session_id {
        return Ok(direct_session_id);
    }
    let related_source = source_key(related_provider_session_id)?;
    let related_session_key = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(related_provider_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source: &related_source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &related_session_key,
    })?)
}

fn explicit_branch(value: &Value) -> Option<String> {
    ["branch", "gitBranch", "git_branch"]
        .into_iter()
        .find_map(|field| value.get(field).and_then(Value::as_str))
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(super::capped_text)
}

fn native_session_id(path: &Path) -> String {
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("openclaw-session");
    super::qualify_session_id(super::openclaw_agent_id(path).as_deref(), fallback_id)
}

fn disposition(change: SourceChange) -> OpenClawSourceBackedDispositionV0 {
    match change {
        SourceChange::Fresh => OpenClawSourceBackedDispositionV0::Cold,
        SourceChange::Unchanged => OpenClawSourceBackedDispositionV0::Noop,
        SourceChange::Append => OpenClawSourceBackedDispositionV0::Append,
        SourceChange::Rewrite | SourceChange::Truncation | SourceChange::Replacement => {
            OpenClawSourceBackedDispositionV0::Replacement
        }
    }
}

fn strip_jsonl_terminator(record: &[u8]) -> &[u8] {
    record
        .strip_suffix(b"\n")
        .unwrap_or(record)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| record.strip_suffix(b"\n").unwrap_or(record))
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
    };

    use ctx_history_core::LocatorRevisionPolicy;
    use serde_json::{json, Value};

    use super::*;

    #[test]
    fn openclaw_source_backed_cold_extraction_retains_full_body_and_is_stable() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openclaw");
        let transcript = transcript_path(&root);
        let complete = format!("{} exact-tail", "bounded lexical projection ".repeat(180));
        write_fixture(
            &transcript,
            &[
                header("session-1"),
                message("message-1", "user", &complete),
                message("message-2", "assistant", "short answer"),
            ],
        );
        let adapter = openclaw_source_backed_adapter_v0();
        let sources = adapter.discover_selected(&root).unwrap();
        assert_eq!(sources.len(), 1);

        let (first_documents, first_scan) = extract(&adapter, &sources[0]);
        let (second_documents, second_scan) = extract(&adapter, &sources[0]);
        assert_eq!(first_documents.len(), 2);
        assert_eq!(
            first_documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>(),
            second_documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            first_documents[0].session_id,
            second_documents[0].session_id
        );
        assert_eq!(
            first_documents[0].source.identity(),
            second_documents[0].source.identity()
        );
        assert_eq!(
            first_documents[0].provider_session_id.as_deref(),
            Some("personal-agent/session-1")
        );
        assert!(first_documents[0].parent_session_id.is_some());
        assert_ne!(
            first_documents[0].root_session_id,
            first_documents[0].session_id
        );
        assert_eq!(
            first_documents[0].branch.as_deref(),
            Some("feature/openclaw")
        );
        assert_eq!(
            first_documents[0].source_path.as_deref(),
            transcript.to_str()
        );
        assert_eq!(first_documents[0].agent_type, "primary");
        assert!(first_documents[0].is_primary);
        assert_eq!(first_documents[0].body, complete);
        assert!(first_documents[0].body.ends_with("exact-tail"));
        assert_eq!(
            first_documents[0].locator.revision_policy(),
            LocatorRevisionPolicy::ExactSourceRevision
        );
        assert!(first_documents[0]
            .locator
            .certified_source_revision_digest()
            .is_some());

        let counts = first_scan.certified_source.counts();
        assert_eq!(counts.complete_records, 3);
        assert_eq!(counts.retained_records, 2);
        assert_eq!(counts.rejected_records, 0);
        assert_eq!(counts.ignored_records, 1);
        assert_eq!(counts.indexed_documents, 2);
        assert_eq!(
            first_scan.certified_source.content_digest(),
            second_scan.certified_source.content_digest()
        );
        assert_eq!(
            first_scan.disposition,
            OpenClawSourceBackedDispositionV0::Cold
        );
        assert!(first_scan.verified_base_prefix.is_none());
    }

    #[test]
    fn openclaw_source_backed_exact_hydration_returns_source_bytes_and_fails_after_append() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openclaw");
        let transcript = transcript_path(&root);
        let complete = format!("{} exact hydration tail", "long message ".repeat(300));
        write_fixture(
            &transcript,
            &[header("session-1"), message("message-1", "user", &complete)],
        );
        let adapter = openclaw_source_backed_adapter_v0();
        let source = adapter.discover_selected(&root).unwrap().remove(0);
        let (documents, _) = extract(&adapter, &source);
        let hydrated = adapter.hydrate(&source, &documents[0].locator).unwrap();
        assert_eq!(
            hydrated.decoded_display_text.as_deref(),
            Some(complete.as_str())
        );
        assert!(std::str::from_utf8(&hydrated.provider_bytes)
            .unwrap()
            .contains("exact hydration tail"));

        let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
        serde_json::to_writer(
            &mut file,
            &message("message-2", "assistant", "later append"),
        )
        .unwrap();
        file.write_all(b"\n").unwrap();
        let error = adapter.hydrate(&source, &documents[0].locator).unwrap_err();
        assert!(matches!(
            error,
            OpenClawSourceBackedErrorV0::LocatorSourceRevisionMismatch
        ));
    }

    #[test]
    fn openclaw_source_backed_rejects_current_agent_sqlite_without_format_expansion() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openclaw");
        let sqlite = root.join("agents/main/agent/openclaw-agent.sqlite");
        fs::create_dir_all(sqlite.parent().unwrap()).unwrap();
        fs::write(&sqlite, b"SQLite format 3\0").unwrap();

        let error = openclaw_source_backed_adapter_v0()
            .discover_selected(&root)
            .unwrap_err();
        match error {
            OpenClawSourceBackedErrorV0::UnsupportedSelectedSource { path, reason } => {
                assert_eq!(path, root);
                assert!(reason.contains("openclaw-agent.sqlite"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn compound_authority_openclaw_rejects_missing_auxiliary_appearing_before_publication() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openclaw");
        let transcript = transcript_path(&root);
        let records = [header("session-1"), message("message-1", "user", "hello")];
        write_fixture(&transcript, &records);
        let index = transcript.parent().unwrap().join("sessions.json");
        fs::remove_file(&index).unwrap();

        let adapter = openclaw_source_backed_adapter_v0();
        let source = adapter.discover_selected(&root).unwrap().remove(0);
        let mut reader = adapter
            .open_source(&source, "2026-07-28T12:00:00Z".parse().unwrap(), None)
            .unwrap();
        while reader.next_page().unwrap().is_some() {}
        fs::write(&index, r#"{"session-1":{"sessionId":"session-1"}}"#).unwrap();

        assert!(reader.finish().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn compound_authority_openclaw_rejects_ancestor_swap_and_stale_locator() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("openclaw");
        let transcript = transcript_path(&root);
        let records = [header("session-1"), message("message-1", "user", "hello")];
        write_fixture(&transcript, &records);

        let adapter = openclaw_source_backed_adapter_v0();
        let source = adapter.discover_selected(&root).unwrap().remove(0);
        let (documents, _) = extract(&adapter, &source);
        let retired = temp.path().join("retired-openclaw");
        fs::rename(&root, &retired).unwrap();
        write_fixture(&transcript, &records);

        assert!(adapter.hydrate(&source, &documents[0].locator).is_err());
    }

    fn extract(
        adapter: &OpenClawSourceBackedAdapterV0,
        source: &OpenClawSourceBackedSourceV0,
    ) -> (Vec<LexicalDocument>, OpenClawSourceBackedScanV0) {
        let mut reader = adapter
            .open_source(source, "2026-07-28T12:00:00Z".parse().unwrap(), None)
            .unwrap();
        let mut documents = Vec::new();
        while let Some(page) = reader.next_page().unwrap() {
            assert!(page.complete_records > 0);
            assert!(page.retained_records >= page.documents.len() as u64);
            assert_eq!(page.rejected_records, 0);
            assert!(page.certified_prefix_bytes > 0);
            documents.extend(page.documents);
        }
        (documents, reader.finish().unwrap())
    }

    fn transcript_path(root: &Path) -> PathBuf {
        root.join("agents/personal-agent/sessions/session-1.jsonl")
    }

    fn header(id: &str) -> Value {
        json!({
            "type": "session",
            "id": id,
            "timestamp": "2026-07-28T12:00:00Z",
            "cwd": "/workspace/openclaw",
        })
    }

    fn message(id: &str, role: &str, content: &str) -> Value {
        json!({
            "type": "message",
            "id": id,
            "timestamp": "2026-07-28T12:00:01Z",
            "message": {
                "role": role,
                "content": content,
            }
        })
    }

    fn write_fixture(path: &Path, records: &[Value]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = Vec::new();
        for record in records {
            serde_json::to_writer(&mut bytes, record).unwrap();
            bytes.push(b'\n');
        }
        fs::write(path, bytes).unwrap();
        fs::write(
            path.parent().unwrap().join("sessions.json"),
            json!({
                "session-1": {
                    "sessionId": "session-1",
                    "label": "source-backed fixture",
                    "parentSessionId": "parent-1",
                    "rootSessionId": "root-1",
                    "branch": "feature/openclaw",
                }
            })
            .to_string(),
        )
        .unwrap();
    }
}
