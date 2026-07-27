use std::{collections::BTreeSet, sync::atomic::Ordering};

use ctx_history_core::{CaptureSource, Event, FileTouched, Run, Session, SessionEdge, SyncCursor};
use rusqlite::{
    hooks::{AuthAction, AuthContext, Authorization},
    params,
};
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
mod accounting;
mod generation;
mod publication;

use accounting::{
    capture_source_bind_bytes, decode_cursor_envelope, encode_cursor_envelope,
    encoded_cursor_cas_bytes, file_touch_bind_bytes, run_bind_bytes, session_bind_bytes,
    session_edge_bind_bytes, validate_limit,
};
pub(crate) use accounting::{event_bind_bytes, BoundEncoding};

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
    published_cursors: Vec<SyncCursor>,
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

    /// Exact Store-owned cursor rows committed by this publication.
    ///
    /// Callers may thread these rows into a subsequent bounded group without
    /// reconstructing or weakening the Store's complete-envelope CAS.
    pub fn published_cursors(&self) -> &[SyncCursor] {
        &self.published_cursors
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
    Published {
        cursors: Vec<SyncCursor>,
    },
    AlreadyCommitted {
        cursors: Vec<SyncCursor>,
    },
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
    bulk_epoch: u64,
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
    pub(crate) fn begin_native_cold_load(&self) -> Result<()> {
        if self.native_cold_load_active.get()
            || !self.conn.is_autocommit()
            || self.native_path_group_token.get().is_some()
            || !self.fresh_provider_projection_eligible()?
        {
            return Err(StoreError::ColdStoreInvalidState);
        }
        self.native_cold_load_active.set(true);
        Ok(())
    }

    pub(crate) fn finish_native_cold_load(&self) -> Result<()> {
        if !self.native_cold_load_active.get()
            || !self.conn.is_autocommit()
            || self.native_path_group_token.get().is_some()
        {
            return Err(StoreError::ColdStoreInvalidState);
        }
        self.conn.flush_prepared_statement_cache();
        self.native_cold_load_active.set(false);
        Ok(())
    }

    pub(crate) fn native_cold_write_scope_active(&self) -> bool {
        self.native_cold_load_active.get()
            && self.native_path_group_token.get().is_some()
            && self.native_path_mutation_scope.load(Ordering::SeqCst)
    }

    /// Returns whether this Store is an unpublished fresh cold stage.
    ///
    /// Capture adapters use this only to defer projection-journal activation
    /// until all canonical Core rows have been loaded. Ordinary installed
    /// Stores can never enter this state.
    #[doc(hidden)]
    pub fn native_cold_load_active(&self) -> bool {
        self.native_cold_load_active.get()
    }

    /// Activates one canonical baseline after a fresh cold Core load and binds
    /// every cold-published cursor to that baseline checkpoint.
    ///
    /// The stage is still unpublished, so failure leaves no externally
    /// observable partial Store. Every cursor must be a canonical NativePath
    /// envelope with no earlier journal authority.
    #[doc(hidden)]
    pub fn activate_native_cold_projection_journal(
        &self,
        contract_fingerprint: &str,
    ) -> Result<JournalCheckpoint> {
        const MAX_COLD_CURSORS: usize = 131_072;
        if !self.native_cold_load_active.get()
            || !self.conn.is_autocommit()
            || self.native_path_group_token.get().is_some()
            || self
                .projection_journal_checkpoint_in_transaction()?
                .is_some()
        {
            return Err(StoreError::ColdStoreInvalidState);
        }

        let checkpoint = self.activate_projection_journal(contract_fingerprint)?;
        self.with_atomic_write(|| {
            let mut statement = self
                .conn
                .prepare("SELECT id, cursor FROM sync_cursors ORDER BY id LIMIT ?1")?;
            let rows = statement.query_map(
                [i64::try_from(MAX_COLD_CURSORS + 1)
                    .map_err(|_| StoreError::ColdStoreInvalidState)?],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?;
            let mut cursors = Vec::new();
            for row in rows {
                cursors.push(row?);
            }
            drop(statement);
            if cursors.len() > MAX_COLD_CURSORS {
                return Err(StoreError::ColdStoreInvalidState);
            }

            let mut update = self.conn.prepare_cached(
                "UPDATE sync_cursors SET cursor = ?1 WHERE id = ?2 AND cursor = ?3",
            )?;
            for (id, encoded) in cursors {
                let mut envelope = decode_cursor_envelope(&encoded)?;
                if envelope.journal_checkpoint.is_some() {
                    return Err(StoreError::ColdStoreInvalidState);
                }
                envelope.journal_checkpoint = Some(checkpoint.clone());
                let next = encode_cursor_envelope(&envelope)?;
                if update.execute(params![next, id, encoded])? != 1 {
                    return Err(StoreError::ColdStoreInvalidState);
                }
            }
            Ok(())
        })?;
        Ok(checkpoint)
    }

    #[doc(hidden)]
    pub fn begin_native_path_publication_group(
        &self,
        admission: EventSearchBulkGroupAdmission,
        coordinator: NativePathGroupAccounting,
    ) -> Result<NativePathPublicationGroup<'_>> {
        self.ensure_connection_usable()?;
        if self.native_path_group_token.get().is_some()
            || self.projection_journal_group_collector.borrow().is_some()
        {
            self.native_path_group_poisoned
                .store(true, Ordering::SeqCst);
            return Err(StoreError::NativePathGroupAlreadyActive);
        }
        coordinator.validate()?;
        let bulk_epoch = self.consume_event_search_bulk_group_admission(admission)?;
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
            bulk_epoch,
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
        if self.connection_quarantined.get() {
            return;
        }
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

    /// Binds a released canonical event identity to the stable native-record
    /// identity now used by a provider. The alias is exact, idempotent, and
    /// remains inside the publication transaction.
    pub fn bind_event_identity_alias(
        &mut self,
        alias_id: Uuid,
        event_id: Uuid,
        created_at_ms: i64,
    ) -> Result<()> {
        const REASON: &str = "native-record-identity-v1";
        self.execute_typed_mutation(
            Ok(2 * std::mem::size_of::<Uuid>() + REASON.len() + std::mem::size_of::<i64>()),
            |store| store.bind_event_identity_alias(alias_id, event_id, REASON, created_at_ms),
        )
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
            self.rollback_internal()?;
            return Err(error);
        }
        if self.source_retirement_preview.is_some()
            && matches!(self.cursor_state, CursorPublicationState::Published { .. })
        {
            self.store.poison_native_path_group();
        }
        if !matches!(
            self.cursor_state,
            CursorPublicationState::Published { .. }
                | CursorPublicationState::AlreadyCommitted { .. }
        ) {
            self.store.poison_native_path_group();
        }
        if self.is_poisoned() || self.collector_overflowed() {
            self.rollback_internal()?;
            return Err(StoreError::NativePathGroupPoisoned);
        }
        if let Err(error) = self.prepare_journal_checkpoint() {
            self.rollback_internal()?;
            return Err(error);
        }
        if let Err(error) = self.validate_final_accounting() {
            self.store.poison_native_path_group();
            self.rollback_internal()?;
            return Err(error);
        }
        let published_cursors = match &self.cursor_state {
            CursorPublicationState::Published { cursors }
            | CursorPublicationState::AlreadyCommitted { cursors } => cursors.clone(),
            CursorPublicationState::None | CursorPublicationState::Expected { .. } => {
                self.store.poison_native_path_group();
                self.rollback_internal()?;
                return Err(StoreError::NativePathGroupPoisoned);
            }
        };
        let receipt = NativePathGroupReceipt {
            coordinator: self.coordinator,
            attempted_mutation_units: self.attempted_mutation_units,
            core_bound_value_bytes: self.core_bound_value_bytes,
            journal_records: self.journal_records,
            journal_uncompressed_bytes: self.journal_uncompressed_bytes,
            checkpoint: self.checkpoint.clone(),
            published_cursors,
        };
        // SQLite can defer canonical virtual-table work until COMMIT. It still
        // belongs to the already bounded typed mutations, so keep both native
        // ownership scopes active while finalizing the transaction.
        if let Err(error) =
            self.with_write_scope(|store| self.with_transaction_control(|_| store.commit_batch()))
        {
            self.store.poison_native_path_group();
            if self.rollback_transaction().is_err() {
                self.finished = true;
                return Err(StoreError::StoreConnectionQuarantined);
            }
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
            self.rollback_internal()?;
            return Err(error);
        }
        self.rollback_transaction()?;
        self.clear_owner();
        self.finished = true;
        Ok(())
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
            || self.store.event_search_bulk_depth.load(Ordering::SeqCst) == 0
            || self.store.event_search_bulk_epoch.load(Ordering::SeqCst) != self.bulk_epoch
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

    fn rollback_transaction(&self) -> Result<()> {
        if self
            .with_transaction_control(|store| store.rollback_batch())
            .is_err()
        {
            self.store.quarantine_connection();
            return Err(StoreError::StoreConnectionQuarantined);
        }
        Ok(())
    }

    fn rollback_internal(&mut self) -> Result<()> {
        if let Err(error) = self.rollback_transaction() {
            self.finished = true;
            return Err(error);
        }
        self.clear_owner();
        self.finished = true;
        Ok(())
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
        if self.rollback_transaction().is_err() {
            self.finished = true;
            return;
        }
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
        // Leaving the scope deliberately retains the statements it prepared.
        // Discarding them here would re-parse every canonical INSERT, search
        // projection, and journal statement once per typed mutation. The
        // authorizer stays enforced because `Store::with_atomic_write` flushes
        // the cache before any out-of-route mutation, and every uncached
        // `Connection::execute` re-prepares unconditionally.
    }
}

#[cfg(test)]
mod tests;
