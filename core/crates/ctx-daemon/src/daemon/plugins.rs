use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use ctx_core::models::{
    PluginCommandContribution, PluginContributionRegistration, PluginDiagnostic,
    PluginDiagnosticSeverity, PluginEnablement, PluginEntrypoint, PluginEntrypointKind,
    PluginExtensionRegistry, PluginInventoryItem, PluginLoadStatus, PluginManifest,
    PluginProviderContribution,
};
use ctx_provider_runtime::ProviderRuntime;
use ctx_providers::adapters::{
    ProviderAdapter, ProviderHealth, ProviderProcessInfo, ProviderRecommendedAction,
    ProviderRestartMode, ProviderRunHooks, ProviderSessionSweepConfig, ProviderSessionSweepStats,
    ProviderStatus, ProviderUsability, ProviderUsabilityStatus, RunHandle, TurnInput,
};
use ctx_providers::crp::Tier1CrpAdapter;
use ctx_providers::events::NormalizedEvent;
use ctx_route_contracts::plugins::{
    PluginCommandExecutionRouteRequest, PluginCommandExecutionRouteResponse,
    PluginCommandInvocationPayload, PluginExtensionRegistryRouteResponse,
    PluginInventoryRouteResponse,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock};

pub const PLUGIN_MANIFEST_FILE_NAMES: &[&str] = &["ctx-plugin.json", "plugin.json"];
const PLUGIN_COMMAND_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30);
const PLUGIN_COMMAND_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
const PLUGIN_AUTO_RELOAD_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct PluginInventorySnapshot {
    pub revision: i64,
    pub roots: Vec<PathBuf>,
    pub plugins: Vec<PluginInventoryItem>,
}

impl PluginInventorySnapshot {
    fn empty(roots: Vec<PathBuf>) -> Self {
        Self {
            revision: 0,
            roots,
            plugins: Vec::new(),
        }
    }

    fn into_route_response(self) -> PluginInventoryRouteResponse {
        PluginInventoryRouteResponse::new(
            self.revision,
            self.roots
                .into_iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect(),
            self.plugins,
        )
    }

    fn extension_registry(&self) -> PluginExtensionRegistry {
        let mut registry = PluginExtensionRegistry {
            revision: self.revision,
            ..PluginExtensionRegistry::default()
        };
        for plugin in self.plugins.iter().filter(|plugin| {
            plugin.enabled == PluginEnablement::Enabled && plugin.status == PluginLoadStatus::Loaded
        }) {
            let Some(manifest) = plugin.manifest.as_ref() else {
                continue;
            };
            registry.providers.extend(
                manifest
                    .contributes
                    .providers
                    .iter()
                    .cloned()
                    .map(|contribution| register_plugin_contribution(plugin, contribution)),
            );
            registry.runtimes.extend(
                manifest
                    .contributes
                    .runtimes
                    .iter()
                    .cloned()
                    .map(|contribution| register_plugin_contribution(plugin, contribution)),
            );
            registry.commands.extend(
                manifest
                    .contributes
                    .commands
                    .iter()
                    .cloned()
                    .map(|contribution| register_plugin_contribution(plugin, contribution)),
            );
            registry.collectors.extend(
                manifest
                    .contributes
                    .collectors
                    .iter()
                    .cloned()
                    .map(|contribution| register_plugin_contribution(plugin, contribution)),
            );
            registry.observers.extend(
                manifest
                    .contributes
                    .observers
                    .iter()
                    .cloned()
                    .map(|contribution| register_plugin_contribution(plugin, contribution)),
            );
            registry.ui_surfaces.extend(
                manifest
                    .contributes
                    .ui_surfaces
                    .iter()
                    .cloned()
                    .map(|contribution| register_plugin_contribution(plugin, contribution)),
            );
        }
        registry
    }
}

#[derive(Debug)]
struct PluginInventoryState {
    snapshot: PluginInventorySnapshot,
    next_revision: i64,
}

#[derive(Debug)]
pub struct PluginInventoryRuntime {
    roots: Vec<PathBuf>,
    state: RwLock<PluginInventoryState>,
    reload_lock: Mutex<()>,
    auto_reload_check: Mutex<Option<Instant>>,
    provider_adapter_sync: Mutex<PluginProviderAdapterSyncState>,
    auto_reload_interval: Duration,
}

#[derive(Debug, Default)]
struct PluginProviderAdapterSyncState {
    revision: i64,
    ownership: BTreeMap<String, PluginProviderAdapterOwnership>,
}

#[derive(Clone)]
struct PluginProviderAdapterOwnership {
    adapter: Arc<dyn ProviderAdapter>,
    plugin_id: String,
    plugin_revision: Option<String>,
}

impl std::fmt::Debug for PluginProviderAdapterOwnership {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginProviderAdapterOwnership")
            .field("plugin_id", &self.plugin_id)
            .field("plugin_revision", &self.plugin_revision)
            .finish_non_exhaustive()
    }
}

impl PluginInventoryRuntime {
    pub fn new(data_root: PathBuf) -> Self {
        let roots = plugin_roots_from_env()
            .filter(|roots| !roots.is_empty())
            .unwrap_or_else(|| vec![data_root.join("plugins")]);
        Self::new_with_roots(roots)
    }

    pub fn new_with_roots(roots: Vec<PathBuf>) -> Self {
        Self::new_with_roots_and_auto_reload_interval(roots, PLUGIN_AUTO_RELOAD_INTERVAL)
    }

    fn new_with_roots_and_auto_reload_interval(
        roots: Vec<PathBuf>,
        auto_reload_interval: Duration,
    ) -> Self {
        let roots = normalize_plugin_roots(roots);
        Self {
            state: RwLock::new(PluginInventoryState {
                snapshot: PluginInventorySnapshot::empty(roots.clone()),
                next_revision: 1,
            }),
            reload_lock: Mutex::new(()),
            auto_reload_check: Mutex::new(None),
            provider_adapter_sync: Mutex::new(PluginProviderAdapterSyncState::default()),
            auto_reload_interval,
            roots,
        }
    }

    async fn ensure_loaded(&self) {
        {
            let state = self.state.read().await;
            if state.snapshot.revision != 0 {
                return;
            }
        }

        let _reload_guard = self.reload_lock.lock().await;
        {
            let state = self.state.read().await;
            if state.snapshot.revision != 0 {
                return;
            }
        }
        let _ = self.reload_locked().await;
    }

    pub async fn snapshot(&self) -> PluginInventoryRouteResponse {
        self.ensure_loaded().await;
        self.reload_if_changed().await;

        let state = self.state.read().await;
        state.snapshot.clone().into_route_response()
    }

    pub async fn extension_registry(&self) -> PluginExtensionRegistryRouteResponse {
        self.ensure_loaded().await;
        self.reload_if_changed().await;

        let state = self.state.read().await;
        PluginExtensionRegistryRouteResponse::new(state.snapshot.extension_registry())
    }

    pub async fn reload(&self) -> anyhow::Result<PluginInventoryRouteResponse> {
        let _reload_guard = self.reload_lock.lock().await;
        self.reload_locked().await
    }

    pub async fn execute_command(
        &self,
        request: PluginCommandExecutionRouteRequest,
    ) -> PluginCommandExecutionRouteResponse {
        self.ensure_loaded().await;
        self.reload_if_changed().await;

        let resolved = match self.resolve_command(&request).await {
            Ok(resolved) => resolved,
            Err(error) => {
                return PluginCommandExecutionRouteResponse::failed(
                    request.plugin_id,
                    request.command_id,
                    error.to_string(),
                    String::new(),
                    String::new(),
                    None,
                );
            }
        };

        execute_resolved_plugin_command(request, resolved).await
    }

    pub async fn sync_provider_adapters(&self, providers: &ProviderRuntime) {
        self.ensure_loaded().await;
        self.reload_if_changed().await;

        let snapshot = {
            let state = self.state.read().await;
            state.snapshot.clone()
        };

        let mut sync_state = self.provider_adapter_sync.lock().await;
        if sync_state.revision == snapshot.revision {
            return;
        }

        let previous_ownership = sync_state.ownership.clone();
        let mut next_ownership = BTreeMap::new();
        for registration in plugin_provider_registrations(&snapshot) {
            let provider_id = registration.contribution.id.trim().to_string();
            if provider_id.is_empty() {
                continue;
            }
            let previous = previous_ownership.get(&provider_id);
            if provider_adapter_conflicts_with_plugin_ownership(providers, &provider_id, previous)
                .await
            {
                tracing::warn!(
                    plugin_id = %registration.plugin_id,
                    provider_id,
                    "plugin provider contribution conflicts with a provider adapter owned by another source"
                );
                continue;
            }

            let (adapter, status) = plugin_provider_adapter_for_registration(&registration).await;
            providers
                .upsert_provider_adapter(provider_id.clone(), Arc::clone(&adapter))
                .await;
            providers
                .upsert_provider_status(provider_id.clone(), status)
                .await;
            next_ownership.insert(
                provider_id,
                PluginProviderAdapterOwnership {
                    adapter,
                    plugin_id: registration.plugin_id.clone(),
                    plugin_revision: registration.plugin_revision.clone(),
                },
            );
        }

        for (provider_id, ownership) in previous_ownership
            .iter()
            .filter(|(provider_id, _)| !next_ownership.contains_key(*provider_id))
        {
            let _ = providers
                .remove_provider_adapter_if_same(provider_id, &ownership.adapter)
                .await;
            remove_plugin_provider_status_if_owned(providers, provider_id, ownership).await;
        }

        sync_state.revision = snapshot.revision;
        sync_state.ownership = next_ownership;
    }

    async fn reload_locked(&self) -> anyhow::Result<PluginInventoryRouteResponse> {
        let mut scanned = self.scan_roots().await?;

        let mut state = self.state.write().await;
        scanned.revision = state.next_revision;
        state.next_revision += 1;
        state.snapshot = scanned.clone();
        *self.auto_reload_check.lock().await = Some(Instant::now());
        Ok(scanned.into_route_response())
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    async fn resolve_command(
        &self,
        request: &PluginCommandExecutionRouteRequest,
    ) -> anyhow::Result<ResolvedPluginCommand> {
        let plugin_id = request.plugin_id.trim();
        let command_id = request.command_id.trim();
        if plugin_id.is_empty() {
            anyhow::bail!("plugin_id is required");
        }
        if command_id.is_empty() {
            anyhow::bail!("command_id is required");
        }

        let state = self.state.read().await;
        let plugin = state
            .snapshot
            .plugins
            .iter()
            .find(|plugin| plugin.id == plugin_id)
            .ok_or_else(|| anyhow::anyhow!("plugin '{plugin_id}' is not installed"))?;
        if plugin.enabled != PluginEnablement::Enabled {
            anyhow::bail!("plugin '{plugin_id}' is disabled");
        }
        if plugin.status != PluginLoadStatus::Loaded {
            anyhow::bail!("plugin '{plugin_id}' is not loaded");
        }
        let manifest = plugin
            .manifest
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("plugin '{plugin_id}' did not load a manifest"))?;
        let command = manifest
            .contributes
            .commands
            .iter()
            .find(|command| command.id == command_id)
            .ok_or_else(|| {
                anyhow::anyhow!("plugin '{plugin_id}' does not declare command '{command_id}'")
            })?;
        let entrypoint_id = command.entrypoint.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "plugin command '{plugin_id}:{command_id}' does not declare an entrypoint"
            )
        })?;
        let entrypoint = manifest
            .entrypoints
            .iter()
            .find(|entrypoint| entrypoint.id == entrypoint_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "plugin command '{plugin_id}:{command_id}' references missing entrypoint '{entrypoint_id}'"
                )
            })?;
        let manifest_path = PathBuf::from(&plugin.path);
        let plugin_root = manifest_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("plugin '{plugin_id}' has no manifest directory"))?
            .to_path_buf();
        let cwd = resolve_plugin_cwd(&plugin_root, entrypoint.cwd.as_deref())?;
        let entrypoint_command =
            resolve_plugin_entrypoint_command(&plugin_root, &entrypoint.command)?;

        Ok(ResolvedPluginCommand {
            plugin_root,
            manifest_path,
            plugin_revision: plugin.revision.clone(),
            command: command.clone(),
            entrypoint: entrypoint.clone(),
            entrypoint_command,
            cwd,
        })
    }

    async fn reload_if_changed(&self) {
        let now = Instant::now();
        {
            let mut last_check = self.auto_reload_check.lock().await;
            if last_check.is_some_and(|checked_at| {
                now.duration_since(checked_at) < self.auto_reload_interval
            }) {
                return;
            }
            *last_check = Some(now);
        }

        let Ok(_reload_guard) = self.reload_lock.try_lock() else {
            return;
        };
        let Ok(mut scanned) = self.scan_roots().await else {
            return;
        };
        let mut state = self.state.write().await;
        if plugin_inventory_signature(&state.snapshot) == plugin_inventory_signature(&scanned) {
            return;
        }
        scanned.revision = state.next_revision;
        state.next_revision += 1;
        state.snapshot = scanned;
    }

    async fn scan_roots(&self) -> anyhow::Result<PluginInventorySnapshot> {
        let roots = self.roots.clone();
        tokio::task::spawn_blocking(move || scan_plugin_roots(&roots))
            .await
            .map_err(|error| anyhow::anyhow!("plugin inventory scan task failed: {error}"))
    }
}

#[derive(Debug, Clone)]
struct PluginProviderRegistration {
    plugin_id: String,
    plugin_name: String,
    plugin_version: String,
    plugin_path: String,
    plugin_revision: Option<String>,
    manifest: PluginManifest,
    contribution: PluginProviderContribution,
}

fn plugin_provider_registrations(
    snapshot: &PluginInventorySnapshot,
) -> Vec<PluginProviderRegistration> {
    let mut registrations = Vec::new();
    for plugin in snapshot.plugins.iter().filter(|plugin| {
        plugin.enabled == PluginEnablement::Enabled && plugin.status == PluginLoadStatus::Loaded
    }) {
        let Some(manifest) = plugin.manifest.as_ref() else {
            continue;
        };
        registrations.extend(
            manifest
                .contributes
                .providers
                .iter()
                .cloned()
                .map(|contribution| PluginProviderRegistration {
                    plugin_id: plugin.id.clone(),
                    plugin_name: plugin.name.clone(),
                    plugin_version: plugin.version.clone(),
                    plugin_path: plugin.path.clone(),
                    plugin_revision: plugin.revision.clone(),
                    manifest: manifest.clone(),
                    contribution,
                }),
        );
    }
    registrations
}

async fn provider_adapter_conflicts_with_plugin_ownership(
    providers: &ProviderRuntime,
    provider_id: &str,
    previous: Option<&PluginProviderAdapterOwnership>,
) -> bool {
    let Some(current) = providers.provider_adapter(provider_id).await else {
        return false;
    };
    previous
        .map(|ownership| !Arc::ptr_eq(&current, &ownership.adapter))
        .unwrap_or(true)
}

async fn remove_plugin_provider_status_if_owned(
    providers: &ProviderRuntime,
    provider_id: &str,
    ownership: &PluginProviderAdapterOwnership,
) {
    let Some(status) = providers.provider_status(provider_id).await else {
        return;
    };
    if plugin_provider_status_matches_ownership(&status, ownership) {
        providers.remove_provider_status(provider_id).await;
    }
}

fn plugin_provider_status_matches_ownership(
    status: &ProviderStatus,
    ownership: &PluginProviderAdapterOwnership,
) -> bool {
    if status.details.get("plugin_provider").map(String::as_str) != Some("true") {
        return false;
    }
    if status.details.get("plugin_id").map(String::as_str) != Some(ownership.plugin_id.as_str()) {
        return false;
    }
    match ownership.plugin_revision.as_deref() {
        Some(revision) => {
            status.details.get("plugin_revision").map(String::as_str) == Some(revision)
        }
        None => !status.details.contains_key("plugin_revision"),
    }
}

async fn plugin_provider_adapter_for_registration(
    registration: &PluginProviderRegistration,
) -> (Arc<dyn ProviderAdapter>, ProviderStatus) {
    let adapter: Arc<dyn ProviderAdapter> = match resolve_plugin_provider(registration) {
        Ok(resolved) => Arc::new(PluginProviderAdapter {
            inner: Arc::new(Tier1CrpAdapter::from_provider_runtime_with_spawn_options(
                &registration.contribution.id,
                resolved.command,
                resolved.args,
                Some(resolved.cwd),
                resolved.env,
            )),
            metadata: PluginProviderMetadata::from_registration(registration),
        }),
        Err(error) => Arc::new(PluginProviderStatusAdapter {
            status: plugin_provider_blocked_status(registration, error.to_string()),
        }),
    };
    let status = adapter.inspect().await.unwrap_or_else(|error| {
        plugin_provider_blocked_status(
            registration,
            format!("plugin provider inspect failed: {error}"),
        )
    });
    (adapter, status)
}

#[derive(Debug)]
struct ResolvedPluginProvider {
    command: String,
    args: Vec<String>,
    cwd: PathBuf,
    env: HashMap<String, String>,
}

fn resolve_plugin_provider(
    registration: &PluginProviderRegistration,
) -> anyhow::Result<ResolvedPluginProvider> {
    let manifest_path = PathBuf::from(&registration.plugin_path);
    let plugin_root = manifest_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("plugin has no manifest directory"))?
        .to_path_buf();
    let entrypoint_id = registration
        .contribution
        .entrypoint
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("plugin provider does not declare an entrypoint"))?;
    let entrypoint = registration
        .manifest
        .entrypoints
        .iter()
        .find(|entrypoint| entrypoint.id == entrypoint_id)
        .ok_or_else(|| {
            anyhow::anyhow!("plugin provider references missing entrypoint '{entrypoint_id}'")
        })?;
    if entrypoint.kind != PluginEntrypointKind::Process {
        anyhow::bail!("plugin provider execution only supports process entrypoints");
    }

    let cwd = resolve_plugin_cwd(&plugin_root, entrypoint.cwd.as_deref())?;
    let command = resolve_plugin_entrypoint_command(&plugin_root, &entrypoint.command)?;
    let mut env = entrypoint
        .environment
        .clone()
        .into_iter()
        .collect::<HashMap<_, _>>();
    env.insert("CTX_PLUGIN_ID".to_string(), registration.plugin_id.clone());
    env.insert(
        "CTX_PLUGIN_PROVIDER_ID".to_string(),
        registration.contribution.id.clone(),
    );
    env.insert(
        "CTX_PLUGIN_CONTRIBUTION_ID".to_string(),
        registration.contribution.id.clone(),
    );
    env.insert("CTX_PLUGIN_PROVIDER_TARGET".to_string(), "host".to_string());
    env.insert(
        "CTX_PLUGIN_ROOT".to_string(),
        plugin_root.to_string_lossy().to_string(),
    );
    env.insert(
        "CTX_PLUGIN_MANIFEST".to_string(),
        manifest_path.to_string_lossy().to_string(),
    );
    if let Some(revision) = registration.plugin_revision.as_deref() {
        env.insert("CTX_PLUGIN_REVISION".to_string(), revision.to_string());
    }

    Ok(ResolvedPluginProvider {
        command,
        args: entrypoint.args.clone(),
        cwd,
        env,
    })
}

fn resolve_plugin_entrypoint_command(plugin_root: &Path, raw: &str) -> anyhow::Result<String> {
    let command = raw.trim();
    if command.is_empty() {
        anyhow::bail!("plugin entrypoint command is required");
    }
    let path = Path::new(command);
    if path.is_absolute() {
        return Ok(command.to_string());
    }
    if command.contains('/') || command.contains('\\') {
        if path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) || command.split(['/', '\\']).any(|segment| segment == "..")
        {
            anyhow::bail!("plugin entrypoint command cannot escape the plugin root");
        }
        return Ok(plugin_root.join(path).to_string_lossy().to_string());
    }
    Ok(command.to_string())
}

#[derive(Debug, Clone)]
struct PluginProviderMetadata {
    plugin_id: String,
    plugin_name: String,
    plugin_version: String,
    plugin_path: String,
    plugin_revision: Option<String>,
    provider_name: String,
}

impl PluginProviderMetadata {
    fn from_registration(registration: &PluginProviderRegistration) -> Self {
        Self {
            plugin_id: registration.plugin_id.clone(),
            plugin_name: registration.plugin_name.clone(),
            plugin_version: registration.plugin_version.clone(),
            plugin_path: registration.plugin_path.clone(),
            plugin_revision: registration.plugin_revision.clone(),
            provider_name: registration.contribution.name.clone(),
        }
    }

    fn decorate_status(&self, status: &mut ProviderStatus) {
        status
            .details
            .insert("plugin_provider".to_string(), "true".to_string());
        status
            .details
            .insert("plugin_provider_target".to_string(), "host".to_string());
        status
            .details
            .insert("plugin_id".to_string(), self.plugin_id.clone());
        status
            .details
            .insert("plugin_name".to_string(), self.plugin_name.clone());
        status
            .details
            .insert("plugin_version".to_string(), self.plugin_version.clone());
        status
            .details
            .insert("plugin_path".to_string(), self.plugin_path.clone());
        status.details.insert(
            "plugin_provider_name".to_string(),
            self.provider_name.clone(),
        );
        if let Some(revision) = self.plugin_revision.as_deref() {
            status
                .details
                .insert("plugin_revision".to_string(), revision.to_string());
        }
        if status.version.is_none() {
            status.version = Some(self.plugin_version.clone());
        }
    }
}

struct PluginProviderAdapter {
    inner: Arc<dyn ProviderAdapter>,
    metadata: PluginProviderMetadata,
}

#[async_trait]
impl ProviderAdapter for PluginProviderAdapter {
    async fn inspect(&self) -> anyhow::Result<ProviderStatus> {
        let mut status = self.inner.inspect().await?;
        self.metadata.decorate_status(&mut status);
        Ok(status)
    }

    async fn run(
        &self,
        input: TurnInput,
        workdir: PathBuf,
        env: HashMap<String, String>,
        event_sink: tokio::sync::mpsc::Sender<NormalizedEvent>,
        hooks: ProviderRunHooks,
    ) -> anyhow::Result<RunHandle> {
        let env = plugin_provider_host_env(env);
        self.inner.run(input, workdir, env, event_sink, hooks).await
    }

    async fn cancel(&self, handle: &mut RunHandle) -> anyhow::Result<()> {
        self.inner.cancel(handle).await
    }

    async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
        self.inner.list_processes().await
    }

    async fn restart(&self, reason: &str, mode: ProviderRestartMode) -> anyhow::Result<()> {
        self.inner.restart(reason, mode).await
    }

    fn supports_restart_mode(&self, mode: ProviderRestartMode) -> bool {
        self.inner.supports_restart_mode(mode)
    }

    async fn has_live_session(&self, session_key: &str) -> bool {
        self.inner.has_live_session(session_key).await
    }

    fn supports_resume(&self) -> bool {
        self.inner.supports_resume()
    }

    async fn set_session_pinned(&self, session_key: String, pinned: bool) -> anyhow::Result<()> {
        self.inner.set_session_pinned(session_key, pinned).await
    }

    async fn set_session_model(&self, session_key: String, model_id: String) -> anyhow::Result<()> {
        self.inner.set_session_model(session_key, model_id).await
    }

    async fn set_session_mode(&self, session_key: String, mode_id: String) -> anyhow::Result<()> {
        self.inner.set_session_mode(session_key, mode_id).await
    }

    async fn authenticate_session(
        &self,
        session_key: String,
        workdir: PathBuf,
        env: HashMap<String, String>,
        method_id: Option<String>,
        event_sink: tokio::sync::mpsc::Sender<NormalizedEvent>,
        hooks: ProviderRunHooks,
    ) -> anyhow::Result<()> {
        let env = plugin_provider_host_env(env);
        self.inner
            .authenticate_session(session_key, workdir, env, method_id, event_sink, hooks)
            .await
    }

    async fn reap_idle_sessions(
        &self,
        config: ProviderSessionSweepConfig,
    ) -> anyhow::Result<ProviderSessionSweepStats> {
        self.inner.reap_idle_sessions(config).await
    }
}

fn plugin_provider_host_env(mut env: HashMap<String, String>) -> HashMap<String, String> {
    env.retain(|key, _| !is_plugin_provider_container_env_key(key));
    env
}

fn is_plugin_provider_container_env_key(key: &str) -> bool {
    matches!(
        key,
        "CTX_HARNESS_RUNTIME_KIND"
            | "CTX_HARNESS_HOST_WORKTREE_ROOT"
            | "CTX_HARNESS_GUEST_WORKTREE_ROOT"
            | "CTX_HARNESS_GUEST_WORKSPACE_ROOT"
            | "CTX_HARNESS_SANDBOX_CLI_PATH"
    ) || key.starts_with("CTX_HARNESS_CONTAINER_")
        || key.starts_with("CTX_AVF_")
}

struct PluginProviderStatusAdapter {
    status: ProviderStatus,
}

#[async_trait]
impl ProviderAdapter for PluginProviderStatusAdapter {
    async fn inspect(&self) -> anyhow::Result<ProviderStatus> {
        Ok(self.status.clone())
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: tokio::sync::mpsc::Sender<NormalizedEvent>,
        _hooks: ProviderRunHooks,
    ) -> anyhow::Result<RunHandle> {
        let message = self
            .status
            .diagnostics
            .first()
            .cloned()
            .unwrap_or_else(|| "plugin provider is unavailable".to_string());
        anyhow::bail!("{message}");
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> anyhow::Result<()> {
        Ok(())
    }
}

fn plugin_provider_blocked_status(
    registration: &PluginProviderRegistration,
    message: String,
) -> ProviderStatus {
    let mut details = HashMap::new();
    details.insert("plugin_provider".to_string(), "true".to_string());
    details.insert("plugin_provider_target".to_string(), "host".to_string());
    details.insert("plugin_provider_error".to_string(), "true".to_string());
    details.insert("plugin_id".to_string(), registration.plugin_id.clone());
    details.insert("plugin_name".to_string(), registration.plugin_name.clone());
    details.insert(
        "plugin_version".to_string(),
        registration.plugin_version.clone(),
    );
    details.insert("plugin_path".to_string(), registration.plugin_path.clone());
    details.insert(
        "plugin_provider_name".to_string(),
        registration.contribution.name.clone(),
    );
    if let Some(revision) = registration.plugin_revision.as_deref() {
        details.insert("plugin_revision".to_string(), revision.to_string());
    }
    ProviderStatus {
        provider_id: registration.contribution.id.clone(),
        installed: false,
        detected_path: None,
        version: Some(registration.plugin_version.clone()),
        capabilities: None,
        health: ProviderHealth::Error,
        diagnostics: vec![message.clone()],
        details,
        usability: ProviderUsability {
            usable: false,
            status: ProviderUsabilityStatus::Blocked,
            reason_code: Some("plugin_provider_invalid".to_string()),
            reason: Some(message),
            blocking_provider_ids: Vec::new(),
            recommended_action: ProviderRecommendedAction::ConfigureRuntime,
        },
    }
}

#[derive(Debug, Clone)]
struct ResolvedPluginCommand {
    plugin_root: PathBuf,
    manifest_path: PathBuf,
    plugin_revision: Option<String>,
    command: PluginCommandContribution,
    entrypoint: PluginEntrypoint,
    entrypoint_command: String,
    cwd: PathBuf,
}

#[derive(Clone)]
pub struct PluginInventoryHandle {
    runtime: Arc<PluginInventoryRuntime>,
}

impl PluginInventoryHandle {
    pub(in crate::daemon) fn new(runtime: Arc<PluginInventoryRuntime>) -> Self {
        Self { runtime }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn new_for_test(runtime: Arc<PluginInventoryRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn plugin_inventory_for_route(&self) -> PluginInventoryRouteResponse {
        self.runtime.snapshot().await
    }

    pub async fn plugin_extension_registry_for_route(
        &self,
    ) -> PluginExtensionRegistryRouteResponse {
        self.runtime.extension_registry().await
    }

    pub async fn reload_plugins_for_route(&self) -> anyhow::Result<PluginInventoryRouteResponse> {
        self.runtime.reload().await
    }

    pub async fn execute_plugin_command_for_route(
        &self,
        request: PluginCommandExecutionRouteRequest,
    ) -> PluginCommandExecutionRouteResponse {
        self.runtime.execute_command(request).await
    }
}

async fn execute_resolved_plugin_command(
    request: PluginCommandExecutionRouteRequest,
    resolved: ResolvedPluginCommand,
) -> PluginCommandExecutionRouteResponse {
    if resolved.entrypoint.kind != PluginEntrypointKind::Process {
        return PluginCommandExecutionRouteResponse::failed(
            request.plugin_id,
            request.command_id,
            "plugin command execution only supports process entrypoints",
            String::new(),
            String::new(),
            None,
        );
    }

    match run_process_plugin_command(&request, &resolved).await {
        Ok(response) => response,
        Err(error) => PluginCommandExecutionRouteResponse::failed(
            request.plugin_id,
            request.command_id,
            error.to_string(),
            String::new(),
            String::new(),
            None,
        ),
    }
}

async fn run_process_plugin_command(
    request: &PluginCommandExecutionRouteRequest,
    resolved: &ResolvedPluginCommand,
) -> anyhow::Result<PluginCommandExecutionRouteResponse> {
    let payload = PluginCommandInvocationPayload::from_request(request);
    let payload_json = serde_json::to_vec(&payload)?;

    let mut command = Command::new(&resolved.entrypoint_command);
    command
        .args(&resolved.entrypoint.args)
        .current_dir(&resolved.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .envs(&resolved.entrypoint.environment)
        .env("CTX_PLUGIN_ID", &request.plugin_id)
        .env("CTX_PLUGIN_COMMAND_ID", &request.command_id)
        .env("CTX_PLUGIN_CONTRIBUTION_ID", &resolved.command.id)
        .env("CTX_PLUGIN_ROOT", &resolved.plugin_root)
        .env("CTX_PLUGIN_MANIFEST", &resolved.manifest_path);
    if let Some(revision) = resolved.plugin_revision.as_deref() {
        command.env("CTX_PLUGIN_REVISION", revision);
    }

    let mut child = command.spawn().map_err(|error| {
        anyhow::anyhow!(
            "failed to spawn plugin command '{}:{}': {error}",
            request.plugin_id,
            request.command_id
        )
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&payload_json).await?;
        stdin.write_all(b"\n").await?;
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("plugin command stdout pipe unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("plugin command stderr pipe unavailable"))?;
    let mut stdout_task = tokio::spawn(read_capped_plugin_output(
        stdout,
        PLUGIN_COMMAND_OUTPUT_LIMIT_BYTES,
    ));
    let mut stderr_task = tokio::spawn(read_capped_plugin_output(
        stderr,
        PLUGIN_COMMAND_OUTPUT_LIMIT_BYTES,
    ));
    let mut timeout = Box::pin(tokio::time::sleep(PLUGIN_COMMAND_EXECUTION_TIMEOUT));
    let mut status = None;
    let mut stdout_output = None;
    let mut stderr_output = None;
    let mut failure = None;

    while status.is_none() || stdout_output.is_none() || stderr_output.is_none() {
        tokio::select! {
            result = &mut stdout_task, if stdout_output.is_none() => {
                let output = plugin_output_task_result(result, "stdout")?;
                if output.truncated && failure.is_none() {
                    failure = Some(format!(
                        "plugin command stdout exceeded {PLUGIN_COMMAND_OUTPUT_LIMIT_BYTES} bytes"
                    ));
                    let _ = child.start_kill();
                }
                stdout_output = Some(output);
            }
            result = &mut stderr_task, if stderr_output.is_none() => {
                let output = plugin_output_task_result(result, "stderr")?;
                if output.truncated && failure.is_none() {
                    failure = Some(format!(
                        "plugin command stderr exceeded {PLUGIN_COMMAND_OUTPUT_LIMIT_BYTES} bytes"
                    ));
                    let _ = child.start_kill();
                }
                stderr_output = Some(output);
            }
            wait_result = child.wait(), if status.is_none() => {
                status = Some(wait_result?);
            }
            _ = &mut timeout, if failure.is_none() => {
                failure = Some("plugin command timed out".to_string());
                let _ = child.start_kill();
            }
        }
    }

    let stdout = stdout_output
        .map(|output| output.into_string())
        .unwrap_or_default();
    let stderr = stderr_output
        .map(|output| output.into_string())
        .unwrap_or_default();
    let status = status.ok_or_else(|| anyhow::anyhow!("plugin command exited without status"))?;
    let exit_code = status.code();
    if let Some(error) = failure {
        return Ok(PluginCommandExecutionRouteResponse::failed(
            request.plugin_id.clone(),
            request.command_id.clone(),
            error,
            stdout,
            stderr,
            exit_code,
        ));
    }
    if !status.success() {
        return Ok(PluginCommandExecutionRouteResponse::failed(
            request.plugin_id.clone(),
            request.command_id.clone(),
            format!("plugin command exited with status {exit_code:?}"),
            stdout,
            stderr,
            exit_code,
        ));
    }

    let message = plugin_command_message_from_stdout(&stdout);
    Ok(PluginCommandExecutionRouteResponse::completed(
        request.plugin_id.clone(),
        request.command_id.clone(),
        message,
        stdout,
        stderr,
        exit_code,
    ))
}

#[derive(Debug)]
struct CappedPluginOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

impl CappedPluginOutput {
    fn into_string(self) -> String {
        String::from_utf8_lossy(&self.bytes).to_string()
    }
}

async fn read_capped_plugin_output<R>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<CappedPluginOutput>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(CappedPluginOutput {
                bytes,
                truncated: false,
            });
        }
        let remaining = limit.saturating_sub(bytes.len());
        if remaining > 0 {
            bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        if read > remaining {
            return Ok(CappedPluginOutput {
                bytes,
                truncated: true,
            });
        }
    }
}

fn plugin_output_task_result(
    result: Result<std::io::Result<CappedPluginOutput>, tokio::task::JoinError>,
    stream: &str,
) -> anyhow::Result<CappedPluginOutput> {
    match result {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(anyhow::anyhow!(
            "failed to read plugin command {stream}: {error}"
        )),
        Err(error) => Err(anyhow::anyhow!(
            "plugin command {stream} reader failed: {error}"
        )),
    }
}

fn plugin_command_message_from_stdout(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(message) = value.get("message").and_then(|message| message.as_str()) {
            let message = message.trim();
            return (!message.is_empty()).then(|| message.to_string());
        }
    }
    Some(trimmed.to_string())
}

fn resolve_plugin_cwd(plugin_root: &Path, cwd: Option<&str>) -> anyhow::Result<PathBuf> {
    let Some(cwd) = cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) else {
        return Ok(plugin_root.to_path_buf());
    };
    let path = Path::new(cwd);
    if path.is_absolute() {
        anyhow::bail!("plugin entrypoint cwd must be relative to the plugin root");
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        anyhow::bail!("plugin entrypoint cwd cannot escape the plugin root");
    }
    Ok(plugin_root.join(path))
}

fn register_plugin_contribution<T>(
    plugin: &PluginInventoryItem,
    contribution: T,
) -> PluginContributionRegistration<T> {
    PluginContributionRegistration {
        plugin_id: plugin.id.clone(),
        plugin_name: plugin.name.clone(),
        plugin_version: plugin.version.clone(),
        plugin_path: plugin.path.clone(),
        plugin_revision: plugin.revision.clone(),
        contribution,
    }
}

fn plugin_roots_from_env() -> Option<Vec<PathBuf>> {
    std::env::var_os("CTX_PLUGIN_ROOTS").map(|raw| std::env::split_paths(&raw).collect())
}

fn normalize_plugin_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut by_key = BTreeMap::new();
    for root in roots {
        let key = root.to_string_lossy().to_string();
        if !key.trim().is_empty() {
            by_key.entry(key).or_insert(root);
        }
    }
    by_key.into_values().collect()
}

fn scan_plugin_roots(roots: &[PathBuf]) -> PluginInventorySnapshot {
    let mut plugins = Vec::new();
    for root in roots {
        scan_plugin_root(root, &mut plugins);
    }
    plugins.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.path.cmp(&right.path))
    });
    PluginInventorySnapshot {
        revision: 0,
        roots: roots.to_vec(),
        plugins,
    }
}

fn plugin_inventory_signature(snapshot: &PluginInventorySnapshot) -> Vec<String> {
    snapshot
        .plugins
        .iter()
        .map(|plugin| {
            format!(
                "{}\0{}\0{}\0{:?}\0{:?}",
                plugin.id,
                plugin.path,
                plugin.revision.as_deref().unwrap_or_default(),
                plugin.enabled,
                plugin.status
            )
        })
        .collect()
}

fn scan_plugin_root(root: &Path, plugins: &mut Vec<PluginInventoryItem>) {
    if let Some(manifest_path) = manifest_path_for_plugin_dir(root) {
        plugins.push(load_plugin_manifest_item(&manifest_path));
    }

    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(manifest_path) = manifest_path_for_plugin_dir(&path) {
            plugins.push(load_plugin_manifest_item(&manifest_path));
        }
    }
}

fn manifest_path_for_plugin_dir(dir: &Path) -> Option<PathBuf> {
    PLUGIN_MANIFEST_FILE_NAMES
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

fn load_plugin_manifest_item(path: &Path) -> PluginInventoryItem {
    let loaded_at = Utc::now();
    let fallback_id = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("unknown-plugin")
        .to_string();
    let fallback_name = fallback_id.clone();
    let path_string = path.to_string_lossy().to_string();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return error_item(
                fallback_id,
                fallback_name,
                path_string,
                "manifest_read_failed",
                format!("Failed to read plugin manifest: {error}"),
                None,
                Some(loaded_at),
            );
        }
    };
    let revision = Some(hex_digest(&bytes));
    let manifest = match serde_json::from_slice::<PluginManifest>(&bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            return error_item(
                fallback_id,
                fallback_name,
                path_string,
                "manifest_parse_failed",
                format!("Failed to parse plugin manifest: {error}"),
                revision,
                Some(loaded_at),
            );
        }
    };

    let mut diagnostics = Vec::new();
    let status = match manifest.validate() {
        Ok(()) => PluginLoadStatus::Loaded,
        Err(error) => {
            diagnostics.push(PluginDiagnostic {
                severity: PluginDiagnosticSeverity::Error,
                message: format!("Invalid plugin manifest: {error:?}"),
                code: Some("manifest_validation_failed".to_string()),
            });
            PluginLoadStatus::Error
        }
    };

    PluginInventoryItem {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        enabled: PluginEnablement::Enabled,
        status,
        path: path_string,
        diagnostics,
        last_loaded_at: Some(loaded_at),
        revision,
        manifest: Some(manifest),
    }
}

fn error_item(
    id: String,
    name: String,
    path: String,
    code: &'static str,
    message: String,
    revision: Option<String>,
    last_loaded_at: Option<chrono::DateTime<Utc>>,
) -> PluginInventoryItem {
    PluginInventoryItem {
        id,
        name,
        version: "unknown".to_string(),
        enabled: PluginEnablement::Enabled,
        status: PluginLoadStatus::Error,
        path,
        diagnostics: vec![PluginDiagnostic {
            severity: PluginDiagnosticSeverity::Error,
            message,
            code: Some(code.to_string()),
        }],
        last_loaded_at,
        revision,
        manifest: None,
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_shell_command() -> &'static str {
        "/bin/sh"
    }

    #[tokio::test]
    async fn reload_discovers_valid_plugin_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": "node",
                        "args": ["dist/index.js"]
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.hello",
                            "title": "Hello",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]);

        let response = runtime.reload().await.expect("reload plugins");

        assert_eq!(response.revision, 1);
        assert_eq!(response.plugins.len(), 1);
        let plugin = &response.plugins[0];
        assert_eq!(plugin.id, "example.tools");
        assert_eq!(plugin.status, PluginLoadStatus::Loaded);
        assert!(plugin.revision.is_some());
        assert!(plugin.manifest.is_some());
    }

    #[tokio::test]
    async fn reload_reports_parse_and_validation_errors_without_failing_inventory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let invalid_dir = temp.path().join("invalid");
        let empty_dir = temp.path().join("empty");
        std::fs::create_dir_all(&invalid_dir).expect("invalid dir");
        std::fs::create_dir_all(&empty_dir).expect("empty dir");
        std::fs::write(invalid_dir.join("ctx-plugin.json"), "{not-json")
            .expect("write invalid manifest");
        std::fs::write(
            empty_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "empty.plugin",
                "name": "Empty Plugin",
                "version": "0.1.0"
            }))
            .unwrap(),
        )
        .expect("write empty manifest");
        let runtime = PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]);

        let response = runtime.reload().await.expect("reload plugins");

        assert_eq!(response.plugins.len(), 2);
        assert!(response
            .plugins
            .iter()
            .all(|plugin| plugin.status == PluginLoadStatus::Error));
        assert!(response.plugins.iter().any(|plugin| {
            plugin
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("manifest_parse_failed"))
        }));
        assert!(response.plugins.iter().any(|plugin| {
            plugin
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("manifest_validation_failed"))
        }));
    }

    #[tokio::test]
    async fn snapshot_loads_initial_inventory_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": "node",
                        "args": ["dist/index.js"]
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.hello",
                            "title": "Hello",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]);

        let first = runtime.snapshot().await;
        let second = runtime.snapshot().await;

        assert_eq!(first.revision, 1);
        assert_eq!(second.revision, 1);
        assert_eq!(first.plugins.len(), 1);
        assert_eq!(second.plugins.len(), 1);
    }

    #[tokio::test]
    async fn extension_registry_auto_reloads_when_manifest_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        let manifest_path = plugin_dir.join("ctx-plugin.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": test_shell_command(),
                        "args": ["-c", "printf '{\"message\":\"hello\"}'"]
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.hello",
                            "title": "Hello",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots_and_auto_reload_interval(
            vec![temp.path().to_path_buf()],
            Duration::ZERO,
        );

        let first = runtime.extension_registry().await.registry;
        assert_eq!(first.revision, 1);
        assert_eq!(first.commands[0].contribution.id, "example.hello");

        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.1",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": test_shell_command(),
                        "args": ["-c", "printf '{\"message\":\"goodbye\"}'"]
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.goodbye",
                            "title": "Goodbye",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("rewrite manifest");

        let second = runtime.extension_registry().await.registry;
        assert_eq!(second.revision, 2);
        assert_eq!(second.commands[0].contribution.id, "example.goodbye");
    }

    #[tokio::test]
    async fn concurrent_initial_inventory_routes_share_one_revision() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": "node",
                        "args": ["dist/index.js"]
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.hello",
                            "title": "Hello",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]);

        let (inventory, extensions) =
            tokio::join!(runtime.snapshot(), runtime.extension_registry());

        assert_eq!(inventory.revision, 1);
        assert_eq!(extensions.registry.revision, 1);
        assert_eq!(inventory.plugins.len(), 1);
        assert_eq!(extensions.registry.commands.len(), 1);
    }

    #[tokio::test]
    async fn extension_registry_projects_only_loaded_plugin_contributions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let valid_dir = temp.path().join("valid");
        let invalid_dir = temp.path().join("invalid");
        std::fs::create_dir_all(&valid_dir).expect("valid dir");
        std::fs::create_dir_all(&invalid_dir).expect("invalid dir");
        std::fs::write(
            valid_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": "node",
                        "args": ["dist/index.js"]
                    }
                ],
                "contributes": {
                    "providers": [
                        {
                            "id": "example-provider",
                            "name": "Example Provider",
                            "entrypoint": "main",
                            "capabilities": ["agent.runtime"]
                        }
                    ],
                    "commands": [
                        {
                            "id": "example.hello",
                            "title": "Hello",
                            "entrypoint": "main"
                        }
                    ],
                    "ui_surfaces": [
                        {
                            "id": "example.status",
                            "name": "Example Status",
                            "surface": "status_bar",
                            "entrypoint": "main",
                            "contexts": ["workspace"]
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write valid manifest");
        std::fs::write(
            invalid_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "broken.tools",
                "name": "Broken Tools",
                "version": "0.1.0"
            }))
            .unwrap(),
        )
        .expect("write invalid manifest");
        let runtime = PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]);

        let inventory = runtime.reload().await.expect("reload plugins");
        let registry = runtime.extension_registry().await.registry;

        assert_eq!(inventory.plugins.len(), 2);
        assert_eq!(registry.revision, 1);
        assert_eq!(registry.providers.len(), 1);
        assert_eq!(
            registry.providers[0].plugin_id, "example.tools",
            "registry should not expose invalid plugin contributions"
        );
        assert_eq!(registry.providers[0].contribution.id, "example-provider");
        assert_eq!(registry.commands.len(), 1);
        assert_eq!(registry.commands[0].contribution.id, "example.hello");
        assert_eq!(registry.ui_surfaces.len(), 1);
        assert_eq!(registry.ui_surfaces[0].contribution.id, "example.status");
    }

    #[tokio::test]
    async fn sync_provider_adapters_registers_loaded_plugin_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        let provider_command = std::env::current_exe()
            .expect("current exe")
            .to_string_lossy()
            .to_string();
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": provider_command,
                        "args": ["--plugin-provider-test"],
                        "environment": {
                            "EXAMPLE_PLUGIN_ENV": "present"
                        }
                    }
                ],
                "contributes": {
                    "providers": [
                        {
                            "id": "example-provider",
                            "name": "Example Provider",
                            "entrypoint": "main",
                            "capabilities": ["agent.runtime"]
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]);
        let providers = ProviderRuntime::new(HashMap::new());

        runtime.sync_provider_adapters(&providers).await;

        assert!(providers.has_provider_adapter("example-provider").await);
        assert!(
            providers
                .can_create_loaded_session_for_provider("example-provider")
                .await
        );
        let status = providers
            .provider_status("example-provider")
            .await
            .expect("provider status");
        assert_eq!(status.provider_id, "example-provider");
        assert!(status.installed);
        assert_eq!(status.health, ProviderHealth::Ok);
        assert_eq!(
            status.details.get("plugin_provider").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            status.details.get("plugin_id").map(String::as_str),
            Some("example.tools")
        );
        assert_eq!(
            status
                .details
                .get("plugin_provider_name")
                .map(String::as_str),
            Some("Example Provider")
        );
    }

    #[tokio::test]
    async fn sync_provider_adapters_marks_invalid_plugin_provider_blocked() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "contributes": {
                    "providers": [
                        {
                            "id": "example-provider",
                            "name": "Example Provider",
                            "capabilities": ["agent.runtime"]
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]);
        let providers = ProviderRuntime::new(HashMap::new());

        runtime.sync_provider_adapters(&providers).await;

        let status = providers
            .provider_status("example-provider")
            .await
            .expect("provider status");
        assert!(!status.installed);
        assert_eq!(status.health, ProviderHealth::Error);
        assert_eq!(status.usability.status, ProviderUsabilityStatus::Blocked);
        assert!(status
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("does not declare an entrypoint")));
    }

    #[tokio::test]
    async fn sync_provider_adapters_auto_reloads_removed_provider_contribution() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        let manifest_path = plugin_dir.join("ctx-plugin.json");
        let provider_command = std::env::current_exe()
            .expect("current exe")
            .to_string_lossy()
            .to_string();
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": provider_command.clone()
                    }
                ],
                "contributes": {
                    "providers": [
                        {
                            "id": "example-provider",
                            "name": "Example Provider",
                            "entrypoint": "main",
                            "capabilities": ["agent.runtime"]
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots_and_auto_reload_interval(
            vec![temp.path().to_path_buf()],
            Duration::ZERO,
        );
        let providers = ProviderRuntime::new(HashMap::new());

        runtime.sync_provider_adapters(&providers).await;
        assert!(providers.has_provider_adapter("example-provider").await);
        assert!(providers.has_provider_status("example-provider").await);

        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.1",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": provider_command
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.noop",
                            "title": "No-op",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("rewrite manifest");

        runtime.sync_provider_adapters(&providers).await;

        assert!(!providers.has_provider_adapter("example-provider").await);
        assert!(!providers.has_provider_status("example-provider").await);
    }

    #[tokio::test]
    async fn sync_provider_adapters_does_not_claim_conflicting_existing_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        let manifest_path = plugin_dir.join("ctx-plugin.json");
        let provider_command = std::env::current_exe()
            .expect("current exe")
            .to_string_lossy()
            .to_string();
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": provider_command.clone()
                    }
                ],
                "contributes": {
                    "providers": [
                        {
                            "id": "example-provider",
                            "name": "Example Provider",
                            "entrypoint": "main",
                            "capabilities": ["agent.runtime"]
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots_and_auto_reload_interval(
            vec![temp.path().to_path_buf()],
            Duration::ZERO,
        );
        let providers = ProviderRuntime::new(HashMap::new());
        providers
            .upsert_provider_adapter(
                "example-provider".to_string(),
                Arc::new(Tier1CrpAdapter::from_raw(
                    "example-provider",
                    provider_command.clone(),
                    Vec::new(),
                )),
            )
            .await;

        runtime.sync_provider_adapters(&providers).await;
        assert!(providers.has_provider_adapter("example-provider").await);
        assert!(!providers.has_provider_status("example-provider").await);

        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.1",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": provider_command.clone()
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.noop",
                            "title": "No-op",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("rewrite manifest");

        runtime.sync_provider_adapters(&providers).await;

        assert!(
            providers.has_provider_adapter("example-provider").await,
            "conflicted built-in adapter should not be removed by plugin reload"
        );
        assert!(!providers.has_provider_status("example-provider").await);
    }

    #[tokio::test]
    async fn sync_provider_adapters_does_not_remove_replaced_plugin_owned_adapter() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        let manifest_path = plugin_dir.join("ctx-plugin.json");
        let provider_command = std::env::current_exe()
            .expect("current exe")
            .to_string_lossy()
            .to_string();
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": provider_command.clone()
                    }
                ],
                "contributes": {
                    "providers": [
                        {
                            "id": "example-provider",
                            "name": "Example Provider",
                            "entrypoint": "main",
                            "capabilities": ["agent.runtime"]
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots_and_auto_reload_interval(
            vec![temp.path().to_path_buf()],
            Duration::ZERO,
        );
        let providers = ProviderRuntime::new(HashMap::new());

        runtime.sync_provider_adapters(&providers).await;
        assert!(providers.has_provider_adapter("example-provider").await);

        let replacement: Arc<dyn ProviderAdapter> = Arc::new(Tier1CrpAdapter::from_raw(
            "example-provider",
            provider_command.clone(),
            Vec::new(),
        ));
        providers
            .upsert_provider_adapter("example-provider".to_string(), Arc::clone(&replacement))
            .await;

        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.1",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": provider_command
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.noop",
                            "title": "No-op",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("rewrite manifest");

        runtime.sync_provider_adapters(&providers).await;

        let current = providers
            .provider_adapter("example-provider")
            .await
            .expect("replacement adapter remains");
        assert!(Arc::ptr_eq(&current, &replacement));
        assert!(!providers.has_provider_status("example-provider").await);
    }

    #[test]
    fn plugin_provider_host_env_removes_container_exec_contract() {
        let mut env = HashMap::new();
        env.insert("CTX_SESSION_ID".to_string(), "session-1".to_string());
        env.insert(
            "CTX_HARNESS_RUNTIME_KIND".to_string(),
            "shared_vm_container".to_string(),
        );
        env.insert(
            "CTX_HARNESS_CONTAINER_ID".to_string(),
            "container-1".to_string(),
        );
        env.insert(
            "CTX_HARNESS_HOST_WORKTREE_ROOT".to_string(),
            "/host/worktree".to_string(),
        );
        env.insert(
            "CTX_AVF_HOST_WORKTREE_ROOT".to_string(),
            "/host/worktree".to_string(),
        );

        let scrubbed = plugin_provider_host_env(env);

        assert_eq!(
            scrubbed.get("CTX_SESSION_ID").map(String::as_str),
            Some("session-1")
        );
        assert!(!scrubbed.contains_key("CTX_HARNESS_RUNTIME_KIND"));
        assert!(!scrubbed.contains_key("CTX_HARNESS_CONTAINER_ID"));
        assert!(!scrubbed.contains_key("CTX_HARNESS_HOST_WORKTREE_ROOT"));
        assert!(!scrubbed.contains_key("CTX_AVF_HOST_WORKTREE_ROOT"));
    }

    #[tokio::test]
    async fn execute_command_runs_process_entrypoint_with_json_payload() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": test_shell_command(),
                        "args": ["-c", "cat > payload.json; grep -q 'draft prompt' payload.json; printf '{\"message\":\"expanded prompt\"}'"],
                        "cwd": "."
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.expand",
                            "title": "Expand",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]);

        let response = runtime
            .execute_command(PluginCommandExecutionRouteRequest {
                plugin_id: "example.tools".to_string(),
                command_id: "example.expand".to_string(),
                input: Some("draft prompt".to_string()),
                workspace_id: Some("workspace-1".to_string()),
                task_id: None,
                session_id: None,
            })
            .await;

        assert_eq!(
            response.status,
            ctx_route_contracts::plugins::PluginCommandExecutionStatus::Completed
        );
        assert_eq!(response.message.as_deref(), Some("expanded prompt"));
        assert_eq!(response.exit_code, Some(0));
        assert!(plugin_dir.join("payload.json").is_file());
    }

    #[tokio::test]
    async fn execute_command_caps_process_stdout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": test_shell_command(),
                        "args": [
                            "-c",
                            "dd if=/dev/zero bs=1048577 count=1 2>/dev/null | tr '\\0' x"
                        ],
                        "cwd": "."
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.noisy",
                            "title": "Noisy",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]);

        let response = runtime
            .execute_command(PluginCommandExecutionRouteRequest {
                plugin_id: "example.tools".to_string(),
                command_id: "example.noisy".to_string(),
                input: None,
                workspace_id: None,
                task_id: None,
                session_id: None,
            })
            .await;

        assert_eq!(
            response.status,
            ctx_route_contracts::plugins::PluginCommandExecutionStatus::Failed
        );
        assert!(response
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("stdout exceeded"));
        assert_eq!(response.stdout.len(), PLUGIN_COMMAND_OUTPUT_LIMIT_BYTES);
    }

    #[tokio::test]
    async fn execute_command_resolves_relative_entrypoint_command_from_plugin_root() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        let bin_dir = plugin_dir.join("bin");
        let work_dir = plugin_dir.join("work");
        std::fs::create_dir_all(&bin_dir).expect("bin dir");
        std::fs::create_dir_all(&work_dir).expect("work dir");
        let tool_path = bin_dir.join("tool");
        std::fs::write(
            &tool_path,
            "#!/bin/sh\nprintf '{\"message\":\"root-relative command\"}'\n",
        )
        .expect("write tool");
        #[cfg(unix)]
        {
            let mut permissions = std::fs::metadata(&tool_path)
                .expect("tool metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&tool_path, permissions).expect("chmod tool");
        }
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": "bin/tool",
                        "cwd": "work"
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.rootRelative",
                            "title": "Root Relative",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]);

        let response = runtime
            .execute_command(PluginCommandExecutionRouteRequest {
                plugin_id: "example.tools".to_string(),
                command_id: "example.rootRelative".to_string(),
                input: None,
                workspace_id: None,
                task_id: None,
                session_id: None,
            })
            .await;

        assert_eq!(
            response.status,
            ctx_route_contracts::plugins::PluginCommandExecutionStatus::Completed
        );
        assert_eq!(response.message.as_deref(), Some("root-relative command"));
    }

    #[test]
    fn relative_entrypoint_command_cannot_escape_plugin_root() {
        let temp = tempfile::tempdir().expect("tempdir");

        assert!(resolve_plugin_entrypoint_command(temp.path(), "../tool").is_err());
        assert!(resolve_plugin_entrypoint_command(temp.path(), "bin/../../tool").is_err());
        assert!(resolve_plugin_entrypoint_command(temp.path(), r"..\tool").is_err());
    }

    #[tokio::test]
    async fn execute_command_auto_reloads_manifest_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        let manifest_path = plugin_dir.join("ctx-plugin.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": test_shell_command(),
                        "args": ["-c", "printf '{\"message\":\"before\"}'"]
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.expand",
                            "title": "Expand",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots_and_auto_reload_interval(
            vec![temp.path().to_path_buf()],
            Duration::ZERO,
        );

        let before = runtime
            .execute_command(PluginCommandExecutionRouteRequest {
                plugin_id: "example.tools".to_string(),
                command_id: "example.expand".to_string(),
                input: None,
                workspace_id: None,
                task_id: None,
                session_id: None,
            })
            .await;
        assert_eq!(before.message.as_deref(), Some("before"));

        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.1",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": test_shell_command(),
                        "args": ["-c", "printf '{\"message\":\"after\"}'"]
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.expand",
                            "title": "Expand",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("rewrite manifest");

        let after = runtime
            .execute_command(PluginCommandExecutionRouteRequest {
                plugin_id: "example.tools".to_string(),
                command_id: "example.expand".to_string(),
                input: None,
                workspace_id: None,
                task_id: None,
                session_id: None,
            })
            .await;
        assert_eq!(
            after.message.as_deref(),
            Some("after"),
            "after response: {after:?}"
        );
    }

    #[tokio::test]
    async fn execute_command_rejects_missing_entrypoint() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "contributes": {
                    "commands": [
                        {
                            "id": "example.expand",
                            "title": "Expand"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]);

        let response = runtime
            .execute_command(PluginCommandExecutionRouteRequest {
                plugin_id: "example.tools".to_string(),
                command_id: "example.expand".to_string(),
                input: None,
                workspace_id: None,
                task_id: None,
                session_id: None,
            })
            .await;

        assert_eq!(
            response.status,
            ctx_route_contracts::plugins::PluginCommandExecutionStatus::Failed
        );
        assert!(response
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("does not declare an entrypoint"));
    }

    #[tokio::test]
    async fn execute_command_rejects_cwd_that_escapes_plugin_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("example");
        std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("ctx-plugin.json"),
            serde_json::to_vec_pretty(&json!({
                "id": "example.tools",
                "name": "Example Tools",
                "version": "0.1.0",
                "entrypoints": [
                    {
                        "id": "main",
                        "command": test_shell_command(),
                        "args": ["-c", "printf '{\"message\":\"should not run\"}'"],
                        "cwd": ".."
                    }
                ],
                "contributes": {
                    "commands": [
                        {
                            "id": "example.expand",
                            "title": "Expand",
                            "entrypoint": "main"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");
        let runtime = PluginInventoryRuntime::new_with_roots(vec![temp.path().to_path_buf()]);

        let response = runtime
            .execute_command(PluginCommandExecutionRouteRequest {
                plugin_id: "example.tools".to_string(),
                command_id: "example.expand".to_string(),
                input: None,
                workspace_id: None,
                task_id: None,
                session_id: None,
            })
            .await;

        assert_eq!(
            response.status,
            ctx_route_contracts::plugins::PluginCommandExecutionStatus::Failed
        );
        assert!(response
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("cannot escape"));
    }
}
