use super::*;
use ctx_merge_queue::MergeQueueRuntime;
use ctx_session_runtime::runtime::SessionRuntime;
use ctx_store::manager::WorkspaceStoreAccessOutcome;
use ctx_store::StoreManager;
use std::sync::Weak;
use std::time::Duration;

const STORE_OPEN_RETRY_LIMIT: usize = 3;
const STORE_OPEN_RETRY_BASE_MS: u64 = 40;

#[derive(Debug)]
pub enum SessionStoreAccessError {
    NotFound,
    LookupUnavailable(anyhow::Error),
    StoreUnavailable,
}

#[derive(Debug)]
pub enum WorkspaceStoreAccessError {
    NotFound,
    Unavailable(anyhow::Error),
}

#[derive(Clone)]
pub(in crate::daemon) struct ProtectedWorkspaceStoreLookup {
    stores: StoreManager,
    sessions: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    merge_queue: Arc<MergeQueueRuntime>,
}

impl ProtectedWorkspaceStoreLookup {
    pub(in crate::daemon) fn new(
        stores: StoreManager,
        sessions: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
        merge_queue: Arc<MergeQueueRuntime>,
    ) -> Self {
        Self {
            stores,
            sessions,
            merge_queue,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        self.stores.global()
    }

    pub(in crate::daemon) async fn store_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Store> {
        match self.lookup_workspace_store(workspace_id).await {
            StoreLookup::Found(store) => Ok(store),
            StoreLookup::Missing | StoreLookup::Deleting => {
                anyhow::bail!("workspace {} not found", workspace_id.0)
            }
            StoreLookup::Unavailable(err) => Err(err),
        }
    }

    pub(in crate::daemon) async fn lookup_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> StoreLookup {
        match self.stores.workspace_access_outcome(workspace_id).await {
            Ok(WorkspaceStoreAccessOutcome::Access(access)) => {
                if access.kind.triggers_open_side_effects() {
                    let mut protected = self.protected_workspace_store_ids().await;
                    protected.insert(workspace_id);
                    self.stores.evict_workspaces_to_cap(&protected).await;
                }
                StoreLookup::Found(access.store)
            }
            Ok(WorkspaceStoreAccessOutcome::Missing) => StoreLookup::Missing,
            Ok(WorkspaceStoreAccessOutcome::Deleting) => StoreLookup::Deleting,
            Err(err) => StoreLookup::Unavailable(err),
        }
    }

    pub(in crate::daemon) async fn store_for_worktree(
        &self,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<Store> {
        let workspace_id = self
            .global_store()
            .get_workspace_id_for_worktree(worktree_id)
            .await?
            .with_context(|| format!("workspace missing for worktree {}", worktree_id.0))?;
        self.store_for_workspace(workspace_id).await
    }

    pub(in crate::daemon) async fn store_for_task(&self, task_id: TaskId) -> anyhow::Result<Store> {
        let workspace_id = self
            .global_store()
            .get_workspace_id_for_task(task_id)
            .await?
            .with_context(|| format!("workspace missing for task {}", task_id.0))?;
        self.store_for_workspace(workspace_id).await
    }

    async fn protected_workspace_store_ids(&self) -> HashSet<WorkspaceId> {
        let mut active_sessions: HashSet<SessionId> = HashSet::new();
        {
            let set = self.sessions.running_sessions.lock().await;
            active_sessions.extend(set.iter().copied());
        }
        {
            let map = self.sessions.schedulers.lock().await;
            active_sessions.extend(map.keys().copied());
        }
        {
            let map = self.sessions.broadcasters.lock().await;
            active_sessions.extend(map.keys().copied());
        }
        {
            let map = self.sessions.session_event_heads.lock().await;
            active_sessions.extend(map.keys().copied());
        }

        let mut active_workspaces: HashSet<WorkspaceId> = HashSet::new();
        let mut missing = Vec::new();
        {
            let cache = self.sessions.session_meta_cache.lock().await;
            for session_id in &active_sessions {
                if let Some(entry) = cache.get(session_id) {
                    active_workspaces.insert(entry.value.workspace_id);
                } else {
                    missing.push(*session_id);
                }
            }
        }
        for session_id in missing {
            if let Ok(Some(workspace_id)) = self
                .stores
                .global()
                .get_workspace_id_for_session(session_id)
                .await
            {
                active_workspaces.insert(workspace_id);
            }
        }
        active_workspaces.extend(self.merge_queue.running_workspaces().await);
        active_workspaces
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, WorkspaceStoreAccessError> {
        match self.lookup_workspace_store(workspace_id).await {
            StoreLookup::Found(store) => Ok(store),
            StoreLookup::Missing | StoreLookup::Deleting => {
                Err(WorkspaceStoreAccessError::NotFound)
            }
            StoreLookup::Unavailable(err) => Err(WorkspaceStoreAccessError::Unavailable(err)),
        }
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct SessionStoreLookup {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
}

#[derive(Clone)]
pub(in crate::daemon) struct WeakSessionStoreLookup {
    global_store: Store,
    stores: StoreManager,
    sessions: Weak<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    merge_queue: Arc<MergeQueueRuntime>,
}

impl WeakSessionStoreLookup {
    pub(in crate::daemon) fn new(
        global_store: Store,
        stores: StoreManager,
        sessions: Weak<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
        merge_queue: Arc<MergeQueueRuntime>,
    ) -> Self {
        Self {
            global_store,
            stores,
            sessions,
            merge_queue,
        }
    }

    pub(in crate::daemon) fn upgraded_lookups(
        &self,
    ) -> Option<(SessionStoreLookup, ProtectedWorkspaceStoreLookup)> {
        let sessions = self.sessions.upgrade()?;
        let workspace_stores = ProtectedWorkspaceStoreLookup::new(
            self.stores.clone(),
            sessions,
            Arc::clone(&self.merge_queue),
        );
        let session_stores =
            SessionStoreLookup::new(self.global_store.clone(), workspace_stores.clone());
        Some((session_stores, workspace_stores))
    }

    pub(in crate::daemon) async fn existing_session_store_allow_archived(
        &self,
        session_id: SessionId,
    ) -> Result<Option<Store>, SessionStoreAccessError> {
        let Some((session_stores, _workspace_stores)) = self.upgraded_lookups() else {
            return Ok(None);
        };
        session_stores
            .existing_session_store_allow_archived(session_id)
            .await
            .map(Some)
    }
}

impl SessionStoreLookup {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
        }
    }

    async fn workspace_id_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<WorkspaceId>, SessionStoreAccessError> {
        self.global_store
            .get_workspace_id_for_session(session_id)
            .await
            .map_err(SessionStoreAccessError::LookupUnavailable)
    }

    pub(in crate::daemon) async fn lookup_session_store(
        &self,
        session_id: SessionId,
    ) -> StoreLookup {
        let workspace_id = match self.workspace_id_for_session(session_id).await {
            Ok(Some(workspace_id)) => workspace_id,
            Ok(None) => return StoreLookup::Missing,
            Err(SessionStoreAccessError::LookupUnavailable(error)) => {
                return StoreLookup::Unavailable(error);
            }
            Err(SessionStoreAccessError::NotFound) => return StoreLookup::Missing,
            Err(SessionStoreAccessError::StoreUnavailable) => {
                return StoreLookup::Unavailable(anyhow::anyhow!("workspace store unavailable"));
            }
        };
        match self
            .workspace_stores
            .lookup_workspace_store(workspace_id)
            .await
        {
            StoreLookup::Found(store) => StoreLookup::Found(store),
            StoreLookup::Missing | StoreLookup::Deleting => StoreLookup::Deleting,
            StoreLookup::Unavailable(error) => StoreLookup::Unavailable(error),
        }
    }

    pub(in crate::daemon) async fn existing_session_store_allow_archived(
        &self,
        session_id: SessionId,
    ) -> Result<Store, SessionStoreAccessError> {
        let Some(workspace_id) = self.workspace_id_for_session(session_id).await? else {
            return Err(SessionStoreAccessError::NotFound);
        };
        match self
            .workspace_stores
            .existing_workspace_store(workspace_id)
            .await
        {
            Ok(store) => Ok(store),
            Err(WorkspaceStoreAccessError::NotFound) => Err(SessionStoreAccessError::NotFound),
            Err(WorkspaceStoreAccessError::Unavailable(error)) => {
                Err(SessionStoreAccessError::LookupUnavailable(error))
            }
        }
    }

    pub(in crate::daemon) async fn store_for_session(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Store> {
        match self.lookup_session_store(session_id).await {
            StoreLookup::Found(store) => Ok(store),
            StoreLookup::Missing | StoreLookup::Deleting => {
                anyhow::bail!("workspace missing for session {}", session_id.0)
            }
            StoreLookup::Unavailable(error) => Err(error),
        }
    }

    pub(in crate::daemon) async fn existing_session_store(
        &self,
        session_id: SessionId,
    ) -> Result<Store, SessionStoreAccessError> {
        let store = self
            .existing_session_store_allow_archived(session_id)
            .await?;
        reject_archived_subagent_session(&store, session_id).await?;
        Ok(store)
    }

    pub(in crate::daemon) async fn existing_session_store_for_write(
        &self,
        session_id: SessionId,
    ) -> Result<Store, SessionStoreAccessError> {
        let mut attempt = 0usize;
        loop {
            let workspace_id = match self.workspace_id_for_session(session_id).await {
                Ok(Some(workspace_id)) => workspace_id,
                Ok(None) => return Err(SessionStoreAccessError::NotFound),
                Err(SessionStoreAccessError::LookupUnavailable(error)) => {
                    if is_transient_store_open_error(&error) && attempt < STORE_OPEN_RETRY_LIMIT {
                        attempt += 1;
                        let backoff_ms = STORE_OPEN_RETRY_BASE_MS.saturating_mul(attempt as u64);
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        continue;
                    }
                    tracing::warn!(
                        session_id = %session_id.0,
                        "session store lookup failed: {error:#}"
                    );
                    return Err(SessionStoreAccessError::StoreUnavailable);
                }
                Err(error) => return Err(error),
            };
            match self
                .workspace_stores
                .existing_workspace_store(workspace_id)
                .await
            {
                Ok(store) => {
                    reject_archived_subagent_session(&store, session_id).await?;
                    return Ok(store);
                }
                Err(WorkspaceStoreAccessError::NotFound) => {
                    return Err(SessionStoreAccessError::NotFound);
                }
                Err(WorkspaceStoreAccessError::Unavailable(error)) => {
                    if is_transient_store_open_error(&error) && attempt < STORE_OPEN_RETRY_LIMIT {
                        attempt += 1;
                        let backoff_ms = STORE_OPEN_RETRY_BASE_MS.saturating_mul(attempt as u64);
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        continue;
                    }
                    tracing::warn!(
                        session_id = %session_id.0,
                        "session store lookup failed: {error:#}"
                    );
                    return Err(SessionStoreAccessError::StoreUnavailable);
                }
            }
        }
    }

    pub(in crate::daemon) async fn require_scoped_mcp_session_context(
        &self,
        mcp_auth: ctx_mcp_auth::McpAuthContext,
        session_id: SessionId,
    ) -> Result<(), crate::daemon::ScopedMcpSessionAccessError> {
        if mcp_auth.session_id != session_id {
            return Err(crate::daemon::ScopedMcpSessionAccessError::Unauthorized(
                "scoped ctx-mcp token is limited to the current session",
            ));
        }

        let store = self
            .existing_session_store(session_id)
            .await
            .map_err(scoped_mcp_session_store_error)?;
        let session = store
            .get_session(session_id)
            .await
            .map_err(crate::daemon::ScopedMcpSessionAccessError::StoreUnavailable)?
            .ok_or(crate::daemon::ScopedMcpSessionAccessError::SessionNotFound)?;

        if session.workspace_id != mcp_auth.workspace_id
            || session.worktree_id != mcp_auth.worktree_id
        {
            return Err(crate::daemon::ScopedMcpSessionAccessError::Unauthorized(
                "scoped ctx-mcp token does not match the loaded session scope",
            ));
        }

        Ok(())
    }
}

pub(in crate::daemon) fn session_store_access_anyhow(
    error: SessionStoreAccessError,
) -> anyhow::Error {
    match error {
        SessionStoreAccessError::NotFound => anyhow::anyhow!("session not found"),
        SessionStoreAccessError::LookupUnavailable(error) => error,
        SessionStoreAccessError::StoreUnavailable => {
            anyhow::anyhow!("workspace store unavailable")
        }
    }
}

fn scoped_mcp_session_store_error(
    error: SessionStoreAccessError,
) -> crate::daemon::ScopedMcpSessionAccessError {
    match error {
        SessionStoreAccessError::NotFound => {
            crate::daemon::ScopedMcpSessionAccessError::SessionNotFound
        }
        SessionStoreAccessError::LookupUnavailable(error) => {
            crate::daemon::ScopedMcpSessionAccessError::StoreUnavailable(error)
        }
        SessionStoreAccessError::StoreUnavailable => {
            crate::daemon::ScopedMcpSessionAccessError::StoreUnavailable(anyhow::anyhow!(
                "workspace store unavailable"
            ))
        }
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct TaskStoreLookup {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
}

impl TaskStoreLookup {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
        }
    }

    pub(in crate::daemon) async fn task_store_or_none(
        &self,
        task_id: TaskId,
    ) -> anyhow::Result<Option<Store>> {
        let Some(workspace_id) = self.global_store.get_workspace_id_for_task(task_id).await? else {
            return Ok(None);
        };
        match self
            .workspace_stores
            .existing_workspace_store(workspace_id)
            .await
        {
            Ok(store) => Ok(Some(store)),
            Err(WorkspaceStoreAccessError::NotFound) => Ok(None),
            Err(WorkspaceStoreAccessError::Unavailable(error)) => Err(error),
        }
    }
}

impl DaemonState {
    pub fn global_store(&self) -> &Store {
        self.core.stores.global()
    }

    pub(in crate::daemon::state) async fn protected_workspace_store_ids(
        &self,
    ) -> HashSet<WorkspaceId> {
        let mut active_sessions: HashSet<SessionId> = HashSet::new();
        {
            let set = self.sessions.running_sessions.lock().await;
            active_sessions.extend(set.iter().copied());
        }
        {
            let map = self.sessions.schedulers.lock().await;
            active_sessions.extend(map.keys().copied());
        }
        {
            let map = self.sessions.broadcasters.lock().await;
            active_sessions.extend(map.keys().copied());
        }
        {
            let map = self.sessions.session_event_heads.lock().await;
            active_sessions.extend(map.keys().copied());
        }

        let mut active_workspaces: HashSet<WorkspaceId> = HashSet::new();
        let mut missing = Vec::new();
        {
            let cache = self.sessions.session_meta_cache.lock().await;
            for session_id in &active_sessions {
                if let Some(entry) = cache.get(session_id) {
                    active_workspaces.insert(entry.value.workspace_id);
                } else {
                    missing.push(*session_id);
                }
            }
        }
        for session_id in missing {
            if let Ok(Some(workspace_id)) = self
                .global_store()
                .get_workspace_id_for_session(session_id)
                .await
            {
                active_workspaces.insert(workspace_id);
            }
        }
        active_workspaces.extend(self.transport.merge_queue.running_workspaces().await);

        active_workspaces
    }

    pub async fn store_for_workspace(&self, workspace_id: WorkspaceId) -> Result<Store> {
        match self.lookup_workspace_store(workspace_id).await {
            StoreLookup::Found(store) => Ok(store),
            StoreLookup::Missing | StoreLookup::Deleting => {
                anyhow::bail!("workspace {} not found", workspace_id.0)
            }
            StoreLookup::Unavailable(err) => Err(err),
        }
    }

    pub async fn lookup_workspace_store(&self, workspace_id: WorkspaceId) -> StoreLookup {
        match self
            .core
            .stores
            .workspace_access_outcome(workspace_id)
            .await
        {
            Ok(ctx_store::manager::WorkspaceStoreAccessOutcome::Access(access)) => {
                if access.kind.triggers_open_side_effects() {
                    let mut protected_workspaces = self.protected_workspace_store_ids().await;
                    protected_workspaces.insert(workspace_id);
                    self.core
                        .stores
                        .evict_workspaces_to_cap(&protected_workspaces)
                        .await;
                }
                StoreLookup::Found(access.store)
            }
            Ok(ctx_store::manager::WorkspaceStoreAccessOutcome::Missing) => StoreLookup::Missing,
            Ok(ctx_store::manager::WorkspaceStoreAccessOutcome::Deleting) => StoreLookup::Deleting,
            Err(err) => StoreLookup::Unavailable(err),
        }
    }

    pub async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, WorkspaceStoreAccessError> {
        match self.lookup_workspace_store(workspace_id).await {
            StoreLookup::Found(store) => Ok(store),
            StoreLookup::Missing | StoreLookup::Deleting => {
                Err(WorkspaceStoreAccessError::NotFound)
            }
            StoreLookup::Unavailable(err) => Err(WorkspaceStoreAccessError::Unavailable(err)),
        }
    }

    pub async fn lookup_session_store(&self, session_id: SessionId) -> StoreLookup {
        let workspace_id = match self
            .global_store()
            .get_workspace_id_for_session(session_id)
            .await
        {
            Ok(Some(workspace_id)) => workspace_id,
            Ok(None) => return StoreLookup::Missing,
            Err(err) => return StoreLookup::Unavailable(err),
        };
        match self.lookup_workspace_store(workspace_id).await {
            StoreLookup::Found(store) => StoreLookup::Found(store),
            StoreLookup::Missing | StoreLookup::Deleting => StoreLookup::Deleting,
            StoreLookup::Unavailable(err) => StoreLookup::Unavailable(err),
        }
    }

    pub async fn existing_session_store_allow_archived(
        &self,
        session_id: SessionId,
    ) -> Result<Store, SessionStoreAccessError> {
        match self.lookup_session_store(session_id).await {
            StoreLookup::Found(store) => Ok(store),
            StoreLookup::Missing | StoreLookup::Deleting => Err(SessionStoreAccessError::NotFound),
            StoreLookup::Unavailable(err) => Err(SessionStoreAccessError::LookupUnavailable(err)),
        }
    }

    pub async fn existing_session_store(
        &self,
        session_id: SessionId,
    ) -> Result<Store, SessionStoreAccessError> {
        let store = self
            .existing_session_store_allow_archived(session_id)
            .await?;
        reject_archived_subagent_session(&store, session_id).await?;
        Ok(store)
    }

    pub async fn existing_session_store_for_write(
        &self,
        session_id: SessionId,
    ) -> Result<Store, SessionStoreAccessError> {
        let store = self
            .existing_session_store_allow_archived_for_write(session_id)
            .await?;
        reject_archived_subagent_session(&store, session_id).await?;
        Ok(store)
    }

    async fn existing_session_store_allow_archived_for_write(
        &self,
        session_id: SessionId,
    ) -> Result<Store, SessionStoreAccessError> {
        let mut attempt = 0usize;
        loop {
            match self.lookup_session_store(session_id).await {
                StoreLookup::Found(store) => return Ok(store),
                StoreLookup::Missing | StoreLookup::Deleting => {
                    return Err(SessionStoreAccessError::NotFound);
                }
                StoreLookup::Unavailable(err) => {
                    if is_transient_store_open_error(&err) && attempt < STORE_OPEN_RETRY_LIMIT {
                        attempt += 1;
                        let backoff_ms = STORE_OPEN_RETRY_BASE_MS.saturating_mul(attempt as u64);
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        continue;
                    }
                    tracing::warn!(
                        session_id = %session_id.0,
                        "session store lookup failed: {err:#}"
                    );
                    return Err(SessionStoreAccessError::StoreUnavailable);
                }
            }
        }
    }

    pub async fn store_for_task(&self, task_id: TaskId) -> Result<Store> {
        let workspace_id = self
            .global_store()
            .get_workspace_id_for_task(task_id)
            .await?
            .with_context(|| format!("workspace missing for task {}", task_id.0))?;
        self.store_for_workspace(workspace_id).await
    }

    pub async fn store_for_session(&self, session_id: SessionId) -> Result<Store> {
        match self.lookup_session_store(session_id).await {
            StoreLookup::Found(store) => Ok(store),
            StoreLookup::Missing | StoreLookup::Deleting => {
                anyhow::bail!("workspace missing for session {}", session_id.0)
            }
            StoreLookup::Unavailable(err) => Err(err),
        }
    }

    pub async fn store_for_worktree(&self, worktree_id: WorktreeId) -> Result<Store> {
        let workspace_id = self
            .global_store()
            .get_workspace_id_for_worktree(worktree_id)
            .await?
            .with_context(|| format!("workspace missing for worktree {}", worktree_id.0))?;
        self.store_for_workspace(workspace_id).await
    }
}

async fn reject_archived_subagent_session(
    store: &Store,
    session_id: SessionId,
) -> Result<(), SessionStoreAccessError> {
    if store
        .is_archived_subagent_session(session_id)
        .await
        .map_err(|_| SessionStoreAccessError::StoreUnavailable)?
    {
        return Err(SessionStoreAccessError::NotFound);
    }
    Ok(())
}

fn is_transient_store_open_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("database is locked")
        || msg.contains("sqlite_busy")
        || msg.contains("database is busy")
}
