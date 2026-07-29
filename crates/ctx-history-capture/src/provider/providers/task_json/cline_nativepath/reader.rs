use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use super::{
    normalize::{
        estimated_rejection_bytes, estimated_session_bytes, estimated_source_bytes,
        ClineArrayCheckpoint, ClineCatalogCompletion, ClineCatalogIndex, ClineCatalogRejection,
        ClineCertifiedPage, ClineComponentFailure, ClineComponentFailureKind,
        ClineComponentReadOutcome, ClineComponentTransition, ClineCorePayload, ClineEventComponent,
        ClineEventKind, ClineFileSourceIdentity, ClineMetadataCheckpoint, ClinePageFrontier,
        ClinePublicationStats, ClineSessionRow, ClineTaskCheckpoint, ClineTaskIdentity,
        ClineTaskIdentityOrigin, CLINE_NATIVE_CORE_PAGE_MAX_BYTES, CLINE_NATIVE_FIXED_PAGE_UNITS,
        CLINE_NATIVE_SESSION_PAGE_UNITS,
    },
    parse::{
        hydrate_component, parse_metadata, parse_root_index, parse_scanned_item,
        pin_component_content, ClineArrayScanStep, ClineArrayScanner, ClineLocalReadError,
        ClinePinnedContentAuthority, ParsedItem,
    },
    source::{
        is_component_local_error, ClineComponent, ClineComponentObservation, ClineDiscovery,
        ClineLiveTaskObservation, ClineObservedFileState,
    },
    ClineNativePathError,
};

pub(crate) struct ClineNativeReader {
    discovery: ClineDiscovery,
    previous_by_path: BTreeMap<PathBuf, ClineTaskCheckpoint>,
    route_index: usize,
    pending_page: Option<ClineCertifiedPage>,
    active_task: Option<ActiveTask>,
    active_array: Option<ActiveArray>,
    outcomes: Vec<ClineComponentReadOutcome>,
    live_checkpoints: Vec<ClineTaskCheckpoint>,
    stats: ClinePublicationStats,
    catalog_finished: bool,
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
    revision_sha256: [u8; 32],
    frontier: ClinePageFrontier,
    observed_items: u64,
    retained_rows: u64,
    native_id_occurrences: BTreeMap<String, u64>,
    prior_prefix_matches: bool,
    attach_session: bool,
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

fn file_source(component: ClineComponent, path: &Path) -> ClineFileSourceIdentity {
    ClineFileSourceIdentity {
        component,
        canonical_path: path.to_path_buf(),
    }
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

pub(super) fn owned_page_bounds_are_valid(core_bytes: usize, logical_units: usize) -> bool {
    logical_units <= super::normalize::CLINE_NATIVE_PAGE_MAX_UNITS
        && core_bytes <= CLINE_NATIVE_CORE_PAGE_MAX_BYTES
}
