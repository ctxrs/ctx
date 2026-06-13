use ctx_settings_model::*;

pub(super) fn apply_env_overrides(settings: &mut Settings) {
    // Environment overrides (optional) for easy local bring-up.
    // These are intentionally "best-effort" and do not persist.
    if let Ok(mode) = std::env::var("CTX_EXECUTION_MODE") {
        let normalized = mode.trim().to_lowercase();
        let mode = match normalized.as_str() {
            "host" => Some(ExecutionMode::Host),
            "sandbox" | "container" => Some(ExecutionMode::Sandbox),
            _ => None,
        };
        if let Some(mode) = mode {
            let execution = settings
                .execution
                .get_or_insert_with(ExecutionSettings::default);
            execution.mode = mode;
        }
    }
    if let Ok(v) = std::env::var("CTX_DICTATION_PROVIDER") {
        if v.trim().eq_ignore_ascii_case("disabled") {
            settings.dictation = Some(DictationSettings {
                enabled: false,
                provider: DictationProvider::Disabled,
                livekit: None,
            });
        }
    }

    if let Ok(api_key) = std::env::var("LIVEKIT_API_KEY") {
        let d = settings
            .dictation
            .get_or_insert_with(DictationSettings::default);
        if d.livekit.is_none() {
            d.livekit = Some(LiveKitDictationSettings::default());
        }
        if let Some(lk) = d.livekit.as_mut() {
            if lk.api_key.trim().is_empty() {
                lk.api_key = api_key;
            }
        }
    }
    if let Ok(api_secret) = std::env::var("LIVEKIT_API_SECRET") {
        let d = settings
            .dictation
            .get_or_insert_with(DictationSettings::default);
        if d.livekit.is_none() {
            d.livekit = Some(LiveKitDictationSettings::default());
        }
        if let Some(lk) = d.livekit.as_mut() {
            if lk
                .api_secret
                .as_ref()
                .map(|s| s.trim().is_empty())
                .unwrap_or(true)
            {
                lk.api_secret = Some(api_secret);
            }
        }
    }
    if let Ok(base_url) = std::env::var("CTX_LIVEKIT_INFERENCE_BASE_URL") {
        let d = settings
            .dictation
            .get_or_insert_with(DictationSettings::default);
        if d.livekit.is_none() {
            d.livekit = Some(LiveKitDictationSettings::default());
        }
        if let Some(lk) = d.livekit.as_mut() {
            if lk.base_url.trim() == "https://agent-gateway.livekit.cloud/v1" {
                lk.base_url = base_url;
            }
        }
    }

    if let Ok(api_key) = std::env::var("CTX_ORACLE_API_KEY") {
        let oracle = settings.oracle.get_or_insert_with(OracleSettings::default);
        if oracle.api_key.trim().is_empty() {
            oracle.api_key = api_key;
        }
        if !oracle.api_key.trim().is_empty() {
            oracle.enabled = true;
        }
    }
    if let Ok(base_url) = std::env::var("CTX_ORACLE_BASE_URL") {
        let oracle = settings.oracle.get_or_insert_with(OracleSettings::default);
        if oracle.base_url.trim() == "https://api.openai.com/v1" {
            oracle.base_url = base_url;
        }
    }
    if let Ok(model) = std::env::var("CTX_ORACLE_MODEL") {
        let oracle = settings.oracle.get_or_insert_with(OracleSettings::default);
        if oracle.model.trim() == "gpt-5.2-pro" {
            oracle.model = model;
        }
    }

    let parse_bool = ctx_core::boolish::parse_boolish;
    let parse_mode = |value: &str| match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(ResourceGovernanceMode::Auto),
        "custom" => Some(ResourceGovernanceMode::Custom),
        _ => None,
    };

    if let Ok(value) = std::env::var("CTX_TOOL_LIMITS_ENABLED") {
        if let Some(enabled) = parse_bool(&value) {
            let tool_limits = settings
                .tool_limits
                .get_or_insert_with(ToolLimitsSettings::default);
            tool_limits.enabled = enabled;
        }
    }
    if let Ok(value) = std::env::var("CTX_TOOL_LIMITS_MODE") {
        if let Some(mode) = parse_mode(&value) {
            let tool_limits = settings
                .tool_limits
                .get_or_insert_with(ToolLimitsSettings::default);
            tool_limits.mode = mode;
        }
    }
    if let Ok(value) = std::env::var("CTX_TOOL_LIMITS_MEMORY_HIGH_MB") {
        if let Ok(parsed) = value.trim().parse::<u32>() {
            let tool_limits = settings
                .tool_limits
                .get_or_insert_with(ToolLimitsSettings::default);
            tool_limits.memory_high_mb = Some(parsed);
        }
    }
    if let Ok(value) = std::env::var("CTX_TOOL_LIMITS_MEMORY_MAX_MB") {
        if let Ok(parsed) = value.trim().parse::<u32>() {
            let tool_limits = settings
                .tool_limits
                .get_or_insert_with(ToolLimitsSettings::default);
            tool_limits.memory_max_mb = Some(parsed);
        }
    }

    if let Ok(value) = std::env::var("CTX_PROVIDER_RESTART_ENABLED") {
        if let Some(enabled) = parse_bool(&value) {
            let restart = settings
                .provider_restart
                .get_or_insert_with(ProviderRestartSettings::default);
            restart.enabled = enabled;
        }
    }
    if let Ok(value) = std::env::var("CTX_PROVIDER_RESTART_MODE") {
        if let Some(mode) = parse_mode(&value) {
            let restart = settings
                .provider_restart
                .get_or_insert_with(ProviderRestartSettings::default);
            restart.mode = mode;
        }
    }
    if let Ok(value) = std::env::var("CTX_PROVIDER_RESTART_MEMORY_HIGH_MB") {
        if let Ok(parsed) = value.trim().parse::<u32>() {
            let restart = settings
                .provider_restart
                .get_or_insert_with(ProviderRestartSettings::default);
            restart.memory_high_mb = Some(parsed);
        }
    }
    if let Ok(value) = std::env::var("CTX_PROVIDER_RESTART_MEMORY_MAX_MB") {
        if let Ok(parsed) = value.trim().parse::<u32>() {
            let restart = settings
                .provider_restart
                .get_or_insert_with(ProviderRestartSettings::default);
            restart.memory_max_mb = Some(parsed);
        }
    }
    if let Ok(value) = std::env::var("CTX_PROVIDER_RESTART_INTERVAL_MS") {
        if let Ok(parsed) = value.trim().parse::<u64>() {
            let restart = settings
                .provider_restart
                .get_or_insert_with(ProviderRestartSettings::default);
            restart.interval_ms = Some(parsed);
        }
    }
    if let Ok(value) = std::env::var("CTX_PROVIDER_RESTART_GRACE_PERIOD_MS") {
        if let Ok(parsed) = value.trim().parse::<u64>() {
            let restart = settings
                .provider_restart
                .get_or_insert_with(ProviderRestartSettings::default);
            restart.grace_period_ms = Some(parsed);
        }
    }
}
