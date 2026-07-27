use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use super::{
    normalize::{
        core_payload_fingerprint, estimated_frontier_bytes, estimated_metadata_checkpoint_bytes,
        estimated_output_bytes, estimated_rejection_bytes, estimated_revision_bytes,
        estimated_session_bytes, estimated_source_bytes, page_identity, ClineArrayCheckpoint,
        ClineCatalogCompletion, ClineCatalogIndex, ClineCatalogRejection, ClineCertifiedPage,
        ClineCertifiedRevision, ClineComponentFailure, ClineComponentFailureKind,
        ClineComponentReadOutcome, ClineComponentTransition, ClineCorePayload, ClineEventComponent,
        ClineEventKind, ClineFileSourceIdentity, ClineItemRejection, ClineItemRejectionKind,
        ClineMetadataCheckpoint, ClineNativeProfile, ClinePageAccounting, ClinePageFrontier,
        ClinePublicationStats, ClineSessionRow, ClineTaskCheckpoint, ClineTaskIdentity,
        ClineTaskIdentityOrigin, ClineTerminalEvidence, ClineTransientOutputPayload,
        CLINE_NATIVE_CORE_PAGE_MAX_BYTES, CLINE_NATIVE_FIXED_PAGE_UNITS,
        CLINE_NATIVE_MAX_REJECTIONS, CLINE_NATIVE_PAGE_MAX_BYTES, CLINE_NATIVE_SESSION_PAGE_UNITS,
        CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES,
    },
    parse::{
        hydrate_component, parse_metadata, parse_root_index, parse_scanned_item,
        pin_component_content, ClineArrayScanStep, ClineArrayScanner, ClineLocalReadError,
        ClinePinnedContentAuthority, ParsedItem,
    },
    source::{
        is_component_local_error, ClineComponent, ClineComponentObservation, ClineDiscovery,
        ClineLiveTaskObservation, ClineObservedFileState, TaskJsonNativeDialect,
    },
    ClineNativePathError,
};

#[cfg(test)]
type BeforeExposureHook = Box<dyn FnMut(&Path, ClineComponent)>;

pub(crate) struct ClineNativeReader {
    discovery: ClineDiscovery,
    dialect: TaskJsonNativeDialect,
    profile: ClineNativeProfile,
    previous_by_path: BTreeMap<PathBuf, ClineTaskCheckpoint>,
    route_index: usize,
    pending_page: Option<ClineCertifiedPage>,
    active_task: Option<ActiveTask>,
    active_array: Option<ActiveArray>,
    outcomes: Vec<ClineComponentReadOutcome>,
    live_checkpoints: Vec<ClineTaskCheckpoint>,
    stats: ClinePublicationStats,
    catalog_finished: bool,
    #[cfg(test)]
    before_exposure: Option<BeforeExposureHook>,
}

struct ActiveTask {
    task: Box<ClineLiveTaskObservation>,
    metadata: ClineMetadataCheckpoint,
    metadata_content_authority: Option<ClinePinnedContentAuthority>,
    deferred_metadata_page: Option<ClineCertifiedPage>,
    discard_deferred_metadata_on_failure: bool,
    component_failed: bool,
    component_page_certified: bool,
    identity_changed: bool,
    api_history: Option<ClineArrayCheckpoint>,
    ui_messages: Option<ClineArrayCheckpoint>,
    fallback_history: Option<ClineArrayCheckpoint>,
    event_components: Box<[ClineEventComponent]>,
    next_component: usize,
    needs_session: bool,
}

struct ActiveArray {
    task: Box<ClineLiveTaskObservation>,
    metadata: ClineMetadataCheckpoint,
    component: ClineEventComponent,
    prior: Option<ClineArrayCheckpoint>,
    scanner: ClineArrayScanner,
    source: ClineFileSourceIdentity,
    revision: ClineCertifiedRevision,
    frontier: ClinePageFrontier,
    observed_items: u64,
    retained_rows: u64,
    native_id_occurrences: BTreeMap<String, u64>,
    prior_prefix_matches: bool,
    attach_session: bool,
    page_transition: ClineComponentTransition,
    pages: usize,
}

mod advance;
mod certification;
mod lifecycle;
mod pages;

enum MetadataResolution {
    Ready(Box<MetadataReady>),
    Unsafe(ClineComponentFailure),
}

struct MetadataReady {
    checkpoint: ClineMetadataCheckpoint,
    page: Option<ClineCertifiedPage>,
    content_authority: Option<ClinePinnedContentAuthority>,
}

fn fallback_metadata(
    task: &ClineLiveTaskObservation,
    observation: ClineComponentObservation,
) -> ClineMetadataCheckpoint {
    ClineMetadataCheckpoint {
        observation,
        content_sha256: None,
        session: ClineSessionRow::new(
            ClineTaskIdentity::new(task.directory_task_id.clone()),
            ClineTaskIdentityOrigin::DirectoryNameDegraded,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
    }
}

fn merge_task_identity_authority(
    session: &mut ClineSessionRow,
    previous: Option<&ClineTaskCheckpoint>,
) {
    let Some(previous) = previous else {
        return;
    };
    let prior = &previous.task_metadata.session;
    let mut aliases = prior
        .identity_aliases
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if prior.identity == session.identity {
        session.identity_aliases = aliases.into_iter().collect::<Vec<_>>().into_boxed_slice();
        return;
    }
    if prior.identity_origin == ClineTaskIdentityOrigin::DirectoryNameDegraded
        && session.identity_origin == ClineTaskIdentityOrigin::TaskMetadata
    {
        aliases.insert(prior.identity.clone());
        aliases.remove(&session.identity);
        session.identity_aliases = aliases.into_iter().collect::<Vec<_>>().into_boxed_slice();
    }
}

fn file_source(
    dialect: super::source::TaskJsonNativeDialect,
    session: &ClineSessionRow,
    component: ClineComponent,
    path: &Path,
    released_ordinal_offset: u64,
) -> ClineFileSourceIdentity {
    ClineFileSourceIdentity {
        provider: dialect.provider.as_str(),
        task: session.identity.clone(),
        task_origin: session.identity_origin,
        task_aliases: session.identity_aliases.clone(),
        component,
        canonical_path: path.to_path_buf(),
        stable_id: format!(
            "{}:{}:{}",
            dialect.provider.as_str(),
            path.display(),
            component.file_name()
        )
        .into_boxed_str(),
        released_ordinal_offset,
    }
}

fn released_ordinal_offset(task: &ActiveTask, component: ClineEventComponent) -> u64 {
    let api = task
        .api_history
        .as_ref()
        .map_or(0, |checkpoint| checkpoint.observed_items);
    let ui = task
        .ui_messages
        .as_ref()
        .map_or(0, |checkpoint| checkpoint.observed_items);
    match component {
        ClineEventComponent::ApiHistory => 0,
        ClineEventComponent::UiMessages => api,
        ClineEventComponent::FallbackHistory => api.saturating_add(ui),
    }
}

fn certified_revision(
    observation: &ClineComponentObservation,
    revision_sha256: [u8; 32],
) -> ClineCertifiedRevision {
    let token = observation
        .stamp()
        .map_or_else(|| "missing".to_owned(), |stamp| stamp.token());
    ClineCertifiedRevision {
        revision_sha256,
        observed_stamp_token: token.into_boxed_str(),
    }
}

fn missing_revision(component: ClineComponent) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-cline-nativepath-missing-component-v1\0");
    hasher.update([component as u8]);
    hasher.finalize().into()
}

fn metadata_frontier(checkpoint: &ClineMetadataCheckpoint) -> ClinePageFrontier {
    ClinePageFrontier::zero_component(checkpoint.observation.component)
        .advance_metadata(&checkpoint.session.metadata_hash)
}

fn classify_transition(
    prior: Option<&ClineArrayCheckpoint>,
    current: &ClineArrayCheckpoint,
    prior_prefix_matches: bool,
) -> ClineComponentTransition {
    let Some(prior) = prior else {
        return if current.observed_items == 0 {
            ClineComponentTransition::LogicalEmpty
        } else {
            ClineComponentTransition::Cold
        };
    };
    if current.observed_items == prior.observed_items
        && current.final_frontier == prior.final_frontier
    {
        return ClineComponentTransition::ControlOnlyRewrite;
    }
    if current.observed_items > prior.observed_items && prior_prefix_matches {
        return ClineComponentTransition::Append {
            prior_items: usize::try_from(prior.observed_items).unwrap_or(usize::MAX),
        };
    }
    // A shorter bounded summary cannot prove that every retained item is an
    // unchanged prefix. Publish it conservatively as a rewrite.
    ClineComponentTransition::Rewrite
}

fn source_changed(observation: &ClineComponentObservation) -> ClineComponentFailure {
    ClineComponentFailure {
        component: observation.component,
        path: observation.path.clone(),
        kind: ClineComponentFailureKind::SourceChanged,
        message: "component changed before its page could be exposed".into(),
        retryable: true,
    }
}

fn deletion_metadata_authority_refusal(
    metadata: &ClineMetadataCheckpoint,
    array: &ClineComponentObservation,
) -> Option<ClineComponentFailure> {
    (metadata.observation.stamp().is_none()
        || metadata.session.identity_origin != ClineTaskIdentityOrigin::TaskMetadata)
        .then(|| ClineComponentFailure {
            component: array.component,
            path: metadata.observation.path.clone(),
            kind: ClineComponentFailureKind::SourceChanged,
            message: "Cline array deletion requires present metadata with a valid certified taskId"
                .into(),
            retryable: true,
        })
}

fn component_failure_outcome(failure: ClineComponentFailure) -> ClineComponentReadOutcome {
    ClineComponentReadOutcome {
        component: failure.component,
        path: failure.path.clone(),
        transition: None,
        pages: 0,
        failure: Some(failure),
    }
}

fn catalog_rejection(failure: ClineComponentFailure) -> ClineCatalogRejection {
    ClineCatalogRejection {
        path: failure.path,
        retryable: failure.retryable,
        message: failure.message,
    }
}

fn component_authority_failure(
    observation: &ClineComponentObservation,
    post_parse: bool,
) -> Result<Option<ClineComponentFailure>, ClineNativePathError> {
    let result = if post_parse {
        observation.post_parse_revalidate()
    } else {
        observation.revalidate()
    };
    match result {
        Ok(true) => Ok(None),
        Ok(false) | Err(ClineNativePathError::SourceChanged { .. }) => {
            Ok(Some(source_changed(observation)))
        }
        Err(ClineNativePathError::SourceAccess { .. }) => Ok(Some(source_changed(observation))),
        Err(error) if is_component_local_error(&error) => {
            Ok(Some(local_authority_failure(observation, &error)))
        }
        Err(error) => Err(error),
    }
}

fn directory_authority_failure(
    task: &ClineLiveTaskObservation,
    observation: &ClineComponentObservation,
) -> Result<Option<ClineComponentFailure>, ClineNativePathError> {
    match task.revalidate_directory() {
        Ok(true) => Ok(None),
        Ok(false) | Err(ClineNativePathError::SourceChanged { .. }) => {
            Ok(Some(source_changed(observation)))
        }
        Err(ClineNativePathError::SourceAccess { .. }) => Ok(Some(source_changed(observation))),
        Err(error) if is_component_local_error(&error) => {
            Ok(Some(local_authority_failure(observation, &error)))
        }
        Err(error) => Err(error),
    }
}

fn local_authority_failure(
    observation: &ClineComponentObservation,
    error: &ClineNativePathError,
) -> ClineComponentFailure {
    ClineComponentFailure {
        component: observation.component,
        path: observation.path.clone(),
        kind: ClineComponentFailureKind::LocalIo,
        message: error.to_string().into_boxed_str(),
        retryable: true,
    }
}

fn output_pressure_rejection(
    component: ClineComponent,
    output: &crate::ProOutputObservation,
) -> Option<ClineItemRejection> {
    let event_component = event_component(component)?;
    Some(ClineItemRejection {
        component: event_component,
        native_index: output.coordinate.native_sequence,
        native_id: None,
        kind: ClineItemRejectionKind::OversizedTransientOutput,
        observed_bytes: u64::try_from(output.content.len()).unwrap_or(u64::MAX),
        detail: "Cline transient output exceeded the independently bounded page lane".into(),
    })
}

fn event_component(component: ClineComponent) -> Option<ClineEventComponent> {
    match component {
        ClineComponent::ApiHistory => Some(ClineEventComponent::ApiHistory),
        ClineComponent::UiMessages => Some(ClineEventComponent::UiMessages),
        ClineComponent::FallbackHistory => Some(ClineEventComponent::FallbackHistory),
        ClineComponent::TaskMetadata
        | ClineComponent::HistoryItem
        | ClineComponent::TaskIndex
        | ClineComponent::RootIndex => None,
    }
}

fn estimated_page_envelope_bytes(
    source: &ClineFileSourceIdentity,
    revision: &ClineCertifiedRevision,
    expected: &ClinePageFrontier,
    next: &ClinePageFrontier,
    evidence: Option<&ClineTerminalEvidence>,
) -> usize {
    32_usize
        .saturating_add(estimated_source_bytes(source))
        .saturating_add(estimated_revision_bytes(revision))
        .saturating_add(estimated_frontier_bytes(expected))
        .saturating_add(estimated_frontier_bytes(next))
        .saturating_add(1)
        .saturating_add(estimated_terminal_evidence_bytes(evidence))
        .saturating_add(6 * 8)
        .saturating_add(1)
        .saturating_add(estimated_transition_bytes())
        .saturating_add(1)
        .saturating_add(8)
        .saturating_add(8)
        .saturating_add(1)
        .saturating_add(1)
        .saturating_add(1)
}

pub(super) fn owned_page_bounds_are_valid(
    core_bytes: usize,
    transient_bytes: usize,
    logical_units: usize,
) -> bool {
    logical_units <= super::normalize::CLINE_NATIVE_PAGE_MAX_UNITS
        && core_bytes <= CLINE_NATIVE_CORE_PAGE_MAX_BYTES
        && core_bytes.saturating_add(transient_bytes) <= CLINE_NATIVE_PAGE_MAX_BYTES
}

fn estimated_transition_bytes() -> usize {
    // The largest legal transition encoding is its tag plus `prior_items`.
    1 + 8
}

fn estimated_terminal_evidence_bytes(evidence: Option<&ClineTerminalEvidence>) -> usize {
    1_usize.saturating_add(evidence.map_or(0, |evidence| match evidence {
        ClineTerminalEvidence::CompleteArray { .. } => 1 + 8 + 8 + 32,
        ClineTerminalEvidence::CompleteMetadata { content_sha256 } => {
            1 + 1 + usize::from(content_sha256.is_some()) * 32
        }
        ClineTerminalEvidence::Deleted => 1,
        ClineTerminalEvidence::ControlOnly { .. } => 1 + 32,
    }))
}
