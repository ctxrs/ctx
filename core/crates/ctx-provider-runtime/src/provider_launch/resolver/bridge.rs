use super::*;

struct StaticStatusAdapter {
    status: ProviderStatus,
}

#[async_trait::async_trait]
impl ProviderAdapter for StaticStatusAdapter {
    async fn inspect(&self) -> Result<ProviderStatus> {
        Ok(self.status.clone())
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: tokio::sync::mpsc::Sender<ctx_providers::events::NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        let msg = self
            .status
            .diagnostics
            .first()
            .cloned()
            .unwrap_or_else(|| "provider is unavailable".to_string());
        anyhow::bail!("{msg}");
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> Result<()> {
        Ok(())
    }
}

fn static_status_adapter(
    provider_id: &str,
    installed: bool,
    health: ProviderHealth,
    error_code: &str,
    message: String,
) -> Arc<dyn ProviderAdapter> {
    let mut details = HashMap::new();
    details.insert("error_code".to_string(), error_code.to_string());
    Arc::new(StaticStatusAdapter {
        status: ProviderStatus {
            provider_id: provider_id.to_string(),
            installed,
            detected_path: None,
            version: None,
            capabilities: None,
            health,
            diagnostics: vec![message],
            details,
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    })
}

pub fn runtime_command_as_agent_command_for_target(
    cfg: &installer::AgentServerConfigFile,
    provider_id: &str,
    requested_target: Option<InstallTarget>,
) -> Result<Option<installer::AgentServerCommand>> {
    let Some(resolved) =
        installer::resolve_runtime_provider_command_for_target(cfg, provider_id, requested_target)?
    else {
        return Ok(None);
    };
    Ok(Some(installer::AgentServerCommand {
        command: resolved.command_abs_path,
        args: resolved.args,
        dependencies: resolved.dependencies,
        managed: None,
    }))
}

pub(super) fn acp_status_adapter_bridge_missing(
    provider_id: &str,
    msg: String,
) -> Arc<dyn ProviderAdapter> {
    static_status_adapter(
        provider_id,
        false,
        ProviderHealth::Error,
        "acp_bridge_missing",
        msg,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpenHandsRuntimeContract {
    UpstreamAcp,
    ShimAcp,
    Unknown,
}

impl OpenHandsRuntimeContract {
    fn as_str(self) -> &'static str {
        match self {
            Self::UpstreamAcp => "upstream_acp",
            Self::ShimAcp => "shim_acp",
            Self::Unknown => "unknown",
        }
    }

    fn note(self) -> &'static str {
        match self {
            Self::UpstreamAcp => "runtime command matches the upstream `openhands acp` contract",
            Self::ShimAcp => "runtime command still points at the legacy `openhands-acp` shim",
            Self::Unknown => {
                "runtime command does not clearly match the upstream `openhands acp` contract"
            }
        }
    }

    fn supports_real_runtime(self) -> bool {
        matches!(self, Self::UpstreamAcp)
    }
}

fn path_file_stem_or_raw(raw: &str) -> String {
    StdPath::new(raw)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(raw)
        .to_string()
}

fn path_file_name_or_raw(raw: &str) -> String {
    StdPath::new(raw)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(raw)
        .to_string()
}

pub(super) fn openhands_runtime_contract_for_command(
    cmd: &installer::AgentServerCommand,
) -> OpenHandsRuntimeContract {
    let command_stem = path_file_stem_or_raw(&cmd.command).to_ascii_lowercase();
    let command_name = path_file_name_or_raw(&cmd.command).to_ascii_lowercase();

    if command_stem == "openhands" && cmd.args.first().is_some_and(|arg| arg == "acp") {
        return OpenHandsRuntimeContract::UpstreamAcp;
    }

    if command_stem == "openhands-acp" || command_name == "openhands-acp.js" {
        return OpenHandsRuntimeContract::ShimAcp;
    }

    if command_stem.starts_with("python") {
        let module_arg = cmd.args.iter().position(|arg| arg == "-m");
        if let Some(idx) = module_arg {
            let module = cmd
                .args
                .get(idx + 1)
                .map(String::as_str)
                .unwrap_or_default();
            let subcommand = cmd
                .args
                .get(idx + 2)
                .map(String::as_str)
                .unwrap_or_default();
            if matches!(module, "openhands" | "openhands.core.main") && subcommand == "acp" {
                return OpenHandsRuntimeContract::UpstreamAcp;
            }
        }
    }

    if let Some(first_arg) = cmd.args.first() {
        let first_stem = path_file_stem_or_raw(first_arg).to_ascii_lowercase();
        let first_name = path_file_name_or_raw(first_arg).to_ascii_lowercase();
        if first_stem == "openhands-acp" || first_name == "openhands-acp.js" {
            return OpenHandsRuntimeContract::ShimAcp;
        }
    }

    OpenHandsRuntimeContract::Unknown
}

fn apply_openhands_runtime_contract_details(
    status: &mut ProviderStatus,
    contract: OpenHandsRuntimeContract,
) {
    status.details.insert(
        "openhands_runtime_contract".to_string(),
        contract.as_str().to_string(),
    );
    status.details.insert(
        "openhands_real_runtime".to_string(),
        if contract.supports_real_runtime() {
            "true".to_string()
        } else {
            "false".to_string()
        },
    );
    status.details.insert(
        "openhands_runtime_contract_note".to_string(),
        contract.note().to_string(),
    );
}

struct OpenHandsRuntimeContractAdapter {
    inner: Arc<dyn ProviderAdapter>,
    contract: OpenHandsRuntimeContract,
}

impl OpenHandsRuntimeContractAdapter {
    fn new(inner: Arc<dyn ProviderAdapter>, contract: OpenHandsRuntimeContract) -> Self {
        Self { inner, contract }
    }
}

#[async_trait::async_trait]
impl ProviderAdapter for OpenHandsRuntimeContractAdapter {
    async fn inspect(&self) -> Result<ProviderStatus> {
        let mut status = self.inner.inspect().await?;
        apply_openhands_runtime_contract_details(&mut status, self.contract);
        Ok(status)
    }

    async fn run(
        &self,
        input: TurnInput,
        workdir: PathBuf,
        env: HashMap<String, String>,
        event_sink: tokio::sync::mpsc::Sender<ctx_providers::events::NormalizedEvent>,
        hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        self.inner.run(input, workdir, env, event_sink, hooks).await
    }

    async fn cancel(&self, handle: &mut RunHandle) -> Result<()> {
        self.inner.cancel(handle).await
    }

    async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
        self.inner.list_processes().await
    }

    async fn restart(&self, reason: &str, mode: ProviderRestartMode) -> Result<()> {
        self.inner.restart(reason, mode).await
    }

    fn supports_restart_mode(&self, mode: ProviderRestartMode) -> bool {
        self.inner.supports_restart_mode(mode)
    }

    async fn reap_idle_sessions(
        &self,
        config: ProviderSessionSweepConfig,
    ) -> Result<ProviderSessionSweepStats> {
        self.inner.reap_idle_sessions(config).await
    }

    async fn has_live_session(&self, session_key: &str) -> bool {
        self.inner.has_live_session(session_key).await
    }

    fn supports_resume(&self) -> bool {
        self.inner.supports_resume()
    }

    async fn set_session_model(&self, session_key: String, model_id: String) -> Result<()> {
        self.inner.set_session_model(session_key, model_id).await
    }

    async fn set_session_mode(&self, session_key: String, mode_id: String) -> Result<()> {
        self.inner.set_session_mode(session_key, mode_id).await
    }

    async fn authenticate_session(
        &self,
        session_key: String,
        workdir: PathBuf,
        env: HashMap<String, String>,
        method_id: Option<String>,
        event_sink: tokio::sync::mpsc::Sender<ctx_providers::events::NormalizedEvent>,
        hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<()> {
        self.inner
            .authenticate_session(session_key, workdir, env, method_id, event_sink, hooks)
            .await
    }
}

pub(super) fn acp_status_adapter_bridge_invalid(
    provider_id: &str,
    msg: String,
) -> Arc<dyn ProviderAdapter> {
    static_status_adapter(
        provider_id,
        false,
        ProviderHealth::Error,
        "acp_bridge_invalid",
        msg,
    )
}

pub(super) fn acp_status_adapter_acp_command_invalid(
    provider_id: &str,
    msg: String,
) -> Arc<dyn ProviderAdapter> {
    static_status_adapter(
        provider_id,
        true,
        ProviderHealth::Error,
        "acp_command_invalid",
        msg,
    )
}

pub(super) fn runtime_command_missing_adapter(provider_id: &str) -> Arc<dyn ProviderAdapter> {
    static_status_adapter(
        provider_id,
        false,
        ProviderHealth::Missing,
        "runtime_command_missing",
        format!("runtime command is not configured for provider '{provider_id}'"),
    )
}

pub(super) fn runtime_command_invalid_adapter(
    provider_id: &str,
    err: String,
) -> Arc<dyn ProviderAdapter> {
    static_status_adapter(
        provider_id,
        false,
        ProviderHealth::Error,
        "runtime_command_invalid",
        format!("invalid runtime command for provider '{provider_id}': {err}"),
    )
}

pub fn is_acp_provider_id(provider_id: &str) -> bool {
    matches!(
        provider_id,
        "gemini"
            | "qwen"
            | "cursor"
            | "pi"
            | "opencode"
            | "mistral"
            | "goose"
            | "kimi"
            | "auggie"
            | "amp"
            | "droid"
            | "copilot"
            | "cline"
            | "openhands"
    )
}

fn escape_shell_arg(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let is_simple = value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"@%_-+=:,./".contains(&b));
    if is_simple {
        return value.to_string();
    }
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn format_shell_command(command: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(1 + args.len());
    parts.push(escape_shell_arg(command));
    for arg in args {
        parts.push(escape_shell_arg(arg));
    }
    parts.join(" ")
}

pub fn acp_bridge_command(
    bridge_cmd: &installer::AgentServerCommand,
    acp_cmd: installer::AgentServerCommand,
) -> installer::AgentServerCommand {
    let acp_command = format_shell_command(&acp_cmd.command, &acp_cmd.args);
    let mut args = bridge_cmd.args.clone();
    args.push("--acp-command".to_string());
    args.push(acp_command);
    installer::AgentServerCommand {
        command: bridge_cmd.command.clone(),
        args,
        dependencies: Vec::new(),
        managed: None,
    }
}

pub fn acp_bridge_adapter(
    id: &str,
    bridge_cmd: &installer::AgentServerCommand,
    acp_cmd: installer::AgentServerCommand,
) -> Arc<dyn ProviderAdapter> {
    let contract = if id == "openhands" {
        Some(openhands_runtime_contract_for_command(&acp_cmd))
    } else {
        None
    };
    let bridged = acp_bridge_command(bridge_cmd, acp_cmd);
    let inner: Arc<dyn ProviderAdapter> = Arc::new(
        Tier1CrpAdapter::from_provider_runtime_acp_bridge(id, bridged.command, bridged.args),
    );
    if let Some(contract) = contract {
        return Arc::new(OpenHandsRuntimeContractAdapter::new(inner, contract));
    }
    inner
}
