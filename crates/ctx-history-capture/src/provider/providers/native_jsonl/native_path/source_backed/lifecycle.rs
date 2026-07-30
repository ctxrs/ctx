use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::Mutex,
};

use ctx_history_core::{
    derive_event_id, CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion,
    CertifiedSourceInventory, EventIdentityInput, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, PositionStability, ScannedSourceCounts, SourceFrontier, SourceKey,
    SourceObservation, SourceRecordLocator, SubrecordSelector, TypedKey,
};
use ctx_history_index::LexicalDocument;

use super::{
    direct_jsonl_session_identity, DirectJsonlCheckpoint, DirectJsonlDisposition, DirectJsonlEvent,
    DirectJsonlProjector, DirectJsonlSelectedLeaf, DirectJsonlSession, DirectJsonlSourceAdapter,
    DirectJsonlSourceBackedError, DirectJsonlSourceBackedResult, ProjectedLine,
    DIRECT_JSONL_DISCOVERY_REVISION, DIRECT_JSONL_DOCUMENT_METADATA_BYTES,
    DIRECT_JSONL_MAX_EXPANDED_RECORD_BYTES, DIRECT_JSONL_MAX_EXPANDED_RECORD_UNITS,
    DIRECT_JSONL_MAX_REJECTION_DETAILS, DIRECT_JSONL_MAX_TOUCHED_FILES,
    DIRECT_JSONL_NATIVEPATH_PARSER_REVISION, DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
    DIRECT_JSONL_SOURCE_BACKED_PARSER_REVISION, DIRECT_JSONL_SOURCE_FRONTIER_KIND,
};
use crate::{
    provider::source_backed::family::jsonl::{
        revalidate_frozen_prefix, JsonlFileObservation, JsonlReader,
    },
    CaptureError,
};

impl DirectJsonlSourceAdapter {
    pub(super) fn revalidate_certificate(
        self,
        terminal_evidence: &DirectJsonlTerminalEvidenceSet,
        expected: &CertifiedSource,
    ) -> DirectJsonlSourceBackedResult<bool> {
        Ok(self
            .terminal_certificate_evidence(terminal_evidence, expected)?
            .is_some())
    }

    fn revalidate_certificate_filesystem(
        self,
        terminal_evidence: &DirectJsonlTerminalEvidenceSet,
        expected: &CertifiedSource,
    ) -> DirectJsonlSourceBackedResult<bool> {
        let Some((evidence, checkpoint)) =
            self.terminal_certificate_evidence(terminal_evidence, expected)?
        else {
            return Ok(false);
        };
        let (_, source_file) = evidence.leaf.open_for_scan()?;
        revalidate_frozen_prefix(
            &evidence.leaf.path,
            source_file.as_ref(),
            checkpoint.physical.source_observation(),
            checkpoint.physical.complete_prefix_end(),
            *checkpoint.physical.complete_prefix_sha256(),
        )?;
        Ok(true)
    }

    fn terminal_certificate_evidence(
        self,
        terminal_evidence: &DirectJsonlTerminalEvidenceSet,
        expected: &CertifiedSource,
    ) -> DirectJsonlSourceBackedResult<Option<(DirectJsonlTerminalEvidence, DirectJsonlCheckpoint)>>
    {
        let checkpoint = match decode_certificate(self, expected) {
            Ok(checkpoint) => checkpoint,
            Err(_) => return Ok(None),
        };
        let Some(evidence) = terminal_evidence.get(expected.observation().source())? else {
            return Ok(None);
        };
        if evidence.certificate != *expected
            || evidence.leaf.path != *checkpoint.physical.identity().source_path()
            || !evidence.leaf.observation.supports_exact_revalidation()
        {
            return Ok(None);
        }
        Ok(Some((evidence, checkpoint)))
    }

    #[cfg(test)]
    pub(crate) fn revalidate_inventory(
        self,
        root: &Path,
        expected: &CertifiedSourceInventory,
    ) -> DirectJsonlSourceBackedResult<bool> {
        if expected.validate_contract().is_err()
            || expected.discovery_revision() != DIRECT_JSONL_DISCOVERY_REVISION
            || expected.observation().provider() != self.provider.as_str()
        {
            return Ok(false);
        }
        let current = self.discover(root)?;
        Ok(current.is_exact_complete() && &current.observation == expected.observation())
    }

    pub(super) fn revalidate_inventory_with_evidence(
        self,
        root: &Path,
        terminal_evidence: &DirectJsonlTerminalEvidenceSet,
        expected: &CertifiedSourceInventory,
    ) -> DirectJsonlSourceBackedResult<bool> {
        if expected.validate_contract().is_err()
            || expected.discovery_revision() != DIRECT_JSONL_DISCOVERY_REVISION
            || expected.observation().provider() != self.provider.as_str()
        {
            return Ok(false);
        }
        let current = self.discover(root)?;
        let evidence = terminal_evidence.all()?;
        let rejected_paths = terminal_evidence.rejected_paths()?;
        if !current.is_exact_complete()
            || &current.observation != expected.observation()
            || evidence.len() != expected.observed_sources()
            || evidence.len().saturating_add(rejected_paths.len()) != current.leaves().len()
            || evidence
                .iter()
                .any(|source| !expected.contains(source.certificate.observation().source()))
        {
            return Ok(false);
        }
        for source in evidence {
            if !self.revalidate_certificate_filesystem(terminal_evidence, &source.certificate)? {
                return Ok(false);
            }
        }
        for path in rejected_paths {
            let Some(leaf) = current.leaves().iter().find(|leaf| leaf.path == path) else {
                return Ok(false);
            };
            if !matches!(
                self.select_leaf(leaf, chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
                Err(DirectJsonlSourceBackedError::RejectedSource { .. })
            ) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn revalidate_deletion(
        self,
        root: &Path,
        deletion: &CertifiedSourceDeletion,
    ) -> DirectJsonlSourceBackedResult<bool> {
        if deletion.validate_contract().is_err()
            || !self.owns(deletion.source())
            || deletion.discovery_revision() != DIRECT_JSONL_DISCOVERY_REVISION
            || deletion.inventory().provider() != self.provider.as_str()
        {
            return Ok(false);
        }
        let current = self.discover(root)?;
        Ok(current.is_exact_complete() && &current.observation == deletion.inventory())
    }
}

pub(crate) struct DirectJsonlSourceReader {
    pub(super) adapter: DirectJsonlSourceAdapter,
    pub(super) selected: DirectJsonlSelectedLeaf,
    pub(super) reader: JsonlReader,
    pub(super) projector: DirectJsonlProjector,
    pub(super) base: Option<CertifiedSource>,
    pub(super) disposition: DirectJsonlDisposition,
    pub(super) accepted_events: u64,
    pub(super) accepted_file_touches: u64,
    pub(super) rejected_records: u64,
    pub(super) rejection_details: Vec<super::super::DirectJsonlRejection>,
    pub(super) represented_physical_records: u64,
    pub(super) ignored_records: u64,
    pub(super) indexed_documents: u64,
    pub(super) pending_projected: Option<ProjectedLine>,
    pub(super) exhausted: bool,
}

impl DirectJsonlSourceReader {
    pub(crate) fn source(&self) -> &SourceKey {
        &self.selected.source
    }

    pub(crate) fn disposition(&self) -> DirectJsonlDisposition {
        self.disposition
    }

    pub(crate) fn visit_documents(
        &mut self,
        emit: &mut impl FnMut(LexicalDocument) -> DirectJsonlSourceBackedResult<()>,
    ) -> DirectJsonlSourceBackedResult<()> {
        if self.exhausted {
            return Ok(());
        }
        if let Some(projected) = self.pending_projected.take() {
            emit_projected(
                self.adapter,
                &self.selected,
                self.projector.session(),
                projected,
                &mut self.accepted_events,
                &mut self.accepted_file_touches,
                &mut self.rejected_records,
                &mut self.rejection_details,
                &mut self.represented_physical_records,
                &mut self.ignored_records,
                &mut self.indexed_documents,
                emit,
            )?;
        }
        loop {
            let adapter = self.adapter;
            let selected = &self.selected;
            let projector = &mut self.projector;
            let accepted_events = &mut self.accepted_events;
            let accepted_file_touches = &mut self.accepted_file_touches;
            let rejected_records = &mut self.rejected_records;
            let rejection_details = &mut self.rejection_details;
            let represented_physical_records = &mut self.represented_physical_records;
            let ignored_records = &mut self.ignored_records;
            let indexed_documents = &mut self.indexed_documents;
            let mut visit =
                |record: crate::provider::source_backed::family::jsonl::JsonlRecordRef<'_>|
                 -> DirectJsonlSourceBackedResult<()> {
                    let projected = projector.project_record(record)?;
                    emit_projected(
                        adapter,
                        selected,
                        projector.session(),
                        projected,
                        accepted_events,
                        accepted_file_touches,
                        rejected_records,
                        rejection_details,
                        represented_physical_records,
                        ignored_records,
                        indexed_documents,
                        emit,
                    )
                };
            let Some(_page) = self.reader.visit_page(&mut visit)? else {
                self.exhausted = true;
                return Ok(());
            };
        }
    }

    pub(crate) fn finish(self) -> DirectJsonlSourceBackedResult<DirectJsonlScanReceipt> {
        if !self.exhausted || self.reader.outcome().is_none() {
            return Err(DirectJsonlSourceBackedError::IncompleteScan);
        }
        let outcome = self
            .reader
            .outcome()
            .ok_or(DirectJsonlSourceBackedError::IncompleteScan)?;
        let session = self.projector.session().cloned().ok_or_else(|| {
            DirectJsonlSourceBackedError::MissingNativeSession(self.selected.leaf.path.clone())
        })?;
        if session.native_session_id != self.selected.session.native_session_id
            || session.provider_session_id != self.selected.session.provider_session_id
        {
            return Err(DirectJsonlSourceBackedError::NativeSessionChanged);
        }
        let checkpoint = DirectJsonlCheckpoint {
            version: DirectJsonlCheckpoint::VERSION,
            physical: outcome.checkpoint().clone(),
            accepted_events: self.accepted_events,
            accepted_file_touches: self.accepted_file_touches,
            rejected_records: self.rejected_records,
            rejection_details: self.rejection_details.clone(),
            represented_physical_records: self.represented_physical_records,
            ignored_records: self.ignored_records,
            indexed_documents: self.indexed_documents,
            session: Some(session),
        };
        if !checkpoint.is_internally_consistent() {
            return Err(DirectJsonlSourceBackedError::CountMismatch);
        }
        let complete_records = self
            .accepted_events
            .checked_add(self.rejected_records)
            .and_then(|value| value.checked_add(self.ignored_records))
            .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
        let closing = self.reader.observation().clone();
        if closing != self.selected.leaf.observation {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let certificate = if self.disposition == DirectJsonlDisposition::Unchanged {
            let base = self
                .base
                .as_ref()
                .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
            if decode_certificate(self.adapter, base)? != checkpoint {
                return Err(DirectJsonlSourceBackedError::CountMismatch);
            }
            base.clone()
        } else {
            if &closing != checkpoint.physical.source_observation() {
                return Err(CaptureError::SourceChangedDuringCapture.into());
            }
            let opening =
                source_observation(&self.selected.source, &self.selected.leaf.observation)?;
            let closing_observation = source_observation(&self.selected.source, &closing)?;
            let frontier = SourceFrontier::new(
                DIRECT_JSONL_SOURCE_FRONTIER_KIND,
                TypedKey::bytes(serde_json::to_vec(&checkpoint)?)?,
                checkpoint.physical.complete_prefix_end(),
                *checkpoint.physical.complete_prefix_sha256(),
            )?;
            CertifiedSource::certify_with_frontier(
                opening,
                closing_observation,
                DIRECT_JSONL_SOURCE_BACKED_PARSER_REVISION,
                *checkpoint.physical.complete_prefix_sha256(),
                ScannedSourceCounts {
                    complete_records,
                    retained_records: self.accepted_events,
                    rejected_records: self.rejected_records,
                    ignored_records: self.ignored_records,
                    indexed_documents: self.indexed_documents,
                    certified_bytes: checkpoint.physical.complete_prefix_end(),
                },
                Some(frontier),
            )?
        };
        let append = match self.disposition {
            DirectJsonlDisposition::Unchanged | DirectJsonlDisposition::Append => {
                let base = self
                    .base
                    .as_ref()
                    .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
                let base_frontier = base
                    .frontier()
                    .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
                let append = CertifiedSourceAppend::certify(
                    base,
                    certificate.clone(),
                    base_frontier.certified_prefix_bytes(),
                    *base_frontier.certified_prefix_digest(),
                )?;
                if self.disposition == DirectJsonlDisposition::Unchanged && append.current() != base
                {
                    return Err(DirectJsonlSourceBackedError::CountMismatch);
                }
                Some(append)
            }
            DirectJsonlDisposition::Cold | DirectJsonlDisposition::Replace => None,
        };
        let terminal_evidence = DirectJsonlTerminalEvidence {
            certificate: certificate.clone(),
            leaf: self.selected.leaf.clone(),
        };
        Ok(DirectJsonlScanReceipt {
            source: self.selected.source,
            certificate,
            rejection_details: self.rejection_details,
            #[cfg(test)]
            disposition: self.disposition,
            append,
            terminal_evidence,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_projected(
    adapter: DirectJsonlSourceAdapter,
    selected: &DirectJsonlSelectedLeaf,
    session: Option<&DirectJsonlSession>,
    projected: ProjectedLine,
    accepted_events: &mut u64,
    accepted_file_touches: &mut u64,
    rejected_records: &mut u64,
    rejection_details: &mut Vec<super::super::DirectJsonlRejection>,
    represented_physical_records: &mut u64,
    ignored_records: &mut u64,
    indexed_documents: &mut u64,
    emit: &mut impl FnMut(LexicalDocument) -> DirectJsonlSourceBackedResult<()>,
) -> DirectJsonlSourceBackedResult<()> {
    let expanded_units = projected
        .events
        .iter()
        .map(|event| 1_usize.saturating_add(event.touches.len()))
        .sum::<usize>()
        .saturating_add(projected.rejections.len())
        .max(1);
    if expanded_units > DIRECT_JSONL_MAX_EXPANDED_RECORD_UNITS
        || projected.serialized_bytes > DIRECT_JSONL_MAX_EXPANDED_RECORD_BYTES
    {
        return Err(DirectJsonlSourceBackedError::Capture(
            CaptureError::InvalidPayload(format!(
                "{} expands past a certified direct JSONL record boundary",
                selected.leaf.path.display()
            )),
        ));
    }
    if !projected.rejections.is_empty() {
        if !projected.events.is_empty() {
            return Err(DirectJsonlSourceBackedError::CountMismatch);
        }
        let rejected = u64::try_from(projected.rejections.len())
            .map_err(|_| DirectJsonlSourceBackedError::CountMismatch)?;
        *rejected_records = rejected_records
            .checked_add(rejected)
            .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
        let remaining_details =
            DIRECT_JSONL_MAX_REJECTION_DETAILS.saturating_sub(rejection_details.len());
        rejection_details.extend(projected.rejections.into_iter().take(remaining_details));
        return Ok(());
    }
    let session = session.ok_or(DirectJsonlSourceBackedError::NativeSessionChanged)?;
    if session.native_session_id != selected.session.native_session_id
        || session.provider_session_id != selected.session.provider_session_id
        || session.parent_provider_session_id != selected.session.parent_provider_session_id
        || session.root_provider_session_id != selected.session.root_provider_session_id
    {
        return Err(DirectJsonlSourceBackedError::NativeSessionChanged);
    }
    if projected.events.is_empty() {
        *ignored_records = ignored_records
            .checked_add(1)
            .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
        return Ok(());
    }
    *represented_physical_records = represented_physical_records
        .checked_add(1)
        .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
    for event in projected.events {
        let touches = u64::try_from(event.touches.len())
            .map_err(|_| DirectJsonlSourceBackedError::CountMismatch)?;
        let document = project_event(adapter, selected, session, event)?;
        emit(document)?;
        *accepted_events = accepted_events
            .checked_add(1)
            .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
        *accepted_file_touches = accepted_file_touches
            .checked_add(touches)
            .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
        *indexed_documents = indexed_documents
            .checked_add(1)
            .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct DirectJsonlScanReceipt {
    source: SourceKey,
    certificate: CertifiedSource,
    rejection_details: Vec<super::super::DirectJsonlRejection>,
    #[cfg(test)]
    disposition: DirectJsonlDisposition,
    append: Option<CertifiedSourceAppend>,
    terminal_evidence: DirectJsonlTerminalEvidence,
}

impl DirectJsonlScanReceipt {
    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn certificate(&self) -> &CertifiedSource {
        &self.certificate
    }

    pub(crate) fn rejections(&self) -> &[super::super::DirectJsonlRejection] {
        &self.rejection_details
    }

    #[cfg(test)]
    pub(crate) fn disposition(&self) -> DirectJsonlDisposition {
        self.disposition
    }

    pub(crate) fn append(&self) -> Option<&CertifiedSourceAppend> {
        self.append.as_ref()
    }

    pub(super) fn terminal_evidence(&self) -> DirectJsonlTerminalEvidence {
        self.terminal_evidence.clone()
    }
}

#[derive(Debug, Clone)]
pub(super) struct DirectJsonlTerminalEvidence {
    certificate: CertifiedSource,
    leaf: super::DirectJsonlInventoryLeaf,
}

#[derive(Default)]
pub(super) struct DirectJsonlTerminalEvidenceSet {
    sources: Mutex<HashMap<[u8; 32], DirectJsonlTerminalEvidence>>,
    rejected_paths: Mutex<BTreeSet<PathBuf>>,
}

impl DirectJsonlTerminalEvidenceSet {
    pub(super) fn reset(&self) -> DirectJsonlSourceBackedResult<()> {
        self.sources
            .lock()
            .map_err(|_| {
                DirectJsonlSourceBackedError::Publication(
                    "terminal evidence lock was poisoned".to_owned(),
                )
            })?
            .clear();
        self.rejected_paths
            .lock()
            .map_err(|_| {
                DirectJsonlSourceBackedError::Publication(
                    "rejected terminal evidence lock was poisoned".to_owned(),
                )
            })?
            .clear();
        Ok(())
    }

    pub(super) fn record(
        &self,
        evidence: DirectJsonlTerminalEvidence,
    ) -> DirectJsonlSourceBackedResult<()> {
        let digest = evidence
            .certificate
            .observation()
            .source()
            .exact_descriptor_digest();
        let replaced = self
            .sources
            .lock()
            .map_err(|_| {
                DirectJsonlSourceBackedError::Publication(
                    "terminal evidence lock was poisoned".to_owned(),
                )
            })?
            .insert(digest, evidence);
        if replaced.is_some() {
            return Err(DirectJsonlSourceBackedError::CountMismatch);
        }
        Ok(())
    }

    pub(super) fn record_rejected_path(&self, path: PathBuf) -> DirectJsonlSourceBackedResult<()> {
        if !self
            .rejected_paths
            .lock()
            .map_err(|_| {
                DirectJsonlSourceBackedError::Publication(
                    "rejected terminal evidence lock was poisoned".to_owned(),
                )
            })?
            .insert(path)
        {
            return Err(DirectJsonlSourceBackedError::CountMismatch);
        }
        Ok(())
    }

    fn get(
        &self,
        source: &SourceKey,
    ) -> DirectJsonlSourceBackedResult<Option<DirectJsonlTerminalEvidence>> {
        Ok(self
            .sources
            .lock()
            .map_err(|_| {
                DirectJsonlSourceBackedError::Publication(
                    "terminal evidence lock was poisoned".to_owned(),
                )
            })?
            .get(&source.exact_descriptor_digest())
            .cloned())
    }

    fn all(&self) -> DirectJsonlSourceBackedResult<Vec<DirectJsonlTerminalEvidence>> {
        Ok(self
            .sources
            .lock()
            .map_err(|_| {
                DirectJsonlSourceBackedError::Publication(
                    "terminal evidence lock was poisoned".to_owned(),
                )
            })?
            .values()
            .cloned()
            .collect())
    }

    fn rejected_paths(&self) -> DirectJsonlSourceBackedResult<Vec<PathBuf>> {
        Ok(self
            .rejected_paths
            .lock()
            .map_err(|_| {
                DirectJsonlSourceBackedError::Publication(
                    "rejected terminal evidence lock was poisoned".to_owned(),
                )
            })?
            .iter()
            .cloned()
            .collect())
    }
}

pub(super) fn decode_previous(
    adapter: DirectJsonlSourceAdapter,
    selected: &DirectJsonlSelectedLeaf,
    previous: &CertifiedSource,
) -> DirectJsonlSourceBackedResult<DirectJsonlCheckpoint> {
    let checkpoint = decode_certificate(adapter, previous)?;
    selected
        .source
        .validate_exact_descriptor(previous.observation().source())?;
    let identity = adapter.physical_identity(&selected.source, &selected.leaf.path);
    let session = checkpoint
        .session
        .as_ref()
        .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
    if checkpoint.physical.identity() != &identity
        || session.native_session_id != selected.session.native_session_id
        || session.provider_session_id != selected.session.provider_session_id
        || session.parent_provider_session_id != selected.session.parent_provider_session_id
        || session.root_provider_session_id != selected.session.root_provider_session_id
    {
        return Err(DirectJsonlSourceBackedError::CountMismatch);
    }
    Ok(checkpoint)
}

pub(super) fn decode_certificate(
    adapter: DirectJsonlSourceAdapter,
    previous: &CertifiedSource,
) -> DirectJsonlSourceBackedResult<DirectJsonlCheckpoint> {
    previous.validate_contract()?;
    let source = previous.observation().source();
    if !adapter.owns(source)
        || source.schema_variant() != adapter.schema_variant
        || previous.parser_revision() != DIRECT_JSONL_SOURCE_BACKED_PARSER_REVISION
    {
        return Err(DirectJsonlSourceBackedError::CountMismatch);
    }
    let frontier = previous
        .frontier()
        .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
    if frontier.checkpoint_kind() != DIRECT_JSONL_SOURCE_FRONTIER_KIND {
        return Err(DirectJsonlSourceBackedError::CountMismatch);
    }
    let TypedKey::Bytes(encoded) = frontier.checkpoint() else {
        return Err(DirectJsonlSourceBackedError::CountMismatch);
    };
    let checkpoint: DirectJsonlCheckpoint = serde_json::from_slice(encoded)?;
    let identity = checkpoint.physical.identity();
    let counts = previous.counts();
    let complete_records = checkpoint
        .accepted_events
        .checked_add(checkpoint.rejected_records)
        .and_then(|value| value.checked_add(checkpoint.ignored_records))
        .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
    let maximum_touches = checkpoint
        .accepted_events
        .checked_mul(super::super::reader::DIRECT_JSONL_MAX_FILE_TOUCHES_PER_RECORD as u64)
        .ok_or(DirectJsonlSourceBackedError::CountMismatch)?;
    if !checkpoint.is_internally_consistent()
        || identity.provider() != adapter.provider.as_str()
        || identity.parser_revision() != DIRECT_JSONL_NATIVEPATH_PARSER_REVISION
        || identity.policy_revision() != DIRECT_JSONL_NATIVEPATH_POLICY_REVISION
        || identity.source_descriptor_digest() != &source.exact_descriptor_digest()
        || checkpoint.physical.complete_prefix_end() != frontier.certified_prefix_bytes()
        || checkpoint.physical.complete_prefix_sha256() != frontier.certified_prefix_digest()
        || checkpoint.physical.complete_prefix_sha256() != previous.content_digest()
        || checkpoint.accepted_file_touches > maximum_touches
        || complete_records != counts.complete_records
        || checkpoint.accepted_events != counts.retained_records
        || checkpoint.rejected_records != counts.rejected_records
        || checkpoint.ignored_records != counts.ignored_records
        || checkpoint.indexed_documents != counts.indexed_documents
        || checkpoint.physical.complete_prefix_end() != counts.certified_bytes
        || previous.observation()
            != &source_observation(source, checkpoint.physical.source_observation())?
    {
        return Err(DirectJsonlSourceBackedError::CountMismatch);
    }
    Ok(checkpoint)
}

fn project_event(
    adapter: DirectJsonlSourceAdapter,
    selected: &DirectJsonlSelectedLeaf,
    session: &DirectJsonlSession,
    event: DirectJsonlEvent,
) -> DirectJsonlSourceBackedResult<LexicalDocument> {
    let source = &selected.source;
    let session_id = selected.session_id;
    let native_session_id = &selected.session.native_session_id;
    let evidence = &event.source_record;
    let byte_length = evidence
        .byte_end_exclusive
        .checked_sub(evidence.byte_start)
        .ok_or(DirectJsonlSourceBackedError::MissingRecordEvidence)?;
    if byte_length == 0 {
        return Err(DirectJsonlSourceBackedError::MissingRecordEvidence);
    }
    let native_item_key = if let Some(native_record_id) = event.native_record_id.as_deref() {
        NativeItemKey::native_id(
            format!("{}.direct-jsonl-event", adapter.provider.as_str()),
            TypedKey::utf8(native_record_id)?,
        )?
    } else {
        NativeItemKey::certified_position(
            format!("{}.direct-jsonl-ordinal", adapter.provider.as_str()),
            TypedKey::U64(event.raw_ordinal),
            PositionStability::AppendStable,
        )?
    };
    let subrecord_selector = (event.sub_ordinal != 0)
        .then(|| {
            SubrecordSelector::certified_position(
                "direct-jsonl-subrecord",
                TypedKey::U64(u64::from(event.sub_ordinal)),
                PositionStability::StableSlot,
            )
        })
        .transpose()?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "direct-jsonl-event",
        native_item_key: &native_item_key,
        subrecord_selector: subrecord_selector.as_ref(),
    })?;
    let native_event_key = TypedKey::composite(vec![
        event
            .native_record_id
            .as_deref()
            .map(TypedKey::utf8)
            .transpose()?
            .unwrap_or(TypedKey::U64(event.raw_ordinal)),
        TypedKey::U64(u64::from(event.sub_ordinal)),
        TypedKey::bytes(selected.leaf.route_key.clone())?,
    ])?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: evidence.byte_start,
            byte_length,
            physical_ordinal: event.raw_ordinal,
            native_session_key: Some(TypedKey::utf8(native_session_id)?),
            native_event_key: Some(native_event_key),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        evidence.record_digest,
    )?;
    let parent_session_id = session
        .parent_provider_session_id
        .as_deref()
        .map(|parent| direct_jsonl_session_identity(adapter, parent).map(|(_, id)| id))
        .transpose()?;
    let root_session_id = match session.root_provider_session_id.as_deref() {
        Some(root) if root == session.native_session_id || root == session.provider_session_id => {
            session_id
        }
        Some(root) => direct_jsonl_session_identity(adapter, root)?.1,
        None => session_id,
    };
    let body = if event.lexical_text.trim().is_empty() {
        event.event_type.as_str().to_owned()
    } else {
        event.lexical_text.clone()
    };
    let touched_files = event
        .touches
        .into_iter()
        .map(|touch| touch.path)
        .filter(|path| path.len() <= DIRECT_JSONL_DOCUMENT_METADATA_BYTES)
        .take(DIRECT_JSONL_MAX_TOUCHED_FILES)
        .collect();
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(session.provider_session_id.clone()),
        branch: None,
        source_path: selected.leaf.path.to_str().map(str::to_owned),
        agent_type: session.agent_type.as_str().to_owned(),
        is_primary: session.is_primary,
        event_sequence: event.provider_event_sequence_index,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: Some(event.role.as_str().to_owned()),
        body,
        workspace: None,
        cwd: session.cwd.clone(),
        touched_files,
    })
}

fn source_observation(
    source: &SourceKey,
    observation: &JsonlFileObservation,
) -> DirectJsonlSourceBackedResult<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        "direct-native-jsonl-file-observation-v1",
        serde_json::to_vec(observation)?,
    )?)
}
