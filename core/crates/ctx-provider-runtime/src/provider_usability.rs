use std::path::Path as StdPath;

use ctx_providers::adapters::{
    ProviderHealth, ProviderRecommendedAction, ProviderStatus, ProviderUsability,
    ProviderUsabilityStatus,
};

use ctx_managed_installs as installer;
use ctx_managed_installs::provider_install_contract;
use ctx_provider_install::install_state::InstallTarget;
use ctx_provider_matrix::ProviderMatrix;

fn first_diagnostic(status: &ProviderStatus) -> Option<String> {
    status
        .diagnostics
        .iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn usability_reason_from_status(
    status: &ProviderStatus,
    pending_dependencies: &[String],
) -> (Option<String>, Option<String>, ProviderRecommendedAction) {
    if !pending_dependencies.is_empty() {
        return (
            Some("missing_dependency".into()),
            Some(format!(
                "provider is not ready until required dependencies are installed: {}",
                pending_dependencies.join(", ")
            )),
            ProviderRecommendedAction::ResolveDependency,
        );
    }

    if status.detail_flag("target_mismatch") == Some(true) {
        return (
            Some("target_mismatch".into()),
            first_diagnostic(status),
            ProviderRecommendedAction::SwitchTarget,
        );
    }

    if status.detail_flag("target_unverified") == Some(true) {
        return (
            Some("target_unverified".into()),
            first_diagnostic(status),
            ProviderRecommendedAction::SwitchTarget,
        );
    }

    if status.detail_flag("install_blocked") == Some(true) {
        return (
            Some(
                status
                    .details
                    .get("install_blocked_code")
                    .cloned()
                    .unwrap_or_else(|| "install_blocked".into()),
            ),
            status
                .details
                .get("install_blocked_reason")
                .cloned()
                .or_else(|| first_diagnostic(status)),
            ProviderRecommendedAction::ConfigureRuntime,
        );
    }

    if matches!(status.health, ProviderHealth::Missing) {
        return (
            Some("not_installed".into()),
            first_diagnostic(status),
            ProviderRecommendedAction::Install,
        );
    }

    (
        Some("unhealthy".into()),
        first_diagnostic(status),
        ProviderRecommendedAction::ConfigureRuntime,
    )
}

pub fn provider_status_is_usable(status: &ProviderStatus) -> bool {
    status.usability.usable
}

pub fn provider_status_unusable_reason(status: &ProviderStatus) -> Option<String> {
    status
        .usability
        .reason
        .clone()
        .or_else(|| first_diagnostic(status))
}

pub fn apply_install_viability_details(
    status: &mut ProviderStatus,
    data_root: &StdPath,
    managed: &installer::AgentServerConfigFile,
    matrix: &ProviderMatrix,
    target: InstallTarget,
    current_ctx_version: Option<&str>,
) {
    let install_viability = provider_install_contract::provider_install_viability_issue(
        data_root,
        managed,
        matrix,
        &status.provider_id,
        target,
        current_ctx_version,
    );
    status.details.insert(
        "install_supported".into(),
        if installer::is_compatible_managed_provider_for_target(
            matrix,
            &status.provider_id,
            target,
            current_ctx_version,
        ) && install_viability.is_none()
        {
            "true".into()
        } else {
            "false".into()
        },
    );
    if let Some(issue) = install_viability {
        status
            .details
            .insert("install_blocked".into(), "true".into());
        status
            .details
            .insert("install_blocked_code".into(), issue.code.to_string());
        status
            .details
            .insert("install_blocked_reason".into(), issue.message.clone());
        if !status
            .diagnostics
            .iter()
            .any(|value| value == &issue.message)
        {
            status.diagnostics.push(issue.message);
        }
    } else {
        status.details.remove("install_blocked");
        status.details.remove("install_blocked_code");
        status.details.remove("install_blocked_reason");
    }
}

pub fn apply_provider_usability_details(
    status: &mut ProviderStatus,
    data_root: &StdPath,
    managed: &installer::AgentServerConfigFile,
    matrix: &ProviderMatrix,
    target: InstallTarget,
    current_ctx_version: Option<&str>,
) {
    let install_supported = status.detail_flag("install_supported").unwrap_or(false);
    let base_ready = status.installed && matches!(status.health, ProviderHealth::Ok);

    if status.provider_id == "fake" {
        status.details.insert("ready_for_use".into(), "true".into());
        status.details.remove("required_dependency_ids");
        status.details.remove("pending_dependency_ids");
        status.details.remove("managed_dependency_update_available");
        status.usability = ProviderUsability {
            usable: true,
            status: ProviderUsabilityStatus::Ready,
            reason_code: None,
            reason: None,
            blocking_provider_ids: Vec::new(),
            recommended_action: ProviderRecommendedAction::None,
        };
        return;
    }

    let contract = provider_install_contract::resolve_provider_install_contract(
        data_root,
        managed,
        matrix,
        &status.provider_id,
        target,
        current_ctx_version,
    );
    let contract = match contract {
        Ok(contract) => contract,
        Err(err) => {
            let reason = err.to_string();
            status
                .details
                .insert("ready_for_use".into(), "false".into());
            status.details.remove("required_dependency_ids");
            status.details.remove("pending_dependency_ids");
            status.usability = ProviderUsability {
                usable: false,
                status: if install_supported {
                    ProviderUsabilityStatus::Blocked
                } else {
                    ProviderUsabilityStatus::Unsupported
                },
                reason_code: Some("dependency_contract_error".into()),
                reason: Some(reason.clone()),
                blocking_provider_ids: Vec::new(),
                recommended_action: ProviderRecommendedAction::ConfigureRuntime,
            };
            if !status.diagnostics.iter().any(|value| value == &reason) {
                status.diagnostics.push(reason);
            }
            return;
        }
    };

    if !contract.dependencies.is_empty() {
        status.details.insert(
            "required_dependency_ids".into(),
            contract
                .dependencies
                .iter()
                .map(|dependency| dependency.provider_id.clone())
                .collect::<Vec<_>>()
                .join(","),
        );
    } else {
        status.details.remove("required_dependency_ids");
    }

    let pending_dependencies = contract
        .dependencies
        .iter()
        .filter(|dependency| !dependency.satisfied)
        .map(|dependency| dependency.provider_id.clone())
        .collect::<Vec<_>>();
    let dependencies_ready = pending_dependencies.is_empty();

    if dependencies_ready {
        status.details.remove("pending_dependency_ids");
        status.details.remove("managed_dependency_update_available");
    } else {
        status.details.insert(
            "pending_dependency_ids".into(),
            pending_dependencies.join(","),
        );
        status
            .details
            .insert("managed_dependency_update_available".into(), "true".into());
    }

    let usable = base_ready && dependencies_ready;
    status.details.insert(
        "ready_for_use".into(),
        if usable {
            "true".into()
        } else {
            "false".into()
        },
    );

    if usable {
        status.usability = ProviderUsability {
            usable: true,
            status: ProviderUsabilityStatus::Ready,
            reason_code: None,
            reason: None,
            blocking_provider_ids: Vec::new(),
            recommended_action: ProviderRecommendedAction::None,
        };
        return;
    }

    let (reason_code, reason, recommended_action) =
        usability_reason_from_status(status, &pending_dependencies);
    let usability_status = if !install_supported {
        ProviderUsabilityStatus::Unsupported
    } else if !base_ready
        && matches!(status.health, ProviderHealth::Missing)
        && pending_dependencies.is_empty()
    {
        ProviderUsabilityStatus::Installable
    } else {
        ProviderUsabilityStatus::Blocked
    };

    if let Some(reason) = reason.clone() {
        if !status.diagnostics.iter().any(|value| value == &reason) {
            status.diagnostics.push(reason);
        }
    }

    status.usability = ProviderUsability {
        usable: false,
        status: usability_status,
        reason_code,
        reason,
        blocking_provider_ids: pending_dependencies,
        recommended_action,
    };
}
