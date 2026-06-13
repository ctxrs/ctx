use ctx_providers::adapters::{
    ProviderHealth, ProviderRecommendedAction, ProviderStatus, ProviderUsability,
    ProviderUsabilityStatus,
};
use std::collections::{BTreeSet, HashMap};

use crate::provider_usability::{
    apply_install_viability_details, apply_provider_usability_details,
};
use crate::{ProviderRuntime, ProviderRuntimeHost};
use ctx_managed_installs as installer;
use ctx_provider_install::install_state::InstallTarget;
use ctx_provider_matrix as provider_matrix;

use super::resolver::ensure_provider_adapter_for_target_with_cfg;

fn inspect_error_status(provider_id: &str, err: anyhow::Error) -> ProviderStatus {
    ProviderStatus {
        provider_id: provider_id.to_string(),
        installed: false,
        detected_path: None,
        version: None,
        capabilities: None,
        health: ctx_providers::adapters::ProviderHealth::Error,
        diagnostics: vec![err.to_string()],
        details: HashMap::new(),
        usability: ctx_providers::adapters::ProviderUsability::default(),
    }
}

fn missing_provider_status(provider_id: &str) -> ProviderStatus {
    ProviderStatus {
        provider_id: provider_id.to_string(),
        installed: false,
        detected_path: None,
        version: None,
        capabilities: None,
        health: ProviderHealth::Missing,
        diagnostics: vec![format!("provider not available: {provider_id}")],
        details: HashMap::new(),
        usability: ProviderUsability::default(),
    }
}

impl ProviderRuntime {
    pub async fn provider_status_without_target_bootstrap(
        &self,
        provider_id: &str,
        target: InstallTarget,
    ) -> ProviderStatus {
        if matches!(target, InstallTarget::Host) {
            return self
                .provider_status(provider_id)
                .await
                .unwrap_or_else(|| missing_provider_status(provider_id));
        }

        ProviderStatus {
            provider_id: provider_id.to_string(),
            installed: false,
            detected_path: None,
            version: None,
            capabilities: None,
            health: ProviderHealth::Missing,
            diagnostics: vec![format!(
                "provider not available for target '{}'",
                target.as_str()
            )],
            details: HashMap::new(),
            usability: ProviderUsability::default(),
        }
    }
}

fn managed_targets_for_provider(
    managed: &installer::AgentServerConfigFile,
    provider_id: &str,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();

    if let Some(targets) = managed.managed_install_targets.get(provider_id) {
        for key in targets.keys() {
            if let Ok(target) = installer::parse_install_target(Some(key.as_str())) {
                out.insert(target.as_str().to_string());
            }
        }
    }
    if let Some(targets) = managed.managed_provider_targets.get(provider_id) {
        for key in targets.keys() {
            if let Ok(target) = installer::parse_install_target(Some(key.as_str())) {
                out.insert(target.as_str().to_string());
            }
        }
    }

    out
}

fn synthesize_target_mismatch_status(
    managed: &installer::AgentServerConfigFile,
    provider_id: &str,
    target: InstallTarget,
) -> Option<ProviderStatus> {
    let runtime_available = match installer::resolve_runtime_provider_command_for_target(
        managed,
        provider_id,
        Some(target),
    ) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(_) => true,
    };
    if runtime_available {
        return None;
    }

    let available_targets = managed_targets_for_provider(managed, provider_id);
    if available_targets.is_empty() {
        return None;
    }

    let requested_target = target.as_str();
    let mut details = HashMap::new();
    details.insert("install_target".into(), requested_target.to_string());
    details.insert("target_mismatch".into(), "true".into());
    details.insert(
        "available_managed_targets".into(),
        available_targets
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(","),
    );
    if available_targets.len() == 1 {
        if let Some(target_value) = available_targets.iter().next() {
            details.insert("managed_target".into(), target_value.clone());
        }
    }

    let diagnostic = if available_targets.contains(requested_target) {
        format!(
            "provider is not installed for target '{requested_target}'; configure a valid runtime command or reinstall it for that target"
        )
    } else if available_targets.len() == 1 {
        let available_target = available_targets.iter().next().cloned().unwrap_or_default();
        format!(
            "provider is installed for target '{available_target}' but not for target '{requested_target}'"
        )
    } else {
        format!(
            "provider is not installed for target '{requested_target}'; available managed targets: {}",
            available_targets.into_iter().collect::<Vec<_>>().join(", ")
        )
    };

    Some(ProviderStatus {
        provider_id: provider_id.to_string(),
        installed: false,
        detected_path: None,
        version: None,
        capabilities: None,
        health: ctx_providers::adapters::ProviderHealth::Missing,
        diagnostics: vec![diagnostic],
        details,
        usability: ctx_providers::adapters::ProviderUsability::default(),
    })
}

pub fn apply_target_aware_provider_status(
    status: &mut ProviderStatus,
    managed: &installer::AgentServerConfigFile,
    target: InstallTarget,
) {
    installer::apply_managed_install_details_for_target(status, managed, Some(target));
    installer::apply_install_target_status(status, target);
}

pub fn mark_provider_status_with_managed_config_error(
    status: &mut ProviderStatus,
    config_error: &str,
) {
    let reason = format!("managed provider config error: {config_error}");
    status.health = ProviderHealth::Error;
    status
        .details
        .insert("managed_config_error".into(), "true".into());
    status.details.insert(
        "managed_config_error_message".into(),
        config_error.to_string(),
    );
    if !status.diagnostics.iter().any(|value| value == &reason) {
        status.diagnostics.push(reason.clone());
    }
    status.usability = ProviderUsability {
        usable: false,
        status: ProviderUsabilityStatus::Blocked,
        reason_code: Some("managed_config_error".into()),
        reason: Some(reason),
        blocking_provider_ids: Vec::new(),
        recommended_action: ProviderRecommendedAction::ConfigureRuntime,
    };
}

pub async fn provider_status_for_target(
    state: &impl ProviderRuntimeHost,
    managed: &installer::AgentServerConfigFile,
    matrix: &provider_matrix::ProviderMatrix,
    provider_id: &str,
    target: InstallTarget,
) -> ProviderStatus {
    let current_ctx_version = state.current_ctx_version();
    let mut status = if let Some(status) =
        synthesize_target_mismatch_status(managed, provider_id, target)
    {
        status
    } else if matches!(target, InstallTarget::Host) {
        state
            .provider_runtime()
            .provider_status(provider_id)
            .await
            .unwrap_or_else(|| missing_provider_status(provider_id))
    } else {
        let adapter =
            ensure_provider_adapter_for_target_with_cfg(state, managed, provider_id, target).await;
        match adapter.inspect().await {
            Ok(status) => status,
            Err(err) => inspect_error_status(provider_id, err),
        }
    };
    status
        .details
        .insert("install_target".into(), target.as_str().to_string());
    apply_target_aware_provider_status(&mut status, managed, target);
    if let Some(entry) = provider_matrix::get_entry(matrix, provider_id) {
        installer::provider_status_matrix::apply_matrix_to_status(
            state.data_root(),
            managed,
            entry,
            &mut status,
            current_ctx_version.as_deref(),
        )
        .await;
    }
    apply_install_viability_details(
        &mut status,
        state.data_root(),
        managed,
        matrix,
        target,
        current_ctx_version.as_deref(),
    );
    apply_provider_usability_details(
        &mut status,
        state.data_root(),
        managed,
        matrix,
        target,
        current_ctx_version.as_deref(),
    );
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_config_error_marks_status_blocked_without_duplicate_diagnostics() {
        let mut status = ProviderStatus {
            provider_id: "codex".to_string(),
            installed: true,
            detected_path: None,
            version: None,
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ProviderUsability::default(),
        };

        mark_provider_status_with_managed_config_error(&mut status, "bad config");
        mark_provider_status_with_managed_config_error(&mut status, "bad config");

        assert_eq!(status.health, ProviderHealth::Error);
        assert_eq!(
            status.details.get("managed_config_error"),
            Some(&"true".to_string())
        );
        assert_eq!(
            status.details.get("managed_config_error_message"),
            Some(&"bad config".to_string())
        );
        assert_eq!(
            status.diagnostics,
            vec!["managed provider config error: bad config".to_string()]
        );
        assert!(!status.usability.usable);
        assert_eq!(
            status.usability.reason_code.as_deref(),
            Some("managed_config_error")
        );
        assert_eq!(
            status.usability.recommended_action,
            ProviderRecommendedAction::ConfigureRuntime
        );
    }

    #[tokio::test]
    async fn provider_status_without_target_bootstrap_uses_host_status_or_missing_target() {
        let runtime = ProviderRuntime::new(HashMap::new());
        runtime
            .upsert_provider_status(
                "codex".to_string(),
                ProviderStatus {
                    provider_id: "codex".to_string(),
                    installed: true,
                    detected_path: None,
                    version: Some("1.2.3".to_string()),
                    capabilities: None,
                    health: ProviderHealth::Ok,
                    diagnostics: Vec::new(),
                    details: HashMap::new(),
                    usability: ProviderUsability::default(),
                },
            )
            .await;

        let host = runtime
            .provider_status_without_target_bootstrap("codex", InstallTarget::Host)
            .await;
        assert_eq!(host.version.as_deref(), Some("1.2.3"));

        let target = runtime
            .provider_status_without_target_bootstrap("codex", InstallTarget::Container)
            .await;
        assert_eq!(target.health, ProviderHealth::Missing);
        assert_eq!(
            target.diagnostics,
            vec!["provider not available for target 'container'".to_string()]
        );
    }
}
