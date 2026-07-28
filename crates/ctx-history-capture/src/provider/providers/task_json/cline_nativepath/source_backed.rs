use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceInventory,
    ContentSourceResolver, EventHydrationRequest, EventIdentityInput, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ProjectionContractError, ScannedSourceCounts,
    SessionHydrationRequest, SessionIdentityInput, SourceAnchor, SourceInventoryObservation,
    SourceKey, SourceObservation, SourceRecordLocator, SourceResolverContractError, StableEntityId,
    SubrecordSelector, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::provider_sources::{ProviderSource, ProviderSourceKind, ProviderSourceStatus};

use super::{
    discover_cline_root, discover_roo_root,
    normalize::{
        ClineArrayCheckpoint, ClineCertifiedPage, ClineEventComponent, ClineEventKind,
        ClineEventRole, ClineEventRow, ClineNativeItemKey, ClinePublicationStats, ClineSessionRow,
        ClineSourceRecordEvidence, ClineTaskCheckpoint,
    },
    parse::{hydrate_component, ClineArrayScanStep, ClineArrayScanner, ClineLocalReadError},
    source::{
        ClineComponent, ClineComponentObservation, ClineDiscovery, ClineLiveTaskObservation,
        ClineObservedFileState, TaskJsonNativeDialect,
    },
    ClineNativePathError, ClineNativeProfile, ClineNativeReader,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "task-directory-id";
const SOURCE_SCHEMA_VARIANT: &str = "task-directory-v1";
const SOURCE_REVISION_KIND: &str = "task-directory-compound-v1";
const INVENTORY_AUTHORITY_NAMESPACE: &str = "task-json-tasks-root";
const INVENTORY_REVISION_KIND: &str = "task-json-root-inventory-v1";
const DISCOVERY_REVISION: &str = "task-json-source-discovery-v1";
const PARSER_REVISION: &str = "task-json-source-backed-v1";
const LOGICAL_SESSION_KIND: &str = "task-json-thread";
const LOGICAL_EVENT_KIND: &str = "task-json-event";
const NATIVE_SESSION_NAMESPACE: &str = "task-json-task-id";
const NATIVE_ITEM_NAMESPACE: &str = "task-json-native-item";
const NATIVE_ITEM_POSITION_KIND: &str = "task-json-component-ordinal";
const SUBRECORD_POSITION_KIND: &str = "task-json-subrecord";
const MAX_LEXICAL_PREVIEW_CHARS: usize = 2_048;
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
    #[error("{provider} selected the same authoritative task root more than once: {path}")]
    DuplicateRoot {
        provider: &'static str,
        path: PathBuf,
    },
    #[error("{provider} selected duplicate task lineage {task_id:?}")]
    DuplicateTask {
        provider: &'static str,
        task_id: String,
    },
    #[error("{provider} source-backed reader emitted a page outside its selected task roots")]
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

#[derive(Debug)]
pub(crate) struct TaskJsonSourceBackedPage {
    pub(crate) source: SourceKey,
    pub(crate) documents: Box<[LexicalDocument]>,
    pub(crate) estimated_owned_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskJsonSourceBackedSession {
    pub(crate) session_id: StableEntityId,
    pub(crate) provider_session_id: String,
    pub(crate) title: Option<String>,
    pub(crate) workspace: Option<String>,
    pub(crate) locator: Option<SourceRecordLocator>,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskJsonCertifiedTask {
    pub(crate) source: SourceKey,
    pub(crate) certified_source: CertifiedSource,
    pub(crate) session: TaskJsonSourceBackedSession,
}

#[derive(Debug)]
pub(crate) struct TaskJsonSourceBackedCompletion {
    pub(crate) inventories: Box<[CertifiedSourceInventory]>,
    pub(crate) tasks: Box<[TaskJsonCertifiedTask]>,
    pub(crate) detected_but_unsupported: Box<[ProviderSource]>,
    pub(crate) unavailable: Box<[ProviderSource]>,
}

pub(crate) struct TaskJsonSourceBackedAdapter {
    dialect: TaskJsonNativeDialect,
    pending_roots: VecDeque<PathBuf>,
    active: Option<ActiveRoot>,
    seen_roots: BTreeSet<PathBuf>,
    seen_sources: BTreeSet<[u8; 32]>,
    inventories: Vec<CertifiedSourceInventory>,
    tasks: Vec<TaskJsonCertifiedTask>,
    detected_but_unsupported: Box<[ProviderSource]>,
    unavailable: Box<[ProviderSource]>,
    drained: bool,
}

struct ActiveRoot {
    reader: ClineNativeReader,
    opening_inventory: SourceInventoryObservation,
    inventory_sources: Vec<SourceKey>,
    component_owners: BTreeMap<PathBuf, PathBuf>,
    tasks: BTreeMap<PathBuf, TaskAccumulator>,
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

#[derive(Debug, Clone)]
struct ResolverTask {
    task: ClineLiveTaskObservation,
    source: SourceKey,
    revision_digest: [u8; 32],
}

pub(crate) struct TaskJsonSourceBackedResolver {
    dialect: TaskJsonNativeDialect,
    tasks: BTreeMap<[u8; 32], ResolverTask>,
}

pub(crate) fn cline_task_json_source_backed_adapter(
    selected: &[ProviderSource],
) -> TaskJsonSourceBackedAdapter {
    TaskJsonSourceBackedAdapter::new(TaskJsonNativeDialect::CLINE, selected)
}

pub(crate) fn roo_task_json_source_backed_adapter(
    selected: &[ProviderSource],
) -> TaskJsonSourceBackedAdapter {
    TaskJsonSourceBackedAdapter::new(TaskJsonNativeDialect::ROO, selected)
}

pub(crate) fn cline_task_json_source_backed_resolver(
    selected: &[ProviderSource],
) -> TaskJsonSourceBackedResult<TaskJsonSourceBackedResolver> {
    TaskJsonSourceBackedResolver::new(TaskJsonNativeDialect::CLINE, selected)
}

pub(crate) fn roo_task_json_source_backed_resolver(
    selected: &[ProviderSource],
) -> TaskJsonSourceBackedResult<TaskJsonSourceBackedResolver> {
    TaskJsonSourceBackedResolver::new(TaskJsonNativeDialect::ROO, selected)
}

impl TaskJsonSourceBackedAdapter {
    fn new(dialect: TaskJsonNativeDialect, selected: &[ProviderSource]) -> Self {
        let selection = select_authoritative_roots(dialect, selected);
        Self {
            dialect,
            pending_roots: selection.roots.into(),
            active: None,
            seen_roots: BTreeSet::new(),
            seen_sources: BTreeSet::new(),
            inventories: Vec::new(),
            tasks: Vec::new(),
            detected_but_unsupported: selection.detected_but_unsupported.into_boxed_slice(),
            unavailable: selection.unavailable.into_boxed_slice(),
            drained: false,
        }
    }

    pub(crate) fn detected_but_unsupported(&self) -> &[ProviderSource] {
        &self.detected_but_unsupported
    }

    pub(crate) fn unavailable(&self) -> &[ProviderSource] {
        &self.unavailable
    }

    pub(crate) fn next_page(
        &mut self,
    ) -> TaskJsonSourceBackedResult<Option<TaskJsonSourceBackedPage>> {
        loop {
            if self.active.is_none() && !self.start_next_root()? {
                self.drained = true;
                return Ok(None);
            }
            let native_page = {
                let active = self
                    .active
                    .as_mut()
                    .expect("active root was just established");
                active.reader.next_page()?
            };
            if let Some(native_page) = native_page {
                if let Some(page) =
                    project_native_page(self.dialect, self.active.as_mut().unwrap(), native_page)?
                {
                    return Ok(Some(page));
                }
                continue;
            }
            let active = self.active.take().expect("drained root must be active");
            self.finish_root(active)?;
        }
    }

    pub(crate) fn finish(self) -> TaskJsonSourceBackedResult<TaskJsonSourceBackedCompletion> {
        if !self.drained || self.active.is_some() || !self.pending_roots.is_empty() {
            return Err(TaskJsonSourceBackedError::IncompleteTask {
                provider: self.dialect.display_name,
                path: self
                    .active
                    .as_ref()
                    .and_then(|active| active.tasks.keys().next().cloned())
                    .unwrap_or_default(),
            });
        }
        Ok(TaskJsonSourceBackedCompletion {
            inventories: self.inventories.into_boxed_slice(),
            tasks: self.tasks.into_boxed_slice(),
            detected_but_unsupported: self.detected_but_unsupported,
            unavailable: self.unavailable,
        })
    }

    fn start_next_root(&mut self) -> TaskJsonSourceBackedResult<bool> {
        let Some(root) = self.pending_roots.pop_front() else {
            return Ok(false);
        };
        let discovery = discover_root(self.dialect, &root)?;
        let canonical_root = discovery.root_authority().tasks_root().to_path_buf();
        if !self.seen_roots.insert(canonical_root.clone()) {
            return Err(TaskJsonSourceBackedError::DuplicateRoot {
                provider: self.dialect.display_name,
                path: canonical_root,
            });
        }
        let active = active_root(self.dialect, discovery, &mut self.seen_sources)?;
        self.active = Some(active);
        Ok(true)
    }

    fn finish_root(&mut self, mut active: ActiveRoot) -> TaskJsonSourceBackedResult<()> {
        let completion = active.reader.finish_catalog()?;
        if !completion.inventory_complete
            || !completion.inventory_revalidated
            || completion
                .component_outcomes
                .iter()
                .any(|outcome| outcome.failure.is_some())
        {
            let path = active.tasks.keys().next().cloned().unwrap_or_default();
            return Err(TaskJsonSourceBackedError::IncompleteTask {
                provider: self.dialect.display_name,
                path,
            });
        }
        let checkpoints = completion
            .live_checkpoints
            .iter()
            .map(|checkpoint| (checkpoint.canonical_task_path.clone(), checkpoint))
            .collect::<BTreeMap<_, _>>();
        for (path, accumulator) in active.tasks {
            if !accumulator.opening.revalidate_all_components()? {
                return Err(TaskJsonSourceBackedError::TaskChanged {
                    provider: self.dialect.display_name,
                    path,
                });
            }
            let checkpoint = checkpoints.get(&path).ok_or_else(|| {
                TaskJsonSourceBackedError::MissingCheckpoint {
                    provider: self.dialect.display_name,
                    path: path.clone(),
                }
            })?;
            self.tasks
                .push(certify_task(self.dialect, accumulator, checkpoint)?);
        }
        let inventory = CertifiedSourceInventory::certify(
            active.opening_inventory.clone(),
            active.opening_inventory,
            DISCOVERY_REVISION,
            active.inventory_sources,
        )?;
        self.inventories.push(inventory);
        Ok(())
    }
}

fn active_root(
    dialect: TaskJsonNativeDialect,
    discovery: ClineDiscovery,
    seen_sources: &mut BTreeSet<[u8; 32]>,
) -> TaskJsonSourceBackedResult<ActiveRoot> {
    let authority_key = TypedKey::bytes(
        discovery
            .root_authority()
            .tasks_root()
            .as_os_str()
            .as_encoded_bytes()
            .to_vec(),
    )?;
    let opening_inventory = SourceInventoryObservation::new(
        dialect.provider.as_str(),
        INVENTORY_AUTHORITY_NAMESPACE,
        authority_key,
        INVENTORY_REVISION_KIND,
        discovery.root_authority().source_backed_revision(),
    )?;
    let mut tasks = BTreeMap::new();
    let mut component_owners = BTreeMap::new();
    let mut inventory_sources = Vec::new();
    for task in discovery.task_routes() {
        let source = task_source_key(dialect, task)?;
        let source_digest = source.identity().digest();
        if !seen_sources.insert(source_digest) {
            return Err(TaskJsonSourceBackedError::DuplicateTask {
                provider: dialect.display_name,
                task_id: task.directory_task_id.to_string(),
            });
        }
        let observation = task_observation(&source, task)?;
        let revision_digest = digest_revision(&observation);
        let mut content_digest = Sha256::new();
        content_digest.update(b"ctx-task-json-source-content-v1\0");
        content_digest.update(source_digest);
        content_digest.update(revision_digest);
        for component in [
            ClineComponent::ApiHistory,
            ClineComponent::UiMessages,
            ClineComponent::FallbackHistory,
            ClineComponent::TaskMetadata,
            ClineComponent::HistoryItem,
            ClineComponent::TaskIndex,
        ] {
            component_owners.insert(
                task.component(component).path.clone(),
                task.canonical_task_path.clone(),
            );
        }
        inventory_sources.push(source.clone());
        tasks.insert(
            task.canonical_task_path.clone(),
            TaskAccumulator {
                opening: task.clone(),
                source,
                observation,
                revision_digest,
                content_digest,
                counts: ScannedSourceCounts::default(),
                session: None,
            },
        );
    }
    Ok(ActiveRoot {
        reader: ClineNativeReader::new(discovery, &[], ClineNativeProfile::SourceBackedCoreOnly),
        opening_inventory,
        inventory_sources,
        component_owners,
        tasks,
    })
}

fn project_native_page(
    dialect: TaskJsonNativeDialect,
    active: &mut ActiveRoot,
    page: ClineCertifiedPage,
) -> TaskJsonSourceBackedResult<Option<TaskJsonSourceBackedPage>> {
    let task_path = active
        .component_owners
        .get(&page.source.canonical_path)
        .ok_or(TaskJsonSourceBackedError::UnownedPage {
            provider: dialect.display_name,
        })?
        .clone();
    let task = active
        .tasks
        .get_mut(&task_path)
        .ok_or(TaskJsonSourceBackedError::UnownedPage {
            provider: dialect.display_name,
        })?;
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
        return Ok(None);
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
    let mut documents = Vec::with_capacity(page.core.events.len());
    for event in page.core.events {
        documents.push(project_event(
            dialect,
            &task.source,
            task.revision_digest,
            session_id,
            &session,
            workspace.as_deref(),
            event,
        )?);
    }
    let estimated_owned_bytes = documents.iter().fold(0_usize, |total, document| {
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
    });
    if documents.len() > MAX_SOURCE_BACKED_PAGE_DOCUMENTS
        || estimated_owned_bytes > MAX_SOURCE_BACKED_PAGE_BYTES
    {
        return Err(TaskJsonSourceBackedError::PageBound {
            provider: dialect.display_name,
        });
    }
    Ok(Some(TaskJsonSourceBackedPage {
        source: task.source.clone(),
        documents: documents.into_boxed_slice(),
        estimated_owned_bytes,
    }))
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
    let event_sequence = event_sequence(dialect, &event)?;
    let body = bounded_event_body(&event);
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
        event_sequence,
        occurred_at_unix_ms: event.occurred_at_millis,
        event_type: event_kind(event.kind).to_owned(),
        role: Some(event_role(event.role).to_owned()),
        body,
        workspace: workspace.map(str::to_owned),
        cwd: workspace.map(str::to_owned),
        touched_files: event
            .file_touches
            .iter()
            .map(|touch| touch.path.to_string())
            .collect(),
    })
}

fn certify_task(
    dialect: TaskJsonNativeDialect,
    mut accumulator: TaskAccumulator,
    checkpoint: &ClineTaskCheckpoint,
) -> TaskJsonSourceBackedResult<TaskJsonCertifiedTask> {
    hash_metadata_checkpoint(&mut accumulator.content_digest, checkpoint);
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
        hash_array_checkpoint(&mut accumulator.content_digest, array);
        certified_bytes = checked_add(dialect, certified_bytes, array.complete_bytes)?;
    }
    accumulator.counts.certified_bytes = certified_bytes;
    let content_digest = accumulator.content_digest.finalize().into();
    let certified_source = CertifiedSource::certify(
        accumulator.observation.clone(),
        accumulator.observation,
        PARSER_REVISION,
        content_digest,
        accumulator.counts,
    )?;
    let session = &checkpoint.task_metadata.session;
    let session_id = derive_task_session_id(&accumulator.source, session.identity.as_str())?;
    let locator = session_locator(
        &accumulator.source,
        accumulator.revision_digest,
        &checkpoint.task_metadata.observation,
        checkpoint.task_metadata.content_sha256,
    )?;
    Ok(TaskJsonCertifiedTask {
        source: accumulator.source,
        certified_source,
        session: TaskJsonSourceBackedSession {
            session_id,
            provider_session_id: session.identity.as_str().to_owned(),
            title: session.title.as_deref().map(str::to_owned),
            workspace: session.workspace_directory.as_deref().map(str::to_owned),
            locator,
        },
    })
}

fn session_locator(
    source: &SourceKey,
    revision_digest: [u8; 32],
    observation: &ClineComponentObservation,
    content_digest: Option<[u8; 32]>,
) -> TaskJsonSourceBackedResult<Option<SourceRecordLocator>> {
    let Some(content_digest) = content_digest else {
        return Ok(None);
    };
    Ok(Some(SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Document {
            object_key: TypedKey::utf8(observation.component.file_name())?,
            json_pointer: None,
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(revision_digest),
        content_digest,
    )?))
}

impl TaskJsonSourceBackedResolver {
    fn new(
        dialect: TaskJsonNativeDialect,
        selected: &[ProviderSource],
    ) -> TaskJsonSourceBackedResult<Self> {
        let selection = select_authoritative_roots(dialect, selected);
        let mut seen_roots = BTreeSet::new();
        let mut tasks = BTreeMap::new();
        for root in selection.roots {
            let discovery = discover_root(dialect, &root)?;
            let root = discovery.root_authority().tasks_root().to_path_buf();
            if !seen_roots.insert(root.clone()) {
                return Err(TaskJsonSourceBackedError::DuplicateRoot {
                    provider: dialect.display_name,
                    path: root,
                });
            }
            for task in discovery.task_routes() {
                let source = task_source_key(dialect, task)?;
                let observation = task_observation(&source, task)?;
                let entry = ResolverTask {
                    revision_digest: digest_revision(&observation),
                    task: task.clone(),
                    source,
                };
                let digest = entry.source.identity().digest();
                if tasks.insert(digest, entry).is_some() {
                    return Err(TaskJsonSourceBackedError::DuplicateTask {
                        provider: dialect.display_name,
                        task_id: task.directory_task_id.to_string(),
                    });
                }
            }
        }
        Ok(Self { dialect, tasks })
    }

    pub(crate) fn hydrate_locator(
        &self,
        locator: &SourceRecordLocator,
    ) -> Result<Vec<u8>, HydrationFailure> {
        locator
            .validate_contract()
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
        let task = self
            .tasks
            .get(&locator.source().identity().digest())
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "task source is absent from the selected authoritative roots",
                )
            })?;
        task.source
            .validate_exact_descriptor(locator.source())
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
        if locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
            || locator.certified_source_revision_digest() != Some(&task.revision_digest)
        {
            return Err(hydration_failure(
                HydrationFailureKind::StaleSourceEvidence,
                "task source revision no longer matches the exact locator",
            ));
        }
        if !task
            .task
            .revalidate_all_components()
            .map_err(native_hydration_failure)?
        {
            return Err(hydration_failure(
                HydrationFailureKind::StaleSourceEvidence,
                "task source changed before hydration",
            ));
        }
        let bytes = match locator.coordinate() {
            NativeRecordCoordinate::Document {
                object_key,
                json_pointer,
            } if json_pointer.is_none() => {
                self.hydrate_document(task, object_key, locator.record_digest())?
            }
            NativeRecordCoordinate::TreeRecord {
                relative_file_key,
                record_coordinate,
            } => self.hydrate_tree_record(
                task,
                relative_file_key,
                record_coordinate,
                locator.record_digest(),
            )?,
            _ => {
                return Err(hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    "locator is not a task-relative Document or TreeRecord coordinate",
                ));
            }
        };
        if !task
            .task
            .revalidate_all_components()
            .map_err(native_hydration_failure)?
        {
            return Err(hydration_failure(
                HydrationFailureKind::StaleSourceEvidence,
                "task source changed during hydration",
            ));
        }
        Ok(bytes)
    }

    fn hydrate_document(
        &self,
        task: &ResolverTask,
        object_key: &TypedKey,
        expected_digest: &[u8; 32],
    ) -> Result<Vec<u8>, HydrationFailure> {
        let TypedKey::Utf8(relative_file) = object_key else {
            return Err(invalid_locator(
                "document object key is not a UTF-8 task-relative file",
            ));
        };
        let observation = task.task.metadata_authority();
        if relative_file != observation.component.file_name() {
            return Err(invalid_locator(
                "document object key is not the selected metadata authority",
            ));
        }
        let mut stats = ClinePublicationStats::default();
        let hydrated =
            hydrate_component(observation, &mut stats).map_err(local_hydration_failure)?;
        if &hydrated.content_sha256 != expected_digest {
            return Err(hydration_failure(
                HydrationFailureKind::StaleRecordEvidence,
                "metadata record digest no longer matches provider bytes",
            ));
        }
        Ok(hydrated.bytes)
    }

    fn hydrate_tree_record(
        &self,
        task: &ResolverTask,
        relative_file_key: &TypedKey,
        record_coordinate: &TypedKey,
        expected_digest: &[u8; 32],
    ) -> Result<Vec<u8>, HydrationFailure> {
        let TypedKey::Utf8(relative_file) = relative_file_key else {
            return Err(invalid_locator("tree-record file key is not UTF-8"));
        };
        let component = event_component_for_file(self.dialect, relative_file)
            .ok_or_else(|| invalid_locator("tree-record file is not an event component"))?;
        let observation = task.task.component(component.source_component());
        let TypedKey::Composite(parts) = record_coordinate else {
            return Err(invalid_locator("tree-record coordinate is not composite"));
        };
        let Some(TypedKey::U64(target_index)) = parts.first() else {
            return Err(invalid_locator(
                "tree-record coordinate has no native item index",
            ));
        };
        let mut stats = ClinePublicationStats::default();
        let mut scanner = ClineArrayScanner::open(observation, &mut stats, true)
            .map_err(local_hydration_failure)?;
        loop {
            match scanner.next_step().map_err(local_hydration_failure)? {
                ClineArrayScanStep::Item(item) if item.native_index == *target_index => {
                    if item.record_digest.as_ref() != Some(expected_digest) {
                        return Err(hydration_failure(
                            HydrationFailureKind::StaleRecordEvidence,
                            "native item digest no longer matches provider bytes",
                        ));
                    }
                    return item.bytes.ok_or_else(|| {
                        hydration_failure(
                            HydrationFailureKind::MissingRecord,
                            "native item exceeds the bounded hydration record size",
                        )
                    });
                }
                ClineArrayScanStep::Item(item) if item.native_index < *target_index => {}
                ClineArrayScanStep::Item(_) | ClineArrayScanStep::EmptyTerminal { .. } => {
                    return Err(hydration_failure(
                        HydrationFailureKind::MissingRecord,
                        "native item coordinate is absent from the provider array",
                    ));
                }
            }
        }
    }
}

impl ContentSourceResolver for TaskJsonSourceBackedResolver {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: self.hydrate_locator(request.locator())?,
        })
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        request
            .events()
            .iter()
            .map(|event| self.hydrate_event(event))
            .collect()
    }
}

struct SelectedRoots {
    roots: Vec<PathBuf>,
    detected_but_unsupported: Vec<ProviderSource>,
    unavailable: Vec<ProviderSource>,
}

fn select_authoritative_roots(
    dialect: TaskJsonNativeDialect,
    selected: &[ProviderSource],
) -> SelectedRoots {
    let mut roots = Vec::new();
    let mut detected_but_unsupported = Vec::new();
    let mut unavailable = Vec::new();
    for source in selected
        .iter()
        .filter(|source| source.provider == dialect.provider)
    {
        let exact_format = source.source_format == dialect.source_format;
        let supported = exact_format
            && source.source_kind == ProviderSourceKind::NativeHistory
            && source.import_support.is_importable();
        if !supported {
            detected_but_unsupported.push(source.clone());
        } else if matches!(
            source.status,
            ProviderSourceStatus::Available | ProviderSourceStatus::Empty
        ) && source.exists
        {
            roots.push(source.path.clone());
        } else {
            unavailable.push(source.clone());
        }
    }
    SelectedRoots {
        roots,
        detected_but_unsupported,
        unavailable,
    }
}

fn discover_root(
    dialect: TaskJsonNativeDialect,
    root: &Path,
) -> Result<ClineDiscovery, ClineNativePathError> {
    match dialect.provider {
        CaptureProvider::Cline => discover_cline_root(root),
        CaptureProvider::RooCode => discover_roo_root(root),
        _ => unreachable!("task-JSON source-backed adapter has only Cline and Roo dialects"),
    }
}

fn task_source_key(
    dialect: TaskJsonNativeDialect,
    task: &ClineLiveTaskObservation,
) -> TaskJsonSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive(
        dialect.provider.as_str(),
        dialect.source_format,
        SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::provider_native(
            SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(task.directory_task_id.as_ref())?,
        )?,
    )?)
}

fn task_observation(
    source: &SourceKey,
    task: &ClineLiveTaskObservation,
) -> TaskJsonSourceBackedResult<SourceObservation> {
    let mut revision = Sha256::new();
    revision.update(b"ctx-task-json-source-revision-v1\0");
    revision.update(source.identity().digest());
    for component in [
        ClineComponent::ApiHistory,
        ClineComponent::UiMessages,
        ClineComponent::FallbackHistory,
        ClineComponent::TaskMetadata,
        ClineComponent::HistoryItem,
        ClineComponent::TaskIndex,
    ] {
        revision.update([component as u8]);
        match &task.component(component).state {
            ClineObservedFileState::Missing => revision.update([0]),
            ClineObservedFileState::Present(stamp) => {
                revision.update([1]);
                revision.update(stamp.len().to_le_bytes());
                let token = stamp.token();
                revision.update(token.len().to_le_bytes());
                revision.update(token.as_bytes());
            }
            ClineObservedFileState::Unavailable(message) => {
                revision.update([2]);
                revision.update(message.len().to_le_bytes());
                revision.update(message.as_bytes());
            }
        }
    }
    SourceObservation::new(
        source.clone(),
        SOURCE_REVISION_KIND,
        revision.finalize().to_vec(),
    )
    .map_err(Into::into)
}

fn digest_revision(observation: &SourceObservation) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx-source-revision-evidence-v1\0");
    digest.update(observation.revision_kind().as_bytes());
    digest.update(observation.revision());
    digest.finalize().into()
}

fn derive_task_session_id(
    source: &SourceKey,
    provider_session_id: &str,
) -> TaskJsonSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn native_item_key(
    event: &ClineEventRow,
    revision_digest: [u8; 32],
) -> TaskJsonSourceBackedResult<NativeItemKey> {
    Ok(match &event.identity.item {
        ClineNativeItemKey::NativeId {
            native_id,
            occurrence,
        } => NativeItemKey::composite(
            NATIVE_ITEM_NAMESPACE,
            vec![
                TypedKey::U64(event.identity.component as u64),
                TypedKey::utf8(native_id.as_ref())?,
                TypedKey::U64(*occurrence),
            ],
        )?,
        ClineNativeItemKey::ComponentOrdinal(ordinal) => NativeItemKey::revision_scoped_position(
            NATIVE_ITEM_POSITION_KIND,
            TypedKey::composite(vec![
                TypedKey::U64(event.identity.component as u64),
                TypedKey::U64(*ordinal),
            ])?,
            TypedKey::bytes(revision_digest.to_vec())?,
        )?,
    })
}

fn typed_native_item_key(item: &ClineNativeItemKey) -> TaskJsonSourceBackedResult<TypedKey> {
    Ok(match item {
        ClineNativeItemKey::NativeId {
            native_id,
            occurrence,
        } => TypedKey::composite(vec![
            TypedKey::U64(0),
            TypedKey::utf8(native_id.as_ref())?,
            TypedKey::U64(*occurrence),
        ])?,
        ClineNativeItemKey::ComponentOrdinal(ordinal) => {
            TypedKey::composite(vec![TypedKey::U64(1), TypedKey::U64(*ordinal)])?
        }
    })
}

fn event_sequence(
    dialect: TaskJsonNativeDialect,
    event: &ClineEventRow,
) -> TaskJsonSourceBackedResult<u64> {
    const SUBRECORD_BITS: u32 = 20;
    const ITEM_BITS: u32 = 41;
    if event.native_order.item_index >= (1_u64 << ITEM_BITS)
        || u64::from(event.native_order.sub_index) >= (1_u64 << SUBRECORD_BITS)
    {
        return Err(TaskJsonSourceBackedError::EventSequenceBound {
            provider: dialect.display_name,
        });
    }
    Ok(
        ((event.native_order.component as u64) << (ITEM_BITS + SUBRECORD_BITS))
            | (event.native_order.item_index << SUBRECORD_BITS)
            | u64::from(event.native_order.sub_index),
    )
}

fn bounded_event_body(event: &ClineEventRow) -> String {
    let candidate = event
        .preview
        .as_deref()
        .or(event.body.as_deref())
        .or_else(|| {
            event
                .tool_call
                .as_ref()
                .and_then(|call| call.name.as_deref().or(call.call_id.as_deref()))
        })
        .or_else(|| {
            event
                .sparse_output
                .as_ref()
                .and_then(|output| output.preview.as_deref())
        })
        .unwrap_or_else(|| event_kind(event.kind));
    candidate.chars().take(MAX_LEXICAL_PREVIEW_CHARS).collect()
}

fn event_kind(kind: ClineEventKind) -> &'static str {
    match kind {
        ClineEventKind::Message => "message",
        ClineEventKind::Summary => "summary",
        ClineEventKind::Notice => "notice",
        ClineEventKind::ToolCall => "tool_call",
        ClineEventKind::ToolOutput => "tool_output",
        ClineEventKind::CommandOutput => "command_output",
    }
}

fn event_role(role: ClineEventRole) -> &'static str {
    match role {
        ClineEventRole::User => "user",
        ClineEventRole::Assistant => "assistant",
        ClineEventRole::System => "system",
        ClineEventRole::Unknown => "unknown",
    }
}

fn hash_record_evidence(
    digest: &mut Sha256,
    component: ClineComponent,
    evidence: ClineSourceRecordEvidence,
) {
    digest.update(b"record\0");
    digest.update([component as u8]);
    digest.update(evidence.native_index.to_le_bytes());
    digest.update(evidence.byte_start.to_le_bytes());
    digest.update(evidence.byte_length.to_le_bytes());
    digest.update(evidence.record_digest);
}

fn hash_metadata_checkpoint(digest: &mut Sha256, checkpoint: &ClineTaskCheckpoint) {
    digest.update(b"metadata\0");
    digest.update([checkpoint.task_metadata.observation.component as u8]);
    match checkpoint.task_metadata.content_sha256 {
        Some(content) => {
            digest.update([1]);
            digest.update(content);
        }
        None => digest.update([0]),
    }
    digest.update(checkpoint.task_metadata.session.metadata_hash);
}

fn hash_array_checkpoint(digest: &mut Sha256, checkpoint: &ClineArrayCheckpoint) {
    digest.update(b"array\0");
    digest.update([checkpoint.component as u8]);
    digest.update(checkpoint.complete_bytes.to_le_bytes());
    digest.update(checkpoint.observed_items.to_le_bytes());
    digest.update(checkpoint.retained_rows.to_le_bytes());
    digest.update(checkpoint.certified_revision_sha256);
    digest.update(checkpoint.final_frontier.prefix_semantic_sha256);
}

fn checked_add(
    dialect: TaskJsonNativeDialect,
    left: u64,
    right: u64,
) -> TaskJsonSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or_else(|| count_overflow(dialect))
}

fn count_overflow(dialect: TaskJsonNativeDialect) -> TaskJsonSourceBackedError {
    TaskJsonSourceBackedError::CountOverflow {
        provider: dialect.display_name,
    }
}

fn event_component_for_file(
    dialect: TaskJsonNativeDialect,
    file: &str,
) -> Option<ClineEventComponent> {
    [
        ClineEventComponent::ApiHistory,
        ClineEventComponent::UiMessages,
        ClineEventComponent::FallbackHistory,
    ]
    .into_iter()
    .find(|component| {
        component.source_component().file_name() == file
            && (dialect == TaskJsonNativeDialect::ROO
                || *component != ClineEventComponent::FallbackHistory)
    })
}

fn hydration_failure(kind: HydrationFailureKind, detail: impl ToString) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.to_string(),
    }
}

fn invalid_locator(detail: impl ToString) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::InvalidLocator, detail)
}

fn native_hydration_failure(error: ClineNativePathError) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
}

fn local_hydration_failure(error: ClineLocalReadError) -> HydrationFailure {
    match error {
        ClineLocalReadError::Local(failure) => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            failure.message,
        ),
        ClineLocalReadError::Fatal(error) => native_hydration_failure(error),
    }
}
