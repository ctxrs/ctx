//! Thin Cline/Roo adapter for the shared replacement-document lifecycle.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, BatchHydrationRequest, BatchHydrationResult,
    CaptureProvider, ContentSourceResolver, EventHydrationRequest, EventIdentityInput,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionHydrationRequest, SessionIdentityInput, SourceAnchor, SourceKey,
    SourceObservation, SourceRecordLocator, SourceResolverContractError, StableEntityId,
    SubrecordSelector, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    provider::source_backed::{
        family::document::{
            ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint,
            DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
        },
        SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
    },
    provider_sources::ProviderSource,
};

use super::{
    normalize::{
        ClineArrayCheckpoint, ClineCertifiedPage, ClineEventKind, ClineEventRole, ClineEventRow,
        ClineNativeItemKey, ClineSessionRow, ClineSourceRecordEvidence, ClineTaskCheckpoint,
    },
    source::{
        ClineComponent, ClineDiscovery, ClineLiveTaskObservation, ClineObservedFileState,
        TaskJsonNativeDialect,
    },
    ClineNativePathError, ClineNativeReader,
};

mod support;

use support::*;

const SOURCE_ANCHOR_NAMESPACE: &str = "task-directory-id";
const SOURCE_SCHEMA_VARIANT: &str = "task-directory-v1";
const SOURCE_REVISION_KIND: &str = "task-directory-compound-v1";
const PARSER_REVISION: &str = "task-json-source-backed-v1";
const LOGICAL_SESSION_KIND: &str = "task-json-thread";
const LOGICAL_EVENT_KIND: &str = "task-json-event";
const NATIVE_SESSION_NAMESPACE: &str = "task-json-task-id";
const NATIVE_ITEM_NAMESPACE: &str = "task-json-native-item";
const NATIVE_ITEM_POSITION_KIND: &str = "task-json-component-ordinal";
const SUBRECORD_POSITION_KIND: &str = "task-json-subrecord";
const MAX_SOURCE_BACKED_PAGE_DOCUMENTS: usize = 64;
const MAX_SOURCE_BACKED_PAGE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Error)]
pub(crate) enum TaskJsonSourceBackedError {
    #[error(transparent)]
    Native(#[from] ClineNativePathError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Locator(#[from] SourceResolverContractError),
    #[error("{provider} selected no authoritative task root")]
    MissingRoot { provider: &'static str },
    #[error("{provider} selected more than one authoritative task root")]
    DuplicateRoot { provider: &'static str },
    #[error("{provider} selected duplicate task lineage {task_id:?}")]
    DuplicateTask {
        provider: &'static str,
        task_id: String,
    },
    #[error("{provider} source-backed reader emitted a page outside its selected task")]
    UnownedPage { provider: &'static str },
    #[error("{provider} source-backed reader emitted a native item without record evidence")]
    MissingRecordEvidence { provider: &'static str },
    #[error("{provider} task {path} did not complete under unchanged authority")]
    IncompleteTask {
        provider: &'static str,
        path: PathBuf,
    },
    #[error("{provider} task {path} changed before source certification")]
    TaskChanged {
        provider: &'static str,
        path: PathBuf,
    },
    #[error("{provider} task {path} has no terminal parser checkpoint")]
    MissingCheckpoint {
        provider: &'static str,
        path: PathBuf,
    },
    #[error("{provider} source-backed counters overflowed")]
    CountOverflow { provider: &'static str },
    #[error("{provider} source-backed page exceeded its fixed document/byte bound")]
    PageBound { provider: &'static str },
    #[error("{provider} native event sequence exceeds the supported coordinate bound")]
    EventSequenceBound { provider: &'static str },
}

pub(crate) type TaskJsonSourceBackedResult<T> = Result<T, TaskJsonSourceBackedError>;

#[derive(Clone)]
pub(crate) struct TaskJsonSourceBackedResolver {
    dialect: TaskJsonNativeDialect,
    selected: Box<[ProviderSource]>,
    #[cfg(test)]
    hydration_scans: Option<Arc<AtomicUsize>>,
}

pub(crate) struct TaskJsonDocumentTreeAdapter {
    dialect: TaskJsonNativeDialect,
    selected: Box<[ProviderSource]>,
    resolver: TaskJsonSourceBackedResolver,
    #[cfg(test)]
    projection_scans: Option<Arc<AtomicUsize>>,
    #[cfg(test)]
    terminal_revalidation_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

pub(crate) fn cline_task_json_source_backed_adapter(
    selected: &[ProviderSource],
) -> TaskJsonDocumentTreeAdapter {
    TaskJsonDocumentTreeAdapter::new(TaskJsonNativeDialect::CLINE, selected)
}

pub(crate) fn roo_task_json_source_backed_adapter(
    selected: &[ProviderSource],
) -> TaskJsonDocumentTreeAdapter {
    TaskJsonDocumentTreeAdapter::new(TaskJsonNativeDialect::ROO, selected)
}

pub(crate) fn cline_task_json_source_backed_resolver(
    selected: &[ProviderSource],
) -> TaskJsonSourceBackedResult<TaskJsonSourceBackedResolver> {
    Ok(TaskJsonSourceBackedResolver::new(
        TaskJsonNativeDialect::CLINE,
        selected,
    ))
}

pub(crate) fn roo_task_json_source_backed_resolver(
    selected: &[ProviderSource],
) -> TaskJsonSourceBackedResult<TaskJsonSourceBackedResolver> {
    Ok(TaskJsonSourceBackedResolver::new(
        TaskJsonNativeDialect::ROO,
        selected,
    ))
}

impl TaskJsonDocumentTreeAdapter {
    fn new(dialect: TaskJsonNativeDialect, selected: &[ProviderSource]) -> Self {
        Self {
            dialect,
            selected: selected.to_vec().into_boxed_slice(),
            resolver: TaskJsonSourceBackedResolver::new(dialect, selected),
            #[cfg(test)]
            projection_scans: None,
            #[cfg(test)]
            terminal_revalidation_hook: None,
        }
    }

    pub(crate) fn with_resolver(mut self, resolver: TaskJsonSourceBackedResolver) -> Self {
        self.resolver = resolver;
        self
    }

    #[cfg(test)]
    pub(super) fn with_projection_scans(mut self, scans: Arc<AtomicUsize>) -> Self {
        self.projection_scans = Some(scans);
        self
    }

    #[cfg(test)]
    pub(super) fn with_terminal_revalidation_hook(
        mut self,
        hook: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        self.terminal_revalidation_hook = Some(hook);
        self
    }
}

impl ReplacementDocumentTree for TaskJsonDocumentTreeAdapter {
    type Leaf = ClineLiveTaskObservation;
    type TreeAuthority = ClineDiscovery;

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        owns_task_source(self.dialect, source)
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let root = selected_root(self.dialect, &self.selected)?;
        discover_document_tree(self.dialect, &root).map_err(task_route_error)
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let current = authority
            .task_routes()
            .iter()
            .find(|task| task.canonical_task_path == leaf.canonical_task_path)
            .ok_or_else(|| {
                source_changed("task leaf disappeared from its retained root authority")
            })?;
        if current != leaf {
            return Err(source_changed(
                "task leaf changed between cheap discovery and projection",
            ));
        }
        #[cfg(test)]
        if let Some(scans) = self.projection_scans.as_ref() {
            scans.fetch_add(1, Ordering::Relaxed);
        }
        let source = task_source_key(self.dialect, leaf).map_err(task_route_error)?;
        sink.begin_source(source.clone())?;
        scan_task(self.dialect, authority, leaf, |document| {
            sink.emit_document(document)
        })
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        #[cfg(test)]
        if let Some(hook) = self.terminal_revalidation_hook.as_ref() {
            hook();
        }
        revalidate_document_tree(self.dialect, tree)
    }

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        self.resolver.hydrate_group(request)
    }
}

impl TaskJsonSourceBackedResolver {
    fn new(dialect: TaskJsonNativeDialect, selected: &[ProviderSource]) -> Self {
        Self {
            dialect,
            selected: selected.to_vec().into_boxed_slice(),
            #[cfg(test)]
            hydration_scans: None,
        }
    }

    #[cfg(test)]
    pub(super) fn with_hydration_scans(mut self, scans: Arc<AtomicUsize>) -> Self {
        self.hydration_scans = Some(scans);
        self
    }

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let first = request.events().first().ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "task hydration group is empty",
            )
        })?;
        let root = selected_root(self.dialect, &self.selected).map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })?;
        let discovery = discover_document_tree(self.dialect, &root)
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?
            .authority;
        let mut matched = None;
        for task in discovery.task_routes() {
            let source = task_source_key(self.dialect, task)
                .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
            if source.exact_descriptor_eq(first.locator().source())
                && matched.replace((task, source)).is_some()
            {
                return Err(hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    "task source identity matched more than one retained leaf",
                ));
            }
        }
        let (task, source) = matched.ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::ConfirmedDeleted,
                "task source is absent from the selected authoritative root",
            )
        })?;
        let observation = task_observation(&source, task)
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
        let revision_digest = digest_revision(&observation);
        let mut positions = HashMap::with_capacity(request.len());
        for (position, event) in request.events().iter().enumerate() {
            event
                .validate_contract()
                .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
            source
                .validate_exact_descriptor(event.locator().source())
                .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
            if event.locator().revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
                || event.locator().certified_source_revision_digest() != Some(&revision_digest)
            {
                return Err(hydration_failure(
                    HydrationFailureKind::StaleSourceEvidence,
                    "task source revision no longer matches the exact locator",
                ));
            }
            positions.insert(event.event_id(), position);
        }

        let mut records = vec![None; request.len()];
        #[cfg(test)]
        if let Some(scans) = self.hydration_scans.as_ref() {
            scans.fetch_add(1, Ordering::Relaxed);
        }
        scan_task(self.dialect, &discovery, task, |document| {
            let Some(position) = positions.get(&document.event_id).copied() else {
                return Ok(());
            };
            let expected = &request.events()[position];
            if expected.locator() == &document.locator {
                records[position] = Some(HydratedProviderRecord {
                    event_id: document.event_id,
                    provider_bytes: document.body.into_bytes(),
                });
            }
            Ok(())
        })
        .map_err(|error| hydration_failure(HydrationFailureKind::StaleSourceEvidence, error))?;

        let records = records
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::MissingRecord,
                    "task locator no longer projects a retained lexical event",
                )
            })?;
        BatchHydrationResult::new(records)
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))
    }
}

impl ContentSourceResolver for TaskJsonSourceBackedResolver {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let batch = BatchHydrationRequest::new(vec![request.clone()])
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
        self.hydrate_group(&batch)?
            .into_records()
            .into_iter()
            .next()
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::MissingRecord,
                    "task event disappeared during exact hydration",
                )
            })
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_group(request.batch())
            .map(BatchHydrationResult::into_records)
    }
}

fn selected_root(
    dialect: TaskJsonNativeDialect,
    selected: &[ProviderSource],
) -> SourceBackedRouteResult<PathBuf> {
    let selection = select_authoritative_roots(dialect, selected);
    if !selection.detected_but_unsupported.is_empty() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unsupported,
            "the selected task directory is a detected but unsupported format",
        ));
    }
    if !selection.unavailable.is_empty() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "the selected task directory is unavailable",
        ));
    }
    match selection.roots.as_slice() {
        [root] => Ok(root.clone()),
        [] => Err(task_route_error(TaskJsonSourceBackedError::MissingRoot {
            provider: dialect.display_name,
        })),
        _ => Err(task_route_error(TaskJsonSourceBackedError::DuplicateRoot {
            provider: dialect.display_name,
        })),
    }
}

fn discover_document_tree(
    dialect: TaskJsonNativeDialect,
    root: &Path,
) -> TaskJsonSourceBackedResult<CompleteDocumentTree<ClineLiveTaskObservation, ClineDiscovery>> {
    let discovery = discover_root(dialect, root)?;
    let mut sources = BTreeMap::new();
    let mut leaves = Vec::with_capacity(discovery.task_routes().len());
    for task in discovery.task_routes() {
        let source = task_source_key(dialect, task)?;
        if sources
            .insert(source.identity().digest(), task.directory_task_id.clone())
            .is_some()
        {
            return Err(TaskJsonSourceBackedError::DuplicateTask {
                provider: dialect.display_name,
                task_id: task.directory_task_id.to_string(),
            });
        }
        leaves.push(observed_task_leaf(dialect, task)?);
    }
    let tree_fingerprint = task_tree_fingerprint(dialect, &discovery, &leaves);
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        leaves,
        discovery,
    ))
}

fn observed_task_leaf(
    dialect: TaskJsonNativeDialect,
    task: &ClineLiveTaskObservation,
) -> TaskJsonSourceBackedResult<ObservedDocumentLeaf<ClineLiveTaskObservation>> {
    let source = task_source_key(dialect, task)?;
    let observation = task_observation(&source, task)?;
    let mut digest = Sha256::new();
    digest.update(b"ctx-task-json-document-leaf-v1\0");
    digest.update(source.exact_descriptor_digest());
    digest.update(digest_revision(&observation));
    let path = task.canonical_task_path.as_os_str().as_encoded_bytes();
    digest.update((path.len() as u64).to_be_bytes());
    digest.update(path);
    Ok(ObservedDocumentLeaf::new(
        DocumentLeafFingerprint::new(digest.finalize().into()),
        task.clone(),
    ))
}

fn task_tree_fingerprint(
    dialect: TaskJsonNativeDialect,
    discovery: &ClineDiscovery,
    leaves: &[ObservedDocumentLeaf<ClineLiveTaskObservation>],
) -> [u8; 32] {
    let mut fingerprints = leaves
        .iter()
        .map(|leaf| leaf.fingerprint.as_bytes())
        .collect::<Vec<_>>();
    fingerprints.sort_unstable();
    let mut digest = Sha256::new();
    digest.update(b"ctx-task-json-document-tree-v1\0");
    digest.update(dialect.provider.as_str().as_bytes());
    digest.update(discovery.root_authority().source_backed_revision());
    digest.update((fingerprints.len() as u64).to_be_bytes());
    for fingerprint in fingerprints {
        digest.update(fingerprint);
    }
    digest.finalize().into()
}

fn revalidate_document_tree(
    dialect: TaskJsonNativeDialect,
    tree: &CompleteDocumentTree<ClineLiveTaskObservation, ClineDiscovery>,
) -> SourceBackedRouteResult<[u8; 32]> {
    if !tree
        .authority
        .root_authority()
        .revalidate_catalog()
        .map_err(|error| task_route_error(error.into()))?
    {
        return Err(source_changed(
            "task document tree changed before terminal revalidation",
        ));
    }
    for task in tree.authority.task_routes() {
        if !task
            .revalidate_all_components()
            .map_err(|error| task_route_error(error.into()))?
        {
            return Err(source_changed(
                "task document tree changed before terminal revalidation",
            ));
        }
    }
    let current = task_tree_fingerprint(dialect, &tree.authority, &tree.leaves);
    if current != tree.tree_fingerprint {
        return Err(source_changed(
            "task document tree fingerprint changed before certification",
        ));
    }
    Ok(current)
}

struct TaskAccumulator {
    opening: ClineLiveTaskObservation,
    source: SourceKey,
    observation: SourceObservation,
    revision_digest: [u8; 32],
    content_digest: Sha256,
    counts: ScannedSourceCounts,
    session: Option<ClineSessionRow>,
}

fn scan_task(
    dialect: TaskJsonNativeDialect,
    authority: &ClineDiscovery,
    task: &ClineLiveTaskObservation,
    mut emit: impl FnMut(LexicalDocument) -> SourceBackedRouteResult<()>,
) -> SourceBackedRouteResult<DocumentSourceTerminal> {
    let source = task_source_key(dialect, task).map_err(task_route_error)?;
    let observation = task_observation(&source, task).map_err(task_route_error)?;
    let revision_digest = digest_revision(&observation);
    let mut content_digest = Sha256::new();
    content_digest.update(b"ctx-task-json-source-content-v1\0");
    content_digest.update(source.identity().digest());
    content_digest.update(revision_digest);
    let mut accumulator = TaskAccumulator {
        opening: task.clone(),
        source: source.clone(),
        observation,
        revision_digest,
        content_digest,
        counts: ScannedSourceCounts::default(),
        session: None,
    };
    let mut reader = ClineNativeReader::new(authority.for_task(task.clone()));
    while let Some(page) = reader
        .next_page()
        .map_err(|error| task_route_error(error.into()))?
    {
        for document in
            project_native_page(dialect, &mut accumulator, page).map_err(task_route_error)?
        {
            emit(document)?;
        }
    }
    let completion = reader
        .finish_task()
        .map_err(|error| task_route_error(error.into()))?;
    if completion
        .component_outcomes
        .iter()
        .any(|outcome| outcome.failure.is_some())
    {
        return Err(task_route_error(
            TaskJsonSourceBackedError::IncompleteTask {
                provider: dialect.display_name,
                path: task.canonical_task_path.clone(),
            },
        ));
    }
    if !accumulator
        .opening
        .revalidate_all_components()
        .map_err(|error| task_route_error(error.into()))?
    {
        return Err(task_route_error(TaskJsonSourceBackedError::TaskChanged {
            provider: dialect.display_name,
            path: task.canonical_task_path.clone(),
        }));
    }
    let checkpoint = completion
        .live_checkpoints
        .iter()
        .find(|checkpoint| checkpoint.canonical_task_path == task.canonical_task_path)
        .ok_or_else(|| {
            task_route_error(TaskJsonSourceBackedError::MissingCheckpoint {
                provider: dialect.display_name,
                path: task.canonical_task_path.clone(),
            })
        })?;
    task_terminal(dialect, accumulator, checkpoint).map_err(task_route_error)
}

fn project_native_page(
    dialect: TaskJsonNativeDialect,
    task: &mut TaskAccumulator,
    page: ClineCertifiedPage,
) -> TaskJsonSourceBackedResult<Vec<LexicalDocument>> {
    if !task_owns_component(&task.opening, &page.source.canonical_path) {
        return Err(TaskJsonSourceBackedError::UnownedPage {
            provider: dialect.display_name,
        });
    }
    if let Some(session) = page.core.session.as_ref() {
        task.session = Some(session.clone());
    }
    let retained = u64::try_from(page.core.events.len()).map_err(|_| count_overflow(dialect))?;
    let rejected =
        u64::try_from(page.core.rejections.len()).map_err(|_| count_overflow(dialect))?;
    if let Some(evidence) = page.source_record {
        hash_record_evidence(&mut task.content_digest, page.source.component, evidence);
        task.counts.complete_records = checked_add(
            dialect,
            task.counts.complete_records,
            if retained == 0 && rejected == 0 {
                1
            } else {
                checked_add(dialect, retained, rejected)?
            },
        )?;
        task.counts.retained_records =
            checked_add(dialect, task.counts.retained_records, retained)?;
        task.counts.rejected_records =
            checked_add(dialect, task.counts.rejected_records, rejected)?;
        task.counts.indexed_documents =
            checked_add(dialect, task.counts.indexed_documents, retained)?;
        if retained == 0 && rejected == 0 {
            task.counts.ignored_records = checked_add(dialect, task.counts.ignored_records, 1)?;
        }
    } else if retained != 0 || rejected != 0 {
        return Err(TaskJsonSourceBackedError::MissingRecordEvidence {
            provider: dialect.display_name,
        });
    }
    if page.core.events.is_empty() {
        return Ok(Vec::new());
    }
    let session = task
        .session
        .as_ref()
        .map(|session| session.identity.as_str())
        .or_else(|| {
            page.core
                .events
                .first()
                .map(|event| event.identity.task.as_str())
        })
        .ok_or(TaskJsonSourceBackedError::UnownedPage {
            provider: dialect.display_name,
        })?
        .to_owned();
    let session_id = derive_task_session_id(&task.source, &session)?;
    let workspace = task
        .session
        .as_ref()
        .and_then(|session| session.workspace_directory.as_deref())
        .map(str::to_owned);
    let documents = page
        .core
        .events
        .into_vec()
        .into_iter()
        .map(|event| {
            project_event(
                dialect,
                &task.source,
                task.revision_digest,
                session_id,
                &session,
                workspace.as_deref(),
                event,
            )
        })
        .collect::<TaskJsonSourceBackedResult<Vec<_>>>()?;
    if documents.len() > MAX_SOURCE_BACKED_PAGE_DOCUMENTS
        || estimated_documents_bytes(&documents) > MAX_SOURCE_BACKED_PAGE_BYTES
    {
        return Err(TaskJsonSourceBackedError::PageBound {
            provider: dialect.display_name,
        });
    }
    Ok(documents)
}

fn task_owns_component(task: &ClineLiveTaskObservation, path: &Path) -> bool {
    [
        ClineComponent::ApiHistory,
        ClineComponent::UiMessages,
        ClineComponent::FallbackHistory,
        ClineComponent::TaskMetadata,
        ClineComponent::HistoryItem,
        ClineComponent::TaskIndex,
    ]
    .into_iter()
    .any(|component| task.component(component).path == path)
}

fn estimated_documents_bytes(documents: &[LexicalDocument]) -> usize {
    documents.iter().fold(0_usize, |total, document| {
        total
            .saturating_add(document.body.len())
            .saturating_add(document.event_type.len())
            .saturating_add(document.role.as_ref().map_or(0, String::len))
            .saturating_add(document.provider_session_id.as_ref().map_or(0, String::len))
            .saturating_add(document.workspace.as_ref().map_or(0, String::len))
            .saturating_add(document.cwd.as_ref().map_or(0, String::len))
            .saturating_add(
                document
                    .touched_files
                    .iter()
                    .map(String::len)
                    .sum::<usize>(),
            )
    })
}

#[allow(clippy::too_many_arguments)]
fn project_event(
    dialect: TaskJsonNativeDialect,
    source: &SourceKey,
    revision_digest: [u8; 32],
    session_id: StableEntityId,
    provider_session_id: &str,
    workspace: Option<&str>,
    event: ClineEventRow,
) -> TaskJsonSourceBackedResult<LexicalDocument> {
    let evidence = event
        .source_record
        .ok_or(TaskJsonSourceBackedError::MissingRecordEvidence {
            provider: dialect.display_name,
        })?;
    let native_item_key = native_item_key(&event, revision_digest)?;
    let subrecord = (event.identity.sub_index != 0)
        .then(|| {
            SubrecordSelector::revision_scoped_position(
                SUBRECORD_POSITION_KIND,
                TypedKey::U64(u64::from(event.identity.sub_index)),
                TypedKey::bytes(evidence.record_digest.to_vec())?,
            )
        })
        .transpose()?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: subrecord.as_ref(),
    })?;
    let relative_file = event.native_order.component.source_component().file_name();
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::utf8(relative_file)?,
            record_coordinate: TypedKey::composite(vec![
                TypedKey::U64(evidence.native_index),
                typed_native_item_key(&event.identity.item)?,
                TypedKey::U64(u64::from(event.identity.sub_index)),
            ])?,
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(revision_digest),
        evidence.record_digest,
    )?;
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(provider_session_id.to_owned()),
        branch: None,
        source_path: Some(relative_file.to_owned()),
        agent_type: match dialect.provider {
            CaptureProvider::Cline => "cline",
            CaptureProvider::RooCode => "roo-code",
            _ => unreachable!(),
        }
        .to_owned(),
        is_primary: true,
        event_sequence: event_sequence(dialect, &event)?,
        occurred_at_unix_ms: event.occurred_at_millis,
        event_type: event_kind(event.kind).to_owned(),
        role: Some(event_role(event.role).to_owned()),
        body: lexical_event_body(&event),
        workspace: workspace.map(str::to_owned),
        cwd: workspace.map(str::to_owned),
        touched_files: event
            .file_touches
            .iter()
            .map(|touch| touch.path.to_string())
            .collect(),
    })
}

fn task_terminal(
    dialect: TaskJsonNativeDialect,
    mut task: TaskAccumulator,
    checkpoint: &ClineTaskCheckpoint,
) -> TaskJsonSourceBackedResult<DocumentSourceTerminal> {
    hash_metadata_checkpoint(&mut task.content_digest, checkpoint);
    let mut certified_bytes = checkpoint
        .task_metadata
        .observation
        .stamp()
        .map_or(0, |stamp| stamp.len());
    for array in [
        checkpoint.api_history.as_ref(),
        checkpoint.ui_messages.as_ref(),
        checkpoint.fallback_history.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        hash_array_checkpoint(&mut task.content_digest, array);
        certified_bytes = checked_add(dialect, certified_bytes, array.complete_bytes)?;
    }
    task.counts.certified_bytes = certified_bytes;
    Ok(DocumentSourceTerminal {
        source: task.source,
        opening: task.observation.clone(),
        closing: task.observation,
        parser_revision: PARSER_REVISION,
        content_digest: task.content_digest.finalize().into(),
        counts: task.counts,
    })
}

fn owns_task_source(dialect: TaskJsonNativeDialect, source: &SourceKey) -> bool {
    source.provider() == dialect.provider.as_str()
        && source.source_format() == dialect.source_format
        && source.schema_variant() == SOURCE_SCHEMA_VARIANT
        && source.provider_identity_version() == 1
}

fn source_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn task_route_error(error: TaskJsonSourceBackedError) -> SourceBackedRouteError {
    let kind = match &error {
        TaskJsonSourceBackedError::Native(ClineNativePathError::SourceChanged { .. })
        | TaskJsonSourceBackedError::TaskChanged { .. } => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        TaskJsonSourceBackedError::Native(ClineNativePathError::SourceIo {
            kind: std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied,
            ..
        }) => SourceBackedRouteErrorKind::Unavailable,
        TaskJsonSourceBackedError::MissingRoot { .. } => SourceBackedRouteErrorKind::Unavailable,
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}
