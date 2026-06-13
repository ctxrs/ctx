use super::*;

pub(super) async fn install_provider_blocking_dependencies(
    state: &ManagedInstallHostObject,
    provider_id: &str,
    dependencies: &[provider_install_contract::ProviderInstallDependency],
    install_id: Option<InstallId>,
) -> Result<()> {
    for dependency in dependencies {
        state
            .validate_install_target_allowed(dependency.target)
            .with_context(|| {
                format!(
                    "provider prerequisite dependency '{}' target '{}' is not allowed",
                    dependency.provider_id,
                    dependency.target.as_str()
                )
            })?;
        ensure_install_not_cancelled(state, install_id).await?;
        let (prerequisite_install_id, started_new) = state
            .start_install(dependency.provider_id.clone(), Some(dependency.target))
            .await;
        if let Some(parent_install_id) = install_id {
            anyhow::ensure!(
                state
                    .register_install_progress_mirror(prerequisite_install_id, parent_install_id)
                    .await,
                "tracked prerequisite install {} for provider '{}' target '{}' is missing",
                prerequisite_install_id,
                dependency.provider_id,
                dependency.target.as_str()
            );
        }
        emit_install(
            state,
            install_id,
            provider_id,
            InstallEventLevel::Info,
            "prerequisites",
            if started_new {
                format!(
                    "Installing prerequisite provider {} for {}",
                    dependency.provider_id, provider_id
                )
            } else {
                format!(
                    "Waiting for prerequisite provider {} install {} for {}",
                    dependency.provider_id, prerequisite_install_id, provider_id
                )
            },
            None,
            None,
            None,
        )
        .await;
        if started_new {
            run_tracked_provider_install(
                state,
                prerequisite_install_id,
                &dependency.provider_id,
                dependency.target,
            )
            .await
            .with_context(|| {
                format!(
                    "installing prerequisite provider '{}' for '{}'",
                    dependency.provider_id, provider_id
                )
            })?;
        } else {
            wait_for_tracked_install(
                state,
                prerequisite_install_id,
                &dependency.provider_id,
                dependency.target,
                install_id,
            )
            .await
            .with_context(|| {
                format!(
                    "waiting for prerequisite provider '{}' for '{}'",
                    dependency.provider_id, provider_id
                )
            })?;
        }
        emit_install(
            state,
            install_id,
            provider_id,
            InstallEventLevel::Info,
            "prerequisites",
            format!(
                "Prerequisite provider {} install {} completed for {}",
                dependency.provider_id, prerequisite_install_id, provider_id
            ),
            None,
            None,
            None,
        )
        .await;
    }
    Ok(())
}

pub(super) async fn wait_for_provider_readiness_dependencies(
    state: &ManagedInstallHostObject,
    provider_id: &str,
    dependencies: &[provider_install_contract::ProviderInstallDependency],
    install_id: Option<InstallId>,
) -> Result<()> {
    for dependency in dependencies {
        if dependency.satisfied {
            continue;
        }
        state
            .validate_install_target_allowed(dependency.target)
            .with_context(|| {
                format!(
                    "provider readiness dependency '{}' target '{}' is not allowed",
                    dependency.provider_id,
                    dependency.target.as_str()
                )
            })?;
        ensure_install_not_cancelled(state, install_id).await?;
        let (dependency_install_id, started_new) = state
            .start_install(dependency.provider_id.clone(), Some(dependency.target))
            .await;
        if let Some(parent_install_id) = install_id {
            anyhow::ensure!(
                state
                    .register_install_progress_mirror(dependency_install_id, parent_install_id)
                    .await,
                "tracked readiness dependency install {} for provider '{}' target '{}' is missing",
                dependency_install_id,
                dependency.provider_id,
                dependency.target.as_str()
            );
        }
        emit_install(
            state,
            install_id,
            provider_id,
            InstallEventLevel::Info,
            "dependencies",
            if started_new {
                format!(
                    "Installing readiness dependency {} for {}",
                    dependency.provider_id, provider_id
                )
            } else {
                format!(
                    "Waiting for readiness dependency {} install {} for {}",
                    dependency.provider_id, dependency_install_id, provider_id
                )
            },
            None,
            None,
            None,
        )
        .await;
        if let Some(parent_install_id) = install_id {
            state
                .set_install_progress_pct_override(parent_install_id, Some(99))
                .await;
        }
        if started_new {
            run_tracked_provider_install(
                state,
                dependency_install_id,
                &dependency.provider_id,
                dependency.target,
            )
            .await
            .with_context(|| {
                format!(
                    "installing readiness dependency '{}' for '{}'",
                    dependency.provider_id, provider_id
                )
            })?;
        } else {
            wait_for_tracked_install(
                state,
                dependency_install_id,
                &dependency.provider_id,
                dependency.target,
                install_id,
            )
            .await
            .with_context(|| {
                format!(
                    "waiting for readiness dependency '{}' for '{}'",
                    dependency.provider_id, provider_id
                )
            })?;
        }
        emit_install(
            state,
            install_id,
            provider_id,
            InstallEventLevel::Info,
            "dependencies",
            format!(
                "Readiness dependency {} install {} completed for {}",
                dependency.provider_id, dependency_install_id, provider_id
            ),
            None,
            None,
            None,
        )
        .await;
    }
    if let Some(parent_install_id) = install_id {
        state
            .set_install_progress_pct_override(parent_install_id, None)
            .await;
    }
    Ok(())
}
