use std::{
    collections::BTreeSet,
    path::{Component, Path},
};

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, ContentSourceResolver, EventHydrationRequest,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy,
    NativeRecordCoordinate, SessionHydrationRequest, SourceAnchor, SourceKey, SourceRecordLocator,
    TypedKey,
};
use sha2::{Digest, Sha256};

use crate::{
    provider::providers::openhands::{
        event::decode_openhands_event, source::OpenHandsFileObservation,
    },
    provider_sources::EventFileInventoryError,
    CaptureError,
};

use super::{
    identities, leaf_revision_digest, lexical_body, openhands_owns_source, session_identity,
    OpenHandsEventFileAdapterV2, OpenHandsSourceBackedErrorV2, OpenHandsSourceBackedResultV2,
    OPENHANDS_OBJECT_COORDINATE_KIND, OPENHANDS_SOURCE_ANCHOR_NAMESPACE,
};

impl OpenHandsEventFileAdapterV2 {
    pub(crate) fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        hydrate_batch(&self.selected, request).map_err(hydration_failure)
    }

    pub(crate) fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let batch = BatchHydrationRequest::new(vec![request.clone()])
            .map_err(|error| hydration_failure(OpenHandsSourceBackedErrorV2::Resolver(error)))?;
        self.hydrate_batch(&batch)?
            .into_records()
            .pop()
            .ok_or_else(|| {
                hydration_failure(OpenHandsSourceBackedErrorV2::LocatorLeafNotFound(
                    "single-event hydration returned no record".to_owned(),
                ))
            })
    }
}

impl ContentSourceResolver for OpenHandsEventFileAdapterV2 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        OpenHandsEventFileAdapterV2::hydrate_event(self, request)
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        OpenHandsEventFileAdapterV2::hydrate_batch(self, request)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        if let Some(first) = request.events().first() {
            let coordinate = validate_locator(first.locator()).map_err(hydration_failure)?;
            let expected = session_identity(first.locator().source(), &coordinate.conversation_id)
                .map_err(hydration_failure)?;
            if expected != request.session_id() {
                return Err(hydration_failure(
                    OpenHandsSourceBackedErrorV2::SessionIdentityMismatch,
                ));
            }
        }
        self.hydrate_batch(request.batch())
            .map(BatchHydrationResult::into_records)
    }
}

fn hydrate_batch(
    selected: &Path,
    request: &BatchHydrationRequest,
) -> OpenHandsSourceBackedResultV2<BatchHydrationResult> {
    if request.is_empty() {
        return Ok(BatchHydrationResult::new(Vec::new())?);
    }

    let mut coordinates = Vec::with_capacity(request.len());
    let first_source = request.events()[0].locator().source().clone();
    let mut selected_leaves = BTreeSet::new();
    for event in request.events() {
        let coordinate = validate_locator(event.locator())?;
        if !event.locator().source().exact_descriptor_eq(&first_source)
            || coordinate.conversation_id
                != source_conversation_id(&first_source)
                    .ok_or(OpenHandsSourceBackedErrorV2::InvalidLocator)?
            || !selected_leaves.insert(coordinate.relative_file_key.clone())
        {
            return Err(OpenHandsSourceBackedErrorV2::InvalidLocator);
        }
        let (_, event_id) = identities(
            event.locator().source(),
            &coordinate.conversation_id,
            &coordinate.event_id,
        )?;
        if event_id != event.event_id() {
            return Err(OpenHandsSourceBackedErrorV2::EventIdentityMismatch);
        }
        validate_relative_file_key(&coordinate.relative_file_key)?;
        coordinates.push(coordinate);
    }

    let adapter = OpenHandsEventFileAdapterV2::new(selected.to_path_buf());
    let inventory = adapter.open_inventory()?;
    let conversation_id = &coordinates[0].conversation_id;
    let group = inventory.group(conversation_id).ok_or_else(|| {
        OpenHandsSourceBackedErrorV2::LocatorConversationNotFound(conversation_id.clone())
    })?;
    let plan = adapter.bind_group(group)?;
    if !plan.source.exact_descriptor_eq(&first_source) {
        return Err(OpenHandsSourceBackedErrorV2::InvalidLocator);
    }
    let leaf_ordinals = coordinates
        .iter()
        .map(|coordinate| {
            group
                .leaf_ordinal(&coordinate.relative_file_key)
                .ok_or_else(|| {
                    OpenHandsSourceBackedErrorV2::LocatorLeafNotFound(
                        coordinate.relative_file_key.clone(),
                    )
                })
        })
        .collect::<OpenHandsSourceBackedResultV2<Vec<_>>>()?;

    let mut records = Vec::with_capacity(request.len());
    for ((event, coordinate), leaf_ordinal) in request
        .events()
        .iter()
        .zip(coordinates)
        .zip(leaf_ordinals.iter().copied())
    {
        let leaf = group.leaf_at(leaf_ordinal).ok_or_else(|| {
            OpenHandsSourceBackedErrorV2::LocatorLeafNotFound(coordinate.relative_file_key.clone())
        })?;
        let provider_bytes = group.read_leaf_at(leaf_ordinal)?;
        let record_digest: [u8; 32] = Sha256::digest(&provider_bytes).into();
        let legacy_observation = OpenHandsFileObservation::from_metadata(leaf.metadata())?;
        let leaf_revision = leaf_revision_digest(
            &coordinate.relative_file_key,
            &legacy_observation,
            record_digest,
        )?;
        if leaf_revision != coordinate.leaf_revision {
            return Err(OpenHandsSourceBackedErrorV2::LeafRevisionMismatch);
        }
        if record_digest != *event.locator().record_digest() {
            return Err(OpenHandsSourceBackedErrorV2::RecordDigestMismatch);
        }
        let decoded = decode_openhands_event(leaf.display_path(), &provider_bytes)
            .map_err(|error| OpenHandsSourceBackedErrorV2::DecodeFailed(error.to_string()))?;
        if decoded.event_id() != coordinate.event_id {
            return Err(OpenHandsSourceBackedErrorV2::ObjectCoordinateMismatch);
        }
        let body =
            lexical_body(&decoded).ok_or(OpenHandsSourceBackedErrorV2::ObjectCoordinateMismatch)?;
        records.push(HydratedProviderRecord {
            event_id: event.event_id(),
            provider_bytes: body.into_bytes(),
        });
    }
    group.revalidate_leaves(leaf_ordinals)?;
    let result = BatchHydrationResult::new(records)?;
    result
        .validate_for_request(request)
        .map_err(|_| OpenHandsSourceBackedErrorV2::InvalidLocator)?;
    Ok(result)
}

pub(crate) struct LocatorCoordinate {
    pub(super) conversation_id: String,
    relative_file_key: String,
    event_id: String,
    leaf_revision: [u8; 32],
}

pub(crate) fn validate_locator(
    locator: &SourceRecordLocator,
) -> OpenHandsSourceBackedResultV2<LocatorCoordinate> {
    locator.validate_contract()?;
    let source = locator.source();
    if !openhands_owns_source(source)
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(OpenHandsSourceBackedErrorV2::InvalidLocator);
    }
    let Some(conversation_id) = source_conversation_id(source) else {
        return Err(OpenHandsSourceBackedErrorV2::InvalidLocator);
    };
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = locator.coordinate()
    else {
        return Err(OpenHandsSourceBackedErrorV2::InvalidLocator);
    };
    let TypedKey::Utf8(relative_file_key) = relative_file_key else {
        return Err(OpenHandsSourceBackedErrorV2::InvalidLocator);
    };
    validate_relative_file_key(relative_file_key)?;
    let TypedKey::Composite(parts) = record_coordinate else {
        return Err(OpenHandsSourceBackedErrorV2::InvalidLocator);
    };
    let [TypedKey::Utf8(kind), TypedKey::Utf8(event_id), TypedKey::Bytes(leaf_revision)] =
        parts.as_slice()
    else {
        return Err(OpenHandsSourceBackedErrorV2::InvalidLocator);
    };
    if kind != OPENHANDS_OBJECT_COORDINATE_KIND || leaf_revision.len() != 32 {
        return Err(OpenHandsSourceBackedErrorV2::InvalidLocator);
    }
    let mut exact_leaf_revision = [0_u8; 32];
    exact_leaf_revision.copy_from_slice(leaf_revision);
    Ok(LocatorCoordinate {
        conversation_id: conversation_id.to_owned(),
        relative_file_key: relative_file_key.clone(),
        event_id: event_id.clone(),
        leaf_revision: exact_leaf_revision,
    })
}

fn source_conversation_id(source: &SourceKey) -> Option<&str> {
    let SourceAnchor::ProviderNative { namespace, key } = source.anchor() else {
        return None;
    };
    let TypedKey::Utf8(conversation_id) = key else {
        return None;
    };
    (namespace == OPENHANDS_SOURCE_ANCHOR_NAMESPACE).then_some(conversation_id)
}

fn validate_relative_file_key(relative_file_key: &str) -> OpenHandsSourceBackedResultV2<()> {
    let path = Path::new(relative_file_key);
    if relative_file_key.is_empty()
        || relative_file_key.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(OpenHandsSourceBackedErrorV2::InvalidLocator);
    }
    Ok(())
}

fn bounded_reason(mut reason: String) -> String {
    const MAX_REASON_BYTES: usize = 4 * 1024;
    if reason.len() <= MAX_REASON_BYTES {
        return reason;
    }
    let mut end = MAX_REASON_BYTES;
    while !reason.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    reason.truncate(end);
    reason
}

pub(crate) fn hydration_failure(error: OpenHandsSourceBackedErrorV2) -> HydrationFailure {
    let kind = match &error {
        OpenHandsSourceBackedErrorV2::UnsupportedCurrentCliFormat { .. } => {
            HydrationFailureKind::UnsupportedParserRevision
        }
        OpenHandsSourceBackedErrorV2::LocatorConversationNotFound(_)
        | OpenHandsSourceBackedErrorV2::LocatorLeafNotFound(_) => {
            HydrationFailureKind::MissingRecord
        }
        OpenHandsSourceBackedErrorV2::LeafRevisionMismatch => {
            HydrationFailureKind::StaleSourceEvidence
        }
        OpenHandsSourceBackedErrorV2::RecordDigestMismatch
        | OpenHandsSourceBackedErrorV2::ObjectCoordinateMismatch
        | OpenHandsSourceBackedErrorV2::DecodeFailed(_) => {
            HydrationFailureKind::StaleRecordEvidence
        }
        OpenHandsSourceBackedErrorV2::EventFiles(
            EventFileInventoryError::SourceChanged { .. }
            | EventFileInventoryError::Unavailable { .. },
        )
        | OpenHandsSourceBackedErrorV2::Capture(CaptureError::SourceChangedDuringCapture) => {
            HydrationFailureKind::TemporarilyUnavailable
        }
        _ => HydrationFailureKind::InvalidLocator,
    };
    HydrationFailure {
        kind,
        detail: bounded_reason(error.to_string()),
    }
}
