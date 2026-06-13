use super::install_kind::install_provider_release;
use super::*;

pub(crate) async fn install_provider_impl(
    state: &ManagedInstallHostObject,
    provider_id: &str,
    target: InstallTarget,
    install_id: Option<InstallId>,
) -> Result<()> {
    if !MANAGED_PROVIDER_INSTALLS_ENABLED {
        anyhow::bail!(
            "managed provider installs are disabled; provider '{provider_id}' must be shipped in bundled harness assets",
        );
    }
    state.validate_install_target_allowed(target)?;

    let provider_id = provider_id.to_string();
    let _provider_install_lock = acquire_provider_install_lock(&provider_id, target).await;
    let requested_target_label = target.as_str();
    let mut stage: &'static str = "start";
    let mut error_package: Option<String> = None;
    let mut error_version: Option<String> = None;
    let mut error_install_dir_rel: Option<String> = None;

    let res: Result<()> = async {
        let matrix = state.load_provider_matrix().await;
        let current_ctx_version_raw = state
            .current_ctx_version()
            .ok_or_else(|| anyhow::anyhow!("current ctx build version unavailable"))?;
        let current_ctx_version = provider_matrix::parse_version_loose(&current_ctx_version_raw)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "current ctx build version is not valid semver: {current_ctx_version_raw}"
                )
            })?;
        let install_cfg = load_agent_server_config(state.data_root())
            .await
            .context("loading agent server config for provider install contract resolution")?;
        let install_contract = provider_install_contract::resolve_provider_install_contract(
            state.data_root(),
            &install_cfg,
            &matrix,
            &provider_id,
            target,
            Some(current_ctx_version_raw.as_str()),
        )
        .map_err(anyhow::Error::new)?;
        let resolved_target_key = install_contract.resolved_target_key;
        for dependency in &install_contract.dependencies {
            state
                .validate_install_target_allowed(dependency.target)
                .with_context(|| {
                    format!(
                        "provider install dependency '{}' target '{}' is not allowed",
                        dependency.provider_id,
                        dependency.target.as_str()
                    )
                })?;
        }
        let blocking_dependencies = install_contract.dependencies_for_role(
            provider_install_contract::ProviderInstallDependencyRoleKind::Prerequisite,
        );
        let readiness_dependencies = install_contract.dependencies_for_role(
            provider_install_contract::ProviderInstallDependencyRoleKind::Readiness,
        );

        ensure_install_not_cancelled(state, install_id).await?;
        if let Some(install_id) = install_id {
            state
                .update_install_start_event(
                    install_id,
                    &provider_id,
                    Some(target),
                    format!(
                        "Installing managed provider: {provider_id} (target: {requested_target_label}, resolved: {resolved_target_key})"
                    ),
                    false,
                )
                .await;
        }
        install_provider_blocking_dependencies(
            state,
            &provider_id,
            &blocking_dependencies,
            install_id,
        )
        .await?;

        let entry = provider_matrix::get_entry(&matrix, &provider_id)
            .ok_or_else(|| anyhow::anyhow!("unsupported provider for install: {provider_id}"))?;
        let Some(install) = entry.managed_install.as_ref() else {
            anyhow::bail!("provider has no managed install: {provider_id}");
        };
        let release = provider_matrix::recommended_release(entry, Some(&current_ctx_version))
            .ok_or_else(|| anyhow::anyhow!("no compatible release for provider: {provider_id}"))?;

        let mut dependency_ids: Vec<String> = install_contract
            .dependencies
            .iter()
            .map(|dependency| dependency.provider_id.clone())
            .collect();
        let mut implicit_managed_dependencies: Vec<(String, ManagedInstallMetadata)> = Vec::new();
        if !entry.dependencies.is_empty() {
            stage = "dependencies";
            ensure_install_not_cancelled(state, install_id).await?;
            emit_install(
                state,
                install_id,
                &provider_id,
                InstallEventLevel::Info,
                "dependencies",
                format!("Installing dependencies for {provider_id}"),
                None,
                None,
                None,
            )
            .await;

            for dep in &entry.dependencies {
                ensure_install_not_cancelled(state, install_id).await?;
                dependency_ids.push(dep.id.clone());
                let managed = match &dep.install {
                    provider_matrix::DependencyInstall::Npm { package, version } => {
                        if !matches!(target, InstallTarget::Host) {
                            anyhow::bail!(
                                "target '{}' is not supported for npm dependency '{}' (provider '{}'); use target host",
                                requested_target_label,
                                dep.id,
                                provider_id
                            );
                        }
                        error_package = Some(package.clone());
                        error_version = Some(version.clone());
                        error_install_dir_rel =
                            Some(format!("providers/agent-servers/{}/{}", dep.id, version));
                        install_managed_npm_dependency(
                            state,
                            install_id,
                            &provider_id,
                            &dep.id,
                            package,
                            version,
                            &mut stage,
                        )
                        .await?
                    }
                    provider_matrix::DependencyInstall::Archive { version, targets } => {
                        let target_entry = targets.get(resolved_target_key).ok_or_else(|| {
                            anyhow::anyhow!(
                                "unsupported dependency target {}: {}",
                                dep.id,
                                resolved_target_key
                            )
                        })?;
                        error_package = Some(target_entry.url.clone());
                        error_version = Some(version.clone());
                        error_install_dir_rel =
                            Some(format!("providers/agent-servers/{}/{}", dep.id, version));
                        install_managed_archive_dependency(
                            state,
                            install_id,
                            &provider_id,
                            &dep.id,
                            version,
                            &target_entry.url,
                            target_entry.sha256.as_deref(),
                            map_archive_kind(target_entry.archive),
                            &target_entry.bin_path,
                            target,
                            &mut stage,
                        )
                        .await?
                    }
                };

                mutate_agent_server_config(state.data_root(), |cfg| {
                    cfg.managed_installs
                        .insert(dep.id.clone(), managed.meta.clone());
                })
                .await
                .context("saving managed install registry")?;
            }
        }

        ensure_install_not_cancelled(state, install_id).await?;
        let managed = install_provider_release(
            state,
            install_id,
            &provider_id,
            requested_target_label,
            resolved_target_key,
            install,
            &release.version,
            target,
            &mut dependency_ids,
            &mut implicit_managed_dependencies,
            &mut stage,
            &mut error_package,
            &mut error_version,
            &mut error_install_dir_rel,
        )
        .await?;

        stage = "inspect";
        ensure_install_not_cancelled(state, install_id).await?;
        emit_install(
            state,
            install_id,
            &provider_id,
            InstallEventLevel::Info,
            "inspect",
            "Verifying provider install".to_string(),
            None,
            None,
            None,
        )
        .await;

        let adapter_cfg = load_agent_server_config(state.data_root())
            .await
            .context("loading managed install registry for provider install verification")?;
        let bridge_cmd = if state.is_acp_provider_id(&provider_id) {
            resolve_runtime_provider_command_for_target(
                &adapter_cfg,
                "acp-crp-bridge",
                Some(target),
            )?
            .map(|resolved| AgentServerCommand {
                command: resolved.command_abs_path,
                args: resolved.args,
                dependencies: resolved.dependencies,
                managed: None,
            })
        } else {
            None
        };
        let runtime_cmd = managed_provider_runtime_command(
            state.data_root(),
            &provider_id,
            AgentServerCommand {
                command: managed.command.clone(),
                args: managed.args.clone(),
                dependencies: Vec::new(),
                managed: None,
            },
            bridge_cmd.as_ref(),
        )?;
        let adapter: std::sync::Arc<Tier1CrpAdapter> = std::sync::Arc::new(
            Tier1CrpAdapter::from_provider_runtime(
                &provider_id,
                runtime_cmd.command,
                runtime_cmd.args,
            ),
        );

        if matches!(target, InstallTarget::Host) {
            state
                .upsert_provider_adapter(provider_id.clone(), adapter.clone())
                .await;
        } else {
            state
                .upsert_target_provider_adapter(
                    format!("{provider_id}@{}", target.as_str()),
                    adapter.clone(),
                )
                .await;
        }

        stage = "refresh";
        ensure_install_not_cancelled(state, install_id).await?;
        let mut status_cfg = load_agent_server_config(state.data_root())
            .await
            .context("loading managed install registry for provider status refresh")?;
        apply_managed_provider_install_to_cfg(
            &mut status_cfg,
            &provider_id,
            target,
            &managed,
            &dependency_ids,
            &implicit_managed_dependencies,
        );
        let mut verified_status = ctx_providers::adapters::ProviderAdapter::inspect(adapter.as_ref())
            .await
            .context("inspecting provider after managed install")?;
        apply_managed_install_details_for_target(&mut verified_status, &status_cfg, Some(target));
        apply_install_target_status(&mut verified_status, target);
        validate_post_install_status(&verified_status, &provider_id, target)?;
        refresh_provider_statuses_with_cfg(state, status_cfg).await?;

        stage = "registry";
        ensure_install_not_cancelled(state, install_id).await?;
        emit_install(
            state,
            install_id,
            &provider_id,
            InstallEventLevel::Info,
            "registry",
            "Writing managed install registry".to_string(),
            None,
            None,
            None,
        )
        .await;

        mutate_agent_server_config(state.data_root(), |cfg| {
            apply_managed_provider_install_to_cfg(
                cfg,
                &provider_id,
                target,
                &managed,
                &dependency_ids,
                &implicit_managed_dependencies,
            );
        })
        .await
        .context("saving managed install registry")?;

        emit_install(
            state,
            install_id,
            &provider_id,
            InstallEventLevel::Success,
            "registry",
            "Wrote managed install registry".to_string(),
            None,
            None,
            None,
        )
        .await;

        wait_for_provider_readiness_dependencies(
            state,
            &provider_id,
            &readiness_dependencies,
            install_id,
        )
        .await?;

        emit_install(
            state,
            install_id,
            &provider_id,
            InstallEventLevel::Success,
            "done",
            "Install complete".to_string(),
            None,
            None,
            None,
        )
        .await;
        Ok(())
    }
    .await;

    if let Err(e) = &res {
        let error_code = classify_install_error(stage, e);
        emit_install_with_code(
            state,
            install_id,
            &provider_id,
            InstallEventLevel::Error,
            "error",
            truncate_for_storage(&format!("{e:#}"), INSTALL_EVENT_ERROR_MAX_LEN),
            None,
            None,
            None,
            Some(error_code),
        )
        .await;
        update_registry_last_error(
            state.data_root(),
            &provider_id,
            stage,
            e,
            error_code,
            error_package.as_deref(),
            error_version.as_deref(),
            error_install_dir_rel.clone(),
            Some(target),
        )
        .await;
    }

    res
}
