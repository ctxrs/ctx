use std::{collections::BTreeSet, sync::atomic::Ordering};

use ctx_history_core::{CaptureSource, Event, FileTouched, Run, Session, SessionEdge, SyncCursor};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::canonical_observations::canonical_actor_by_id;
use crate::connection::ms_to_time;
use crate::result_storage::durable_event;
use crate::runs::provider_output_run_is_retained_failure;
use crate::source_generations::{
    NativePathRetainedSourceEntities, NativePathSourceEntityFrontier, NativePathSourceEntityKind,
    NativePathSourceGenerationKey, NativePathSourceRetirementPage,
    NativePathSourceRetirementPreparation,
};
use crate::{
    CanonicalActor, EventSearchBulkGroupAdmission, JournalCheckpoint, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceLocatorResolution, ProviderSourceRouteBinding,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition, Result, Store,
    StoreError,
};

pub const NATIVE_PATH_MAX_GROUP_PAGES: usize = 512;
pub const NATIVE_PATH_MAX_GROUP_SOURCES: usize = 512;
pub const NATIVE_PATH_MAX_RETAINED_PAGE_BYTES: usize = 8 * 1024 * 1024;
pub const NATIVE_PATH_MAX_MUTATION_UNITS: usize = 4_096;
pub const NATIVE_PATH_MAX_CORE_BOUND_BYTES: usize = 8 * 1024 * 1024;
pub const NATIVE_PATH_MAX_JOURNAL_RECORDS: usize = 4_096;
pub const NATIVE_PATH_MAX_JOURNAL_BYTES: usize = 8 * 1024 * 1024;

const BOUND_MUTATION_HEADER_BYTES: usize = 8;
const BOUND_TAG_BYTES: usize = 1;
const BOUND_LENGTH_BYTES: usize = 8;
const NATIVE_PATH_CURSOR_ENVELOPE_VERSION: u32 = 1;

/// Coordinator-owned totals that the Store revalidates before opening the
/// publication transaction and again before commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativePathGroupAccounting {
    page_count: usize,
    source_count: usize,
    retained_page_bytes: usize,
}

impl NativePathGroupAccounting {
    pub fn new(page_count: usize, source_count: usize, retained_page_bytes: usize) -> Result<Self> {
        let accounting = Self {
            page_count,
            source_count,
            retained_page_bytes,
        };
        accounting.validate()?;
        Ok(accounting)
    }

    pub fn page_count(self) -> usize {
        self.page_count
    }

    pub fn source_count(self) -> usize {
        self.source_count
    }

    pub fn retained_page_bytes(self) -> usize {
        self.retained_page_bytes
    }

    fn validate(self) -> Result<()> {
        validate_limit(
            "coordinator-supplied pages",
            self.page_count,
            NATIVE_PATH_MAX_GROUP_PAGES,
        )?;
        validate_limit(
            "coordinator-supplied sources",
            self.source_count,
            NATIVE_PATH_MAX_GROUP_SOURCES,
        )?;
        validate_limit(
            "coordinator-supplied retained page bytes",
            self.retained_page_bytes,
            NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NativePathCursorKey {
    team_id: Option<String>,
    device_id: String,
    stream: String,
}

impl NativePathCursorKey {
    pub fn new(
        team_id: Option<String>,
        device_id: impl Into<String>,
        stream: impl Into<String>,
    ) -> Self {
        Self {
            team_id,
            device_id: device_id.into(),
            stream: stream.into(),
        }
    }

    pub fn team_id(&self) -> Option<&str> {
        self.team_id.as_deref()
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn stream(&self) -> &str {
        &self.stream
    }

    fn from_cursor(cursor: &SyncCursor) -> Self {
        Self {
            team_id: cursor.team_id.clone(),
            device_id: cursor.device_id.clone(),
            stream: cursor.stream.clone(),
        }
    }

    fn matches(&self, cursor: &SyncCursor) -> bool {
        self.team_id == cursor.team_id
            && self.device_id == cursor.device_id
            && self.stream == cursor.stream
    }
}

/// One exact cursor transition. `expected_cursor` is the complete currently
/// encoded cursor column, not a reconstructed `SyncCursor` with new timestamps.
/// `next.cursor` is the provider-owned semantic payload; the Store wraps it in
/// its committed publication envelope.
#[derive(Debug, Clone)]
pub struct NativePathCursorTransition {
    key: NativePathCursorKey,
    expected_cursor: Option<String>,
    next: SyncCursor,
}

impl NativePathCursorTransition {
    pub fn new(expected_cursor: Option<String>, next: SyncCursor) -> Self {
        Self {
            key: NativePathCursorKey::from_cursor(&next),
            expected_cursor,
            next,
        }
    }

    pub fn key(&self) -> &NativePathCursorKey {
        &self.key
    }

    pub fn expected_cursor(&self) -> Option<&str> {
        self.expected_cursor.as_deref()
    }

    pub fn next(&self) -> &SyncCursor {
        &self.next
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativePathCommittedCursorEnvelope {
    version: u32,
    publication_id: String,
    provider_cursor: String,
    journal_checkpoint: Option<JournalCheckpoint>,
}

/// Store-owned mechanical envelope decoded from a committed NativePath cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePathCommittedCursor {
    publication_id: String,
    provider_cursor: String,
    journal_checkpoint: Option<JournalCheckpoint>,
}

impl NativePathCommittedCursor {
    pub fn publication_id(&self) -> &str {
        &self.publication_id
    }

    pub fn provider_cursor(&self) -> &str {
        &self.provider_cursor
    }

    pub fn journal_checkpoint(&self) -> Option<&JournalCheckpoint> {
        self.journal_checkpoint.as_ref()
    }
}

/// Decodes only the Store-owned cursor envelope. Provider cursor semantics
/// remain capture-owned.
pub fn decode_native_path_committed_cursor(encoded: &str) -> Result<NativePathCommittedCursor> {
    let envelope = decode_cursor_envelope(encoded)?;
    Ok(NativePathCommittedCursor {
        publication_id: envelope.publication_id,
        provider_cursor: envelope.provider_cursor,
        journal_checkpoint: envelope.journal_checkpoint,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativePathCursorSetClassification {
    AllExpected,
    AllNextSameGroup {
        checkpoint: Option<JournalCheckpoint>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePathGroupReceipt {
    coordinator: NativePathGroupAccounting,
    attempted_mutation_units: usize,
    core_bound_value_bytes: usize,
    journal_records: usize,
    journal_uncompressed_bytes: usize,
    checkpoint: Option<JournalCheckpoint>,
}

impl NativePathGroupReceipt {
    pub fn coordinator_accounting(&self) -> NativePathGroupAccounting {
        self.coordinator
    }

    pub fn attempted_mutation_units(&self) -> usize {
        self.attempted_mutation_units
    }

    pub fn core_bound_value_bytes(&self) -> usize {
        self.core_bound_value_bytes
    }

    pub fn journal_records(&self) -> usize {
        self.journal_records
    }

    pub fn journal_uncompressed_bytes(&self) -> usize {
        self.journal_uncompressed_bytes
    }

    pub fn checkpoint(&self) -> Option<&JournalCheckpoint> {
        self.checkpoint.as_ref()
    }
}

#[derive(Debug)]
enum CursorPublicationState {
    None,
    Expected {
        publication_id: String,
        transitions: Vec<NativePathCursorTransition>,
        rows: Vec<Option<SyncCursor>>,
    },
    Published,
    AlreadyCommitted,
}

struct NativePathSourceRetirementPreview {
    key: NativePathSourceGenerationKey,
    after: Option<NativePathSourceEntityFrontier>,
    limit: usize,
    page: NativePathSourceRetirementPage,
}

/// Store-owned, non-rotating `BEGIN IMMEDIATE` publication transaction.
///
/// Its public mutation surface is intentionally limited to canonical model
/// operations. Callers cannot supply SQL, a Store closure, mutation counts, or
/// bind accounting.
pub struct NativePathPublicationGroup<'store> {
    store: &'store Store,
    token: Uuid,
    coordinator: NativePathGroupAccounting,
    attempted_mutation_units: usize,
    core_bound_value_bytes: usize,
    journal_records: usize,
    journal_uncompressed_bytes: usize,
    checkpoint: Option<JournalCheckpoint>,
    journal_prepared: bool,
    cursor_state: CursorPublicationState,
    source_retirement_preview: Option<NativePathSourceRetirementPreview>,
    finished: bool,
}

impl Store {
    #[doc(hidden)]
    pub fn begin_native_path_publication_group(
        &self,
        admission: EventSearchBulkGroupAdmission,
        coordinator: NativePathGroupAccounting,
    ) -> Result<NativePathPublicationGroup<'_>> {
        if self.native_path_group_token.get().is_some()
            || self.projection_journal_group_collector.borrow().is_some()
        {
            self.native_path_group_poisoned
                .store(true, Ordering::SeqCst);
            return Err(StoreError::NativePathGroupAlreadyActive);
        }
        coordinator.validate()?;
        self.consume_event_search_bulk_group_admission(admission)?;
        if !self.conn.is_autocommit() || self.batch_depth.get() != 0 {
            return Err(StoreError::NativePathGroupRequiresAutocommit);
        }

        self.native_path_group_poisoned
            .store(false, Ordering::SeqCst);
        self.conn.flush_prepared_statement_cache();
        let mutation_scope = self.native_path_mutation_scope.clone();
        let poisoned = self.native_path_group_poisoned.clone();
        self.conn.authorizer(Some(move |context: AuthContext<'_>| {
            let is_write = matches!(
                context.action,
                AuthAction::Insert { .. }
                    | AuthAction::Update { .. }
                    | AuthAction::Delete { .. }
                    | AuthAction::CreateIndex { .. }
                    | AuthAction::CreateTable { .. }
                    | AuthAction::CreateTrigger { .. }
                    | AuthAction::CreateView { .. }
                    | AuthAction::CreateVtable { .. }
                    | AuthAction::DropIndex { .. }
                    | AuthAction::DropTable { .. }
                    | AuthAction::DropTrigger { .. }
                    | AuthAction::DropView { .. }
                    | AuthAction::DropVtable { .. }
                    | AuthAction::AlterTable { .. }
            );
            if is_write && !mutation_scope.load(Ordering::SeqCst) {
                poisoned.store(true, Ordering::SeqCst);
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        }));

        if let Err(error) = self.begin_immediate_batch() {
            self.clear_native_path_authorizer();
            return Err(error);
        }
        let token = Uuid::new_v4();
        self.native_path_group_token.set(Some(token));
        self.projection_journal_group_collector
            .replace(Some(Default::default()));
        Ok(NativePathPublicationGroup {
            store: self,
            token,
            coordinator,
            attempted_mutation_units: 0,
            core_bound_value_bytes: 0,
            journal_records: 0,
            journal_uncompressed_bytes: 0,
            checkpoint: None,
            journal_prepared: false,
            cursor_state: CursorPublicationState::None,
            source_retirement_preview: None,
            finished: false,
        })
    }

    pub(crate) fn poison_native_path_group(&self) {
        if self.native_path_group_token.get().is_some() {
            self.native_path_group_poisoned
                .store(true, Ordering::SeqCst);
        }
    }

    fn clear_native_path_authorizer(&self) {
        self.native_path_mutation_scope
            .store(false, Ordering::SeqCst);
        self.conn.flush_prepared_statement_cache();
        self.conn
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    }
}

impl NativePathPublicationGroup<'_> {
    /// Upserts one canonical capture source and derives accounting from all
    /// eighteen values bound by `Store::upsert_capture_source`.
    pub fn upsert_capture_source(&mut self, source: &CaptureSource) -> Result<()> {
        self.execute_typed_mutation(capture_source_bind_bytes(source), |store| {
            store.upsert_capture_source(source)
        })
    }

    /// Attempts one canonical insert-if-absent session mutation.
    pub fn insert_session_if_absent(&mut self, session: &Session) -> Result<bool> {
        self.execute_typed_mutation(session_bind_bytes(session), |store| {
            store.insert_session_if_absent(session)
        })
    }

    /// Upserts one canonical session inside the bounded publication group.
    /// Providers use this when an append refreshes canonical session metadata
    /// or a fully observed parent replaces an earlier bounded placeholder.
    pub fn upsert_session(&mut self, session: &Session) -> Result<()> {
        self.execute_typed_mutation(session_bind_bytes(session), |store| {
            store.upsert_session(session)
        })
    }

    /// Reconciles one canonical provider event. Provider projection semantics
    /// remain in the existing Store operation; this guard owns its bounds.
    pub fn reconcile_provider_event(
        &mut self,
        event: &Event,
        authority: ProviderEventHashAuthority,
    ) -> Result<bool> {
        self.ensure_mutable()?;
        self.charge_core_mutations(1, 0)?;
        let (result, encoded_bytes) = match self.with_write_scope(|store| {
            store.reconcile_provider_event_with_native_path_accounting(event, authority)
        }) {
            Ok(result) => result,
            Err(error) => return self.poison_with(error),
        };
        self.charge_core_mutations(0, encoded_bytes)?;
        Ok(result)
    }

    /// Performs a one-time, exact migration from a released provider-supplied
    /// positional hash to the provider's current normalized-payload hash.
    pub fn reconcile_provider_event_migrating_exact_legacy_provider_hash(
        &mut self,
        event: &Event,
        exact_legacy_provider_hash: &str,
    ) -> Result<bool> {
        self.ensure_mutable()?;
        if exact_legacy_provider_hash.is_empty() || exact_legacy_provider_hash.len() > 4 * 1024 {
            return self.poison_with(StoreError::InvalidNativePathLegacyProviderHashMigration);
        }
        self.charge_core_mutations(1, exact_legacy_provider_hash.len())?;
        let (result, encoded_bytes) = match self.with_write_scope(|store| {
            store
                .reconcile_provider_event_migrating_exact_legacy_provider_hash_with_native_path_accounting(
                    event,
                    exact_legacy_provider_hash,
                )
        }) {
            Ok(result) => result,
            Err(error) => return self.poison_with(error),
        };
        self.charge_core_mutations(0, encoded_bytes)?;
        Ok(result)
    }

    /// Upserts one canonical run.
    pub fn upsert_run(&mut self, run: &Run) -> Result<()> {
        self.execute_typed_mutation(run_bind_bytes(run), |store| store.upsert_run(run))
    }

    /// Upserts one canonical file-touch row.
    pub fn upsert_file_touched(&mut self, file: &FileTouched) -> Result<()> {
        self.execute_typed_mutation(file_touch_bind_bytes(file), |store| {
            store.upsert_file_touched(file)
        })
    }

    /// Durably stages one bounded page of the canonical entities retained by
    /// the current provider-owned source generation.
    pub fn stage_source_generation_page(
        &mut self,
        key: &NativePathSourceGenerationKey,
        retained: &NativePathRetainedSourceEntities,
    ) -> Result<()> {
        self.ensure_mutable()?;
        let encoded_bytes = key.bound_value_bytes().and_then(|key_bytes| {
            key_bytes
                .checked_add(retained.bound_value_bytes())
                .ok_or(StoreError::NativePathSourceGenerationConflict)
        });
        let encoded_bytes = match encoded_bytes {
            Ok(value) => value,
            Err(error) => return self.poison_with(error),
        };
        self.charge_core_mutations(retained.len().saturating_add(1), encoded_bytes)?;
        match self.with_write_scope(|store| store.stage_source_generation_page_tx(key, retained)) {
            Ok(()) => Ok(()),
            Err(error) => self.poison_with(error),
        }
    }

    /// Previews the exact stable retirement page before cursor
    /// classification. This is a read-only operation in the group's existing
    /// `BEGIN IMMEDIATE` transaction; a newly published cursor cannot commit
    /// unless the matching retirement page is subsequently applied.
    pub fn preview_source_generation_retirement_page(
        &mut self,
        key: &NativePathSourceGenerationKey,
        after: Option<&NativePathSourceEntityFrontier>,
        limit: usize,
    ) -> Result<NativePathSourceRetirementPage> {
        self.ensure_open()?;
        if self.is_poisoned()
            || self.attempted_mutation_units != 0
            || self.journal_prepared
            || !matches!(self.cursor_state, CursorPublicationState::None)
            || limit == 0
            || limit > (NATIVE_PATH_MAX_MUTATION_UNITS.saturating_sub(2) / 2)
        {
            return self.poison_with(StoreError::NativePathSourceGenerationConflict);
        }
        if let Some(preview) = &self.source_retirement_preview {
            if preview.key == *key && preview.after.as_ref() == after && preview.limit == limit {
                return Ok(preview.page.clone());
            }
            return self.poison_with(StoreError::NativePathSourceGenerationConflict);
        }
        let page = match self
            .store
            .preview_source_generation_retirement_page_tx(key, after, limit)
        {
            Ok(value) => value,
            Err(error) => return self.poison_with(error),
        };
        self.source_retirement_preview = Some(NativePathSourceRetirementPreview {
            key: key.clone(),
            after: after.cloned(),
            limit,
            page: page.clone(),
        });
        Ok(page)
    }

    /// Retires one bounded, stable page of canonical rows omitted from the
    /// provider-owned current generation. Capture sources and routes remain;
    /// only the omitted canonical entities are soft-deleted.
    pub fn retire_source_generation_page(
        &mut self,
        key: &NativePathSourceGenerationKey,
        after: Option<&NativePathSourceEntityFrontier>,
        limit: usize,
        retired_at_ms: i64,
    ) -> Result<NativePathSourceRetirementPage> {
        self.ensure_mutable()?;
        if limit == 0 || limit > (NATIVE_PATH_MAX_MUTATION_UNITS.saturating_sub(2) / 2) {
            return self.poison_with(StoreError::NativePathSourceGenerationConflict);
        }
        if let Some(preview) = &self.source_retirement_preview {
            if preview.key != *key || preview.after.as_ref() != after || preview.limit != limit {
                return self.poison_with(StoreError::NativePathSourceGenerationConflict);
            }
        }
        let retired_at = match ms_to_time(retired_at_ms) {
            Ok(value) => value,
            Err(error) => return self.poison_with(StoreError::Sql(error)),
        };
        let preparation = match self.with_write_scope(|store| {
            store.prepare_source_generation_retirement_page_tx(key, after, limit)
        }) {
            Ok(value) => value,
            Err(error) => return self.poison_with(error),
        };
        let (candidates, next_after, done) = match preparation {
            NativePathSourceRetirementPreparation::Replay(page) => {
                self.consume_source_retirement_preview(&page)?;
                return Ok(page);
            }
            NativePathSourceRetirementPreparation::Work {
                candidates,
                next_after,
                done,
            } => (candidates, next_after, done),
        };

        let frontier_bytes = std::mem::size_of::<Uuid>()
            .saturating_add(
                after
                    .map(|value| value.kind.as_str().len())
                    .unwrap_or_default(),
            )
            .saturating_add(
                next_after
                    .as_ref()
                    .map(|value| value.kind.as_str().len())
                    .unwrap_or_default(),
            );
        let key_bytes = match key.bound_value_bytes() {
            Ok(value) => value,
            Err(error) => return self.poison_with(error),
        };
        self.charge_core_mutations(
            candidates.len().saturating_add(1),
            key_bytes.saturating_add(frontier_bytes),
        )?;

        let mut retired = 0_usize;
        for candidate in &candidates {
            if candidate.retained {
                continue;
            }
            match candidate.kind {
                NativePathSourceEntityKind::SessionEdge => {
                    let mut edge = match self.store.get_session_edge(candidate.id) {
                        Ok(value) => value,
                        Err(error) => return self.poison_with(error),
                    };
                    let expected_actor =
                        match canonical_actor_by_id(&self.store.conn, edge.from_session_id) {
                            Ok(Some(value)) => value,
                            Ok(None) => {
                                return self
                                    .poison_with(StoreError::NotFound(edge.from_session_id));
                            }
                            Err(error) => return self.poison_with(StoreError::Sql(error)),
                        };
                    edge.timestamps.updated_at = retired_at;
                    edge.sync.deleted_at = Some(retired_at);
                    self.upsert_projection_neutral_session_edge(&expected_actor, &edge)?;
                }
                NativePathSourceEntityKind::Run => {
                    let mut run = match self.store.get_run(candidate.id) {
                        Ok(value) => value,
                        Err(error) => return self.poison_with(error),
                    };
                    run.timestamps.updated_at = retired_at;
                    run.sync.deleted_at = Some(retired_at);
                    self.upsert_run(&run)?;
                }
                NativePathSourceEntityKind::Event => {
                    let mut event = match self.store.get_event(candidate.id) {
                        Ok(value) => value,
                        Err(error) => return self.poison_with(error),
                    };
                    event.sync.deleted_at = Some(retired_at);
                    self.upsert_event_exact(&event)?;
                }
                NativePathSourceEntityKind::FileTouch => {
                    let mut file = match self.store.get_file_touched(candidate.id) {
                        Ok(value) => value,
                        Err(error) => return self.poison_with(error),
                    };
                    file.timestamps.updated_at = retired_at;
                    file.sync.deleted_at = Some(retired_at);
                    self.upsert_file_touched(&file)?;
                }
                NativePathSourceEntityKind::Session => {
                    let mut session = match self.store.get_session(candidate.id) {
                        Ok(value) => value,
                        Err(error) => return self.poison_with(error),
                    };
                    session.timestamps.updated_at = retired_at;
                    session.sync.deleted_at = Some(retired_at);
                    self.upsert_session(&session)?;
                }
            }
            retired = retired.saturating_add(1);
        }

        let page = NativePathSourceRetirementPage {
            next_after,
            done,
            inspected: candidates.len(),
            retired,
        };
        self.consume_source_retirement_preview(&page)?;
        match self.with_write_scope(|store| {
            store.finish_source_generation_retirement_page_tx(key, after, &page)
        }) {
            Ok(()) => Ok(page),
            Err(error) => self.poison_with(error),
        }
    }

    fn consume_source_retirement_preview(
        &mut self,
        page: &NativePathSourceRetirementPage,
    ) -> Result<()> {
        let Some(preview) = self.source_retirement_preview.take() else {
            return Ok(());
        };
        if preview.page != *page {
            return self.poison_with(StoreError::NativePathSourceGenerationConflict);
        }
        Ok(())
    }

    fn upsert_event_exact(&mut self, event: &Event) -> Result<()> {
        self.ensure_mutable()?;
        self.charge_core_mutations(1, 0)?;
        let encoded_bytes = match self
            .with_write_scope(|store| store.upsert_event_with_native_path_accounting(event))
        {
            Ok(value) => value,
            Err(error) => return self.poison_with(error),
        };
        self.charge_core_mutations(0, encoded_bytes)
    }

    /// Reconciles one provider-owned physical source locator inside the same
    /// atomic NativePath publication transaction. Provider revision and
    /// relocation semantics remain provider-owned; this surface only supplies
    /// typed transaction ownership and conservative bound accounting.
    pub fn reconcile_provider_source_locator(
        &mut self,
        observation: &ProviderSourceLocatorObservation,
    ) -> Result<ProviderSourceLocatorResolution> {
        self.execute_typed_mutation(Ok(observation.native_path_bound_value_bytes()), |store| {
            store.reconcile_provider_source_locator_tx(observation)
        })
    }

    /// Binds one canonical capture source to the exact route returned by
    /// `reconcile_provider_source_locator` without opening a nested batch.
    pub fn bind_capture_source_provider_route(
        &mut self,
        capture_source_id: Uuid,
        binding: &ProviderSourceRouteBinding,
    ) -> Result<()> {
        let encoded_bytes =
            std::mem::size_of::<Uuid>().saturating_add(binding.native_path_bound_value_bytes());
        self.execute_typed_mutation(Ok(encoded_bytes), |store| {
            store.bind_capture_source_provider_route_tx(capture_source_id, binding)
        })
    }

    /// Atomically revokes the exact current provider route while preserving all
    /// canonical Core history and cursor state.
    pub fn retire_provider_source_route(
        &mut self,
        retirement: &ProviderSourceRouteRetirement,
    ) -> Result<ProviderSourceRouteRetirementDisposition> {
        self.execute_typed_mutation(Ok(retirement.native_path_bound_value_bytes()), |store| {
            store.retire_provider_source_route_tx(retirement)
        })
    }

    /// Reads and classifies every required cursor row inside this transaction.
    /// Duplicate, empty, missing, extra, mixed, malformed, or stale sets fail
    /// closed and poison the group.
    pub fn classify_cursor_set(
        &mut self,
        publication_id: &str,
        transitions: &[NativePathCursorTransition],
    ) -> Result<NativePathCursorSetClassification> {
        self.ensure_mutable()?;
        if publication_id.is_empty()
            || transitions.is_empty()
            || transitions.len() != self.coordinator.source_count()
            || self.attempted_mutation_units != 0
            || !matches!(self.cursor_state, CursorPublicationState::None)
        {
            return self.poison_with(StoreError::InvalidNativePathCursorSet);
        }
        let unique = transitions
            .iter()
            .map(|transition| transition.key.clone())
            .collect::<BTreeSet<_>>();
        if unique.len() != transitions.len() {
            return self.poison_with(StoreError::InvalidNativePathCursorSet);
        }

        let rows = match transitions
            .iter()
            .map(|transition| {
                self.store.get_sync_cursor(
                    transition.key.team_id(),
                    transition.key.device_id(),
                    transition.key.stream(),
                )
            })
            .collect::<Result<Vec<_>>>()
        {
            Ok(rows) => rows,
            Err(error) => return self.poison_with(error),
        };

        let all_expected = rows.iter().zip(transitions).all(|(row, transition)| {
            row.as_ref().map(|cursor| cursor.cursor.as_str())
                == transition.expected_cursor.as_deref()
        });

        let mut common_checkpoint: Option<Option<JournalCheckpoint>> = None;
        let mut all_next = true;
        for (row, transition) in rows.iter().zip(transitions) {
            let Some(row) = row else {
                all_next = false;
                break;
            };
            let Ok(envelope) = decode_cursor_envelope(&row.cursor) else {
                all_next = false;
                break;
            };
            let canonical = match encode_cursor_envelope(&envelope) {
                Ok(canonical) => canonical,
                Err(error) => return self.poison_with(error),
            };
            if canonical != row.cursor
                || envelope.publication_id != publication_id
                || envelope.provider_cursor != transition.next.cursor
            {
                all_next = false;
                break;
            }
            match &common_checkpoint {
                Some(checkpoint) if checkpoint != &envelope.journal_checkpoint => {
                    all_next = false;
                    break;
                }
                None => common_checkpoint = Some(envelope.journal_checkpoint.clone()),
                Some(_) => {}
            }
        }

        if all_expected == all_next {
            return self.poison_with(StoreError::NativePathCursorConflict);
        }
        if all_expected {
            self.cursor_state = CursorPublicationState::Expected {
                publication_id: publication_id.to_owned(),
                transitions: transitions.to_vec(),
                rows,
            };
            return Ok(NativePathCursorSetClassification::AllExpected);
        }

        let checkpoint = common_checkpoint.unwrap_or(None);
        match self
            .store
            .verify_projection_journal_checkpoint_in_transaction(checkpoint.as_ref())
        {
            Ok(true) => {}
            Ok(false) => return self.poison_with(StoreError::NativePathCursorConflict),
            Err(error) => return self.poison_with(error),
        }
        self.checkpoint.clone_from(&checkpoint);
        self.journal_prepared = true;
        self.cursor_state = CursorPublicationState::AlreadyCommitted;
        Ok(NativePathCursorSetClassification::AllNextSameGroup { checkpoint })
    }

    /// Flushes the group collector in bounded chunks and returns the exact
    /// checkpoint from this still-open SQLite transaction.
    pub fn prepare_journal_checkpoint(&mut self) -> Result<Option<JournalCheckpoint>> {
        self.ensure_open()?;
        if self.is_poisoned() {
            return Err(StoreError::NativePathGroupPoisoned);
        }
        if self.journal_prepared {
            return Ok(self.checkpoint.clone());
        }
        let flush_result = self.with_write_scope(|store| {
            let mut collector = store.projection_journal_group_collector.borrow_mut();
            let collector = collector
                .as_mut()
                .ok_or(StoreError::NativePathGroupPoisoned)?;
            let (records, bytes) = collector.seal_and_flush(&store.conn)?;
            let checkpoint = store.projection_journal_checkpoint_in_transaction()?;
            Ok((records, bytes, checkpoint))
        });
        let (records, bytes, checkpoint) = match flush_result {
            Ok(value) => value,
            Err(error) => return self.poison_with(error),
        };
        if let Err(error) = validate_limit(
            "actual journal records",
            records,
            NATIVE_PATH_MAX_JOURNAL_RECORDS,
        ) {
            return self.poison_with(error);
        }
        if let Err(error) = validate_limit(
            "uncompressed journal encoding bytes",
            bytes,
            NATIVE_PATH_MAX_JOURNAL_BYTES,
        ) {
            return self.poison_with(error);
        }
        self.journal_records = records;
        self.journal_uncompressed_bytes = bytes;
        self.checkpoint = checkpoint;
        self.journal_prepared = true;
        Ok(self.checkpoint.clone())
    }

    /// Publishes the previously classified all-expected cursor set using its
    /// exact freshly read rows. The Store embeds one common exact checkpoint.
    pub fn publish_cursor_set(&mut self) -> Result<()> {
        self.ensure_open()?;
        if self.is_poisoned() {
            return Err(StoreError::NativePathGroupPoisoned);
        }
        if !self.journal_prepared {
            return self.poison_with(StoreError::InvalidNativePathCursorSet);
        }

        let state = std::mem::replace(&mut self.cursor_state, CursorPublicationState::None);
        let CursorPublicationState::Expected {
            publication_id,
            transitions,
            rows,
        } = state
        else {
            return self.poison_with(StoreError::InvalidNativePathCursorSet);
        };

        let mut next = Vec::with_capacity(transitions.len());
        let mut encoded_bytes = 0_usize;
        for (transition, current) in transitions.iter().zip(&rows) {
            if !transition.key.matches(&transition.next) {
                return self.poison_with(StoreError::InvalidNativePathCursorSet);
            }
            let envelope = NativePathCommittedCursorEnvelope {
                version: NATIVE_PATH_CURSOR_ENVELOPE_VERSION,
                publication_id: publication_id.clone(),
                provider_cursor: transition.next.cursor.clone(),
                journal_checkpoint: self.checkpoint.clone(),
            };
            let mut cursor = transition.next.clone();
            cursor.cursor = match encode_cursor_envelope(&envelope) {
                Ok(encoded) => encoded,
                Err(error) => return self.poison_with(error),
            };
            encoded_bytes =
                encoded_bytes.saturating_add(encoded_cursor_cas_bytes(current.as_ref(), &cursor));
            next.push(cursor);
        }
        self.charge_core_mutations(next.len(), encoded_bytes)?;
        let result = self.with_write_scope(|store| {
            for ((current, transition), next) in rows.iter().zip(&transitions).zip(&next) {
                if !transition.key.matches(next)
                    || !store.compare_and_set_sync_cursor(current.as_ref(), next)?
                {
                    return Err(StoreError::NativePathCursorConflict);
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => {
                self.cursor_state = CursorPublicationState::Published;
                Ok(())
            }
            Err(error) => self.poison_with(error),
        }
    }

    /// Inserts or updates only a session edge after proving the child's
    /// canonical actor row is exactly unchanged. It never rewrites parent/root
    /// actor columns and never fans out dependent journal events.
    pub fn upsert_projection_neutral_session_edge(
        &mut self,
        expected_actor: &CanonicalActor,
        edge: &SessionEdge,
    ) -> Result<()> {
        if edge.from_session_id != expected_actor.direct_session_id {
            return self.poison_with(StoreError::ProjectionChangingSessionRelationship);
        }
        self.execute_typed_mutation(session_edge_bind_bytes(edge), |store| {
            store.with_atomic_write(|| {
                let before = canonical_actor_by_id(&store.conn, edge.from_session_id)?
                    .ok_or(StoreError::NotFound(edge.from_session_id))?;
                if &before != expected_actor {
                    return Err(StoreError::ProjectionChangingSessionRelationship);
                }
                store.upsert_session_edge(edge)?;
                let after = canonical_actor_by_id(&store.conn, edge.from_session_id)?
                    .ok_or(StoreError::NotFound(edge.from_session_id))?;
                if after != before {
                    return Err(StoreError::ProjectionChangingSessionRelationship);
                }
                Ok(())
            })
        })
    }

    pub fn commit(mut self) -> Result<NativePathGroupReceipt> {
        if let Err(error) = self.ensure_open() {
            self.rollback_internal();
            return Err(error);
        }
        if self.source_retirement_preview.is_some()
            && matches!(self.cursor_state, CursorPublicationState::Published)
        {
            self.store.poison_native_path_group();
        }
        if !matches!(
            self.cursor_state,
            CursorPublicationState::Published | CursorPublicationState::AlreadyCommitted
        ) {
            self.store.poison_native_path_group();
        }
        if self.is_poisoned() || self.collector_overflowed() {
            self.rollback_internal();
            return Err(StoreError::NativePathGroupPoisoned);
        }
        if let Err(error) = self.prepare_journal_checkpoint() {
            self.rollback_internal();
            return Err(error);
        }
        if let Err(error) = self.validate_final_accounting() {
            self.store.poison_native_path_group();
            self.rollback_internal();
            return Err(error);
        }
        let receipt = NativePathGroupReceipt {
            coordinator: self.coordinator,
            attempted_mutation_units: self.attempted_mutation_units,
            core_bound_value_bytes: self.core_bound_value_bytes,
            journal_records: self.journal_records,
            journal_uncompressed_bytes: self.journal_uncompressed_bytes,
            checkpoint: self.checkpoint.clone(),
        };
        // SQLite can defer canonical virtual-table work until COMMIT. It still
        // belongs to the already bounded typed mutations, so keep both native
        // ownership scopes active while finalizing the transaction.
        if let Err(error) =
            self.with_write_scope(|store| self.with_transaction_control(|_| store.commit_batch()))
        {
            self.store.poison_native_path_group();
            let _ = self.with_transaction_control(|store| store.rollback_batch());
            self.clear_owner();
            self.finished = true;
            return Err(error);
        }
        self.clear_owner();
        self.finished = true;
        Ok(receipt)
    }

    pub fn rollback(mut self) -> Result<()> {
        if let Err(error) = self.ensure_open() {
            self.rollback_internal();
            return Err(error);
        }
        let result = self.with_transaction_control(|store| store.rollback_batch());
        self.clear_owner();
        self.finished = true;
        result
    }

    fn execute_typed_mutation<T>(
        &mut self,
        encoded_bytes: Result<usize>,
        operation: impl FnOnce(&Store) -> Result<T>,
    ) -> Result<T> {
        self.ensure_mutable()?;
        let encoded_bytes = match encoded_bytes {
            Ok(encoded_bytes) => encoded_bytes,
            Err(error) => return self.poison_with(error),
        };
        self.charge_core_mutations(1, encoded_bytes)?;
        match self.with_write_scope(operation) {
            Ok(value) => Ok(value),
            Err(error) => self.poison_with(error),
        }
    }

    fn ensure_open(&mut self) -> Result<()> {
        if self.finished
            || self.store.native_path_group_token.get() != Some(self.token)
            || self.store.event_search_bulk_depth.load(Ordering::SeqCst) != 1
        {
            self.store.poison_native_path_group();
            return Err(StoreError::NativePathGroupPoisoned);
        }
        Ok(())
    }

    fn ensure_mutable(&mut self) -> Result<()> {
        self.ensure_open()?;
        if self.is_poisoned() {
            return Err(StoreError::NativePathGroupPoisoned);
        }
        if self.journal_prepared {
            return self.poison_with(StoreError::NativePathJournalSealed);
        }
        Ok(())
    }

    fn charge_core_mutations(&mut self, mutation_units: usize, encoded_bytes: usize) -> Result<()> {
        self.ensure_open()?;
        if self.is_poisoned() {
            return Err(StoreError::NativePathGroupPoisoned);
        }
        let next_units = self.attempted_mutation_units.saturating_add(mutation_units);
        let next_bytes = self.core_bound_value_bytes.saturating_add(encoded_bytes);
        if let Err(error) = validate_limit(
            "attempted Store mutation units",
            next_units,
            NATIVE_PATH_MAX_MUTATION_UNITS,
        ) {
            return self.poison_with(error);
        }
        if let Err(error) = validate_limit(
            "Core bound-value encoding bytes",
            next_bytes,
            NATIVE_PATH_MAX_CORE_BOUND_BYTES,
        ) {
            return self.poison_with(error);
        }
        self.attempted_mutation_units = next_units;
        self.core_bound_value_bytes = next_bytes;
        Ok(())
    }

    fn validate_final_accounting(&self) -> Result<()> {
        self.coordinator.validate()?;
        validate_limit(
            "attempted Store mutation units",
            self.attempted_mutation_units,
            NATIVE_PATH_MAX_MUTATION_UNITS,
        )?;
        validate_limit(
            "Core bound-value encoding bytes",
            self.core_bound_value_bytes,
            NATIVE_PATH_MAX_CORE_BOUND_BYTES,
        )?;
        validate_limit(
            "actual journal records",
            self.journal_records,
            NATIVE_PATH_MAX_JOURNAL_RECORDS,
        )?;
        validate_limit(
            "uncompressed journal encoding bytes",
            self.journal_uncompressed_bytes,
            NATIVE_PATH_MAX_JOURNAL_BYTES,
        )
    }

    fn poison_with<T>(&mut self, error: StoreError) -> Result<T> {
        self.store.poison_native_path_group();
        Err(error)
    }

    fn is_poisoned(&self) -> bool {
        self.store.native_path_group_poisoned.load(Ordering::SeqCst)
    }

    fn with_write_scope<T>(&self, operation: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        self.store
            .native_path_mutation_scope
            .store(true, Ordering::SeqCst);
        let scope = NativePathWriteScope { store: self.store };
        let result = operation(self.store);
        drop(scope);
        result
    }

    fn with_transaction_control<T>(
        &self,
        operation: impl FnOnce(&Store) -> Result<T>,
    ) -> Result<T> {
        self.store.native_path_transaction_control_scope.set(true);
        let scope = NativePathTransactionControlScope { store: self.store };
        let result = operation(self.store);
        drop(scope);
        result
    }

    fn collector_overflowed(&self) -> bool {
        self.store
            .projection_journal_group_collector
            .borrow()
            .as_ref()
            .is_some_and(|collector| collector.is_overflowed())
    }

    fn rollback_internal(&mut self) {
        let _ = self.with_transaction_control(|store| store.rollback_batch());
        self.clear_owner();
        self.finished = true;
    }

    fn clear_owner(&self) {
        self.store.projection_journal_group_collector.replace(None);
        if self.store.native_path_group_token.get() == Some(self.token) {
            self.store.native_path_group_token.set(None);
        }
        self.store.clear_native_path_authorizer();
        self.store
            .native_path_group_poisoned
            .store(false, Ordering::SeqCst);
    }
}

impl Drop for NativePathPublicationGroup<'_> {
    fn drop(&mut self) {
        if self.finished || self.store.native_path_group_token.get() != Some(self.token) {
            return;
        }
        let _ = self.with_transaction_control(|store| store.rollback_batch());
        self.clear_owner();
        self.finished = true;
    }
}

struct NativePathWriteScope<'store> {
    store: &'store Store,
}

struct NativePathTransactionControlScope<'store> {
    store: &'store Store,
}

impl Drop for NativePathTransactionControlScope<'_> {
    fn drop(&mut self) {
        self.store.native_path_transaction_control_scope.set(false);
    }
}

impl Drop for NativePathWriteScope<'_> {
    fn drop(&mut self) {
        self.store
            .native_path_mutation_scope
            .store(false, Ordering::SeqCst);
        self.store.conn.flush_prepared_statement_cache();
    }
}

#[derive(Debug)]
pub(crate) struct BoundEncoding {
    bytes: usize,
}

impl BoundEncoding {
    pub(crate) fn mutation() -> Self {
        Self {
            bytes: BOUND_MUTATION_HEADER_BYTES,
        }
    }

    pub(crate) fn null(&mut self) {
        self.bytes = self.bytes.saturating_add(BOUND_TAG_BYTES);
    }

    pub(crate) fn integer(&mut self) {
        self.bytes = self.bytes.saturating_add(BOUND_TAG_BYTES + 8);
    }

    pub(crate) fn text(&mut self, value: &str) {
        self.bytes = self
            .bytes
            .saturating_add(BOUND_TAG_BYTES)
            .saturating_add(BOUND_LENGTH_BYTES)
            .saturating_add(value.len());
    }

    pub(crate) fn optional_text(&mut self, value: Option<&str>) {
        match value {
            Some(value) => self.text(value),
            None => self.null(),
        }
    }

    pub(crate) fn optional_integer(&mut self, present: bool) {
        if present {
            self.integer();
        } else {
            self.null();
        }
    }

    pub(crate) fn finish(self) -> usize {
        self.bytes
    }
}

fn capture_source_bind_bytes(source: &CaptureSource) -> Result<usize> {
    let metadata = serde_json::to_string(&source.sync.metadata)?;
    let mut values = BoundEncoding::mutation();
    values.text(&source.id.to_string());
    values.text(source.descriptor.kind.as_str());
    values.text(source.descriptor.provider.as_str());
    values.text(&source.descriptor.machine_id);
    values.optional_integer(source.descriptor.process_id.is_some());
    values.optional_text(source.descriptor.cwd.as_deref());
    values.optional_text(source.descriptor.raw_source_path.as_deref());
    values.optional_text(source.descriptor.source_format.as_deref());
    values.optional_text(source.descriptor.source_root.as_deref());
    values.optional_text(source.descriptor.source_identity.as_deref());
    values.optional_text(source.descriptor.external_session_id.as_deref());
    values.integer();
    values.optional_integer(source.ended_at.is_some());
    values.text(source.sync.fidelity.as_str());
    values.text(source.sync.visibility.as_str());
    values.text(source.sync.sync_state.as_str());
    values.integer();
    values.text(&metadata);
    Ok(values.finish())
}

fn session_bind_bytes(session: &Session) -> Result<usize> {
    let metadata = serde_json::to_string(&session.sync.metadata)?;
    let mut values = BoundEncoding::mutation();
    values.text(&session.id.to_string());
    add_optional_uuid(&mut values, session.history_record_id);
    add_optional_uuid(&mut values, session.parent_session_id);
    add_optional_uuid(&mut values, session.root_session_id);
    add_optional_uuid(&mut values, session.capture_source_id);
    values.text(session.provider.as_str());
    values.optional_text(session.external_session_id.as_deref());
    values.optional_text(session.external_agent_id.as_deref());
    values.text(session.agent_type.as_str());
    values.optional_text(session.role_hint.as_deref());
    values.integer();
    values.text(session.status.as_str());
    values.text(session.sync.fidelity.as_str());
    add_optional_uuid(&mut values, session.transcript_blob_id);
    values.integer();
    values.optional_integer(session.ended_at.is_some());
    values.integer();
    values.integer();
    values.text(session.sync.visibility.as_str());
    values.text(session.sync.sync_state.as_str());
    values.integer();
    values.optional_integer(session.sync.deleted_at.is_some());
    values.text(&metadata);
    Ok(values.finish())
}

pub(crate) fn event_bind_bytes(event: &Event) -> Result<usize> {
    let event = durable_event(event)?;
    let payload = serde_json::to_string(&event.payload)?;
    let metadata = serde_json::to_string(&event.sync.metadata)?;
    let mut values = BoundEncoding::mutation();
    values.text(&event.id.to_string());
    values.integer();
    add_optional_uuid(&mut values, event.history_record_id);
    add_optional_uuid(&mut values, event.session_id);
    add_optional_uuid(&mut values, event.run_id);
    values.text(event.event_type.as_str());
    values.optional_text(event.role.map(|role| role.as_str()));
    values.integer();
    add_optional_uuid(&mut values, event.capture_source_id);
    values.text(&payload);
    add_optional_uuid(&mut values, event.payload_blob_id);
    values.optional_text(event.dedupe_key.as_deref());
    values.text(event.sync.visibility.as_str());
    values.text(event.sync.fidelity.as_str());
    values.text(event.sync.sync_state.as_str());
    values.integer();
    values.optional_integer(event.sync.deleted_at.is_some());
    values.text(&metadata);
    Ok(values.finish())
}

fn run_bind_bytes(run: &Run) -> Result<usize> {
    if !provider_output_run_is_retained_failure(run) {
        return Ok(0);
    }
    let metadata = serde_json::to_string(&run.sync.metadata)?;
    let mut values = BoundEncoding::mutation();
    values.text(&run.id.to_string());
    add_optional_uuid(&mut values, run.history_record_id);
    add_optional_uuid(&mut values, run.session_id);
    values.text(run.run_type.as_str());
    values.text(run.status.as_str());
    values.integer();
    values.optional_integer(run.ended_at.is_some());
    values.optional_integer(run.exit_code.is_some());
    values.optional_text(run.cwd.as_deref());
    values.optional_text(run.command_preview.as_deref());
    add_optional_uuid(&mut values, run.input_blob_id);
    add_optional_uuid(&mut values, run.output_blob_id);
    values.integer();
    values.integer();
    add_optional_uuid(&mut values, run.source_id);
    values.text(run.sync.visibility.as_str());
    values.text(run.sync.fidelity.as_str());
    values.text(run.sync.sync_state.as_str());
    values.integer();
    values.optional_integer(run.sync.deleted_at.is_some());
    values.text(&metadata);
    Ok(values.finish())
}

fn file_touch_bind_bytes(file: &FileTouched) -> Result<usize> {
    let metadata = serde_json::to_string(&file.sync.metadata)?;
    let mut values = BoundEncoding::mutation();
    values.text(&file.id.to_string());
    add_optional_uuid(&mut values, file.history_record_id);
    add_optional_uuid(&mut values, file.run_id);
    add_optional_uuid(&mut values, file.event_id);
    add_optional_uuid(&mut values, file.vcs_workspace_id);
    values.text(file.path.as_str());
    values.optional_text(file.change_kind.map(|kind| kind.as_str()));
    values.optional_text(file.old_path.as_deref());
    values.optional_integer(file.line_count_delta.is_some());
    values.text(file.confidence.as_str());
    values.integer();
    values.integer();
    add_optional_uuid(&mut values, file.source_id);
    values.text(file.sync.visibility.as_str());
    values.text(file.sync.fidelity.as_str());
    values.text(file.sync.sync_state.as_str());
    values.integer();
    values.optional_integer(file.sync.deleted_at.is_some());
    values.text(&metadata);
    Ok(values.finish())
}

fn session_edge_bind_bytes(edge: &SessionEdge) -> Result<usize> {
    let metadata = serde_json::to_string(&edge.sync.metadata)?;
    let mut values = BoundEncoding::mutation();
    values.text(&edge.id.to_string());
    values.text(&edge.from_session_id.to_string());
    values.text(&edge.to_session_id.to_string());
    values.text(edge.edge_type.as_str());
    values.text(edge.confidence.as_str());
    add_optional_uuid(&mut values, edge.source_id);
    values.integer();
    values.integer();
    values.text(edge.sync.visibility.as_str());
    values.text(edge.sync.fidelity.as_str());
    values.text(edge.sync.sync_state.as_str());
    values.integer();
    values.optional_integer(edge.sync.deleted_at.is_some());
    values.text(&metadata);
    Ok(values.finish())
}

fn add_optional_uuid(values: &mut BoundEncoding, value: Option<Uuid>) {
    match value {
        Some(value) => values.text(&value.to_string()),
        None => values.null(),
    }
}

fn encoded_cursor_cas_bytes(expected: Option<&SyncCursor>, next: &SyncCursor) -> usize {
    let mut values = BoundEncoding::mutation();
    match expected {
        Some(expected) => {
            values.text(&next.cursor);
            values.optional_integer(next.last_synced_at.is_some());
            values.integer();
            values.text(&expected.id.to_string());
            values.optional_text(expected.team_id.as_deref());
            values.text(&expected.device_id);
            values.text(&expected.stream);
            values.text(&expected.cursor);
            values.optional_integer(expected.last_synced_at.is_some());
            values.integer();
            values.integer();
        }
        None => {
            values.text(&next.id.to_string());
            values.optional_text(next.team_id.as_deref());
            values.text(&next.device_id);
            values.text(&next.stream);
            values.text(&next.cursor);
            values.optional_integer(next.last_synced_at.is_some());
            values.integer();
            values.integer();
        }
    }
    values.finish()
}

fn encode_cursor_envelope(envelope: &NativePathCommittedCursorEnvelope) -> Result<String> {
    serde_json::to_string(envelope).map_err(StoreError::from)
}

fn decode_cursor_envelope(encoded: &str) -> Result<NativePathCommittedCursorEnvelope> {
    let envelope: NativePathCommittedCursorEnvelope =
        serde_json::from_str(encoded).map_err(|_| StoreError::InvalidNativePathCursorSet)?;
    if envelope.version != NATIVE_PATH_CURSOR_ENVELOPE_VERSION || envelope.publication_id.is_empty()
    {
        return Err(StoreError::InvalidNativePathCursorSet);
    }
    Ok(envelope)
}

fn validate_limit(limit: &'static str, actual: usize, maximum: usize) -> Result<()> {
    if actual > maximum {
        return Err(StoreError::NativePathGroupLimitExceeded {
            limit,
            actual,
            maximum,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
