use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn install_provider_release(
    state: &ManagedInstallHostObject,
    install_id: Option<InstallId>,
    provider_id: &str,
    requested_target_label: &str,
    resolved_target_key: &str,
    install: &provider_matrix::ProviderInstall,
    release_version: &str,
    target: InstallTarget,
    dependency_ids: &mut Vec<String>,
    implicit_managed_dependencies: &mut Vec<(String, ManagedInstallMetadata)>,
    stage: &mut &'static str,
    error_package: &mut Option<String>,
    error_version: &mut Option<String>,
    error_install_dir_rel: &mut Option<String>,
) -> Result<ManagedProviderInstall> {
    match install {
        provider_matrix::ProviderInstall::Npm {
            package,
            version,
            entrypoint,
            args,
            targets,
        } => {
            if !matches!(
                target,
                InstallTarget::Host
                    | InstallTarget::Container
                    | InstallTarget::LinuxAarch64
                    | InstallTarget::LinuxX8664
            ) {
                anyhow::bail!(
                    "target '{requested_target_label}' is not supported for npm provider '{provider_id}' installs; use target host, container, linux-aarch64, or linux-x86_64"
                );
            }
            if provider_matrix::normalize_version(version)
                != provider_matrix::normalize_version(release_version)
            {
                anyhow::bail!(
                    "provider matrix version mismatch for {provider_id}: release={release_version} install={version}",
                );
            }
            let version = release_version.to_string();
            *error_package = Some(package.clone());
            *error_version = Some(version.clone());
            *error_install_dir_rel =
                Some(format!("providers/agent-servers/{provider_id}/{version}"));
            if matches!(target, InstallTarget::Host) && !targets.contains_key(resolved_target_key) {
                install_managed_npm_provider(
                    state,
                    install_id,
                    provider_id,
                    package,
                    &version,
                    entrypoint,
                    resolve_install_args(args),
                    target,
                    stage,
                )
                .await
            } else {
                let target_entry = targets.get(resolved_target_key).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unsupported provider target {provider_id}: {resolved_target_key}"
                    )
                })?;
                let mut managed = install_managed_archive_provider(
                    state,
                    install_id,
                    provider_id,
                    &version,
                    &target_entry.url,
                    target_entry.sha256.as_deref(),
                    map_archive_kind(target_entry.archive),
                    &target_entry.bin_path,
                    resolve_install_args(args),
                    target,
                    stage,
                )
                .await?;
                if archive_bin_requires_node_runtime(
                    &target_entry.bin_path,
                    Path::new(&managed.command),
                ) {
                    *stage = "node";
                    for dependency_target in node_runtime_dependency_targets_for_install_target(
                        target,
                        std::env::consts::OS,
                    ) {
                        let node = ensure_node_runtime(
                            state,
                            install_id,
                            provider_id,
                            state.data_root(),
                            dependency_target,
                        )
                        .await
                        .context("ensuring managed Node runtime for archive-backed npm provider")?;
                        let dep_id = node_runtime_dependency_id(dependency_target);
                        if !dependency_ids.contains(&dep_id) {
                            dependency_ids.push(dep_id.clone());
                        }
                        implicit_managed_dependencies.push((
                            dep_id,
                            node_runtime_dependency_metadata(
                                state.data_root(),
                                &node,
                                dependency_target,
                            ),
                        ));
                        if provider_id == "gemini" && dependency_target == target {
                            let entrypoint_path = managed.command.clone();
                            managed.command = node.node_bin.to_string_lossy().to_string();
                            managed.args.insert(0, entrypoint_path);
                        }
                    }
                }
                Ok(managed)
            }
        }
        provider_matrix::ProviderInstall::Python {
            package,
            version,
            entrypoint,
            args,
            targets,
            python_version,
            python_build_tag,
        } => {
            if !matches!(
                target,
                InstallTarget::Host
                    | InstallTarget::Container
                    | InstallTarget::LinuxAarch64
                    | InstallTarget::LinuxX8664
            ) {
                anyhow::bail!(
                    "target '{requested_target_label}' is not supported for python provider '{provider_id}' installs; use target host, container, linux-aarch64, or linux-x86_64",
                );
            }
            if provider_matrix::normalize_version(version)
                != provider_matrix::normalize_version(release_version)
            {
                anyhow::bail!(
                    "provider matrix version mismatch for {provider_id}: release={release_version} install={version}",
                );
            }
            *error_package = Some(package.clone());
            *error_version = Some(version.clone());
            *error_install_dir_rel =
                Some(format!("providers/agent-servers/{provider_id}/{version}",));
            if matches!(target, InstallTarget::Host) && !targets.contains_key(resolved_target_key) {
                install_managed_python_provider(
                    state,
                    install_id,
                    provider_id,
                    package,
                    version,
                    entrypoint,
                    python_version.as_deref(),
                    python_build_tag.as_deref(),
                    resolve_install_args(args),
                    target,
                    stage,
                )
                .await
            } else {
                let target_entry = targets.get(resolved_target_key).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unsupported provider target {provider_id}: {resolved_target_key}"
                    )
                })?;
                install_managed_archive_provider(
                    state,
                    install_id,
                    provider_id,
                    version,
                    &target_entry.url,
                    target_entry.sha256.as_deref(),
                    map_archive_kind(target_entry.archive),
                    &target_entry.bin_path,
                    resolve_install_args(args),
                    target,
                    stage,
                )
                .await
            }
        }
        provider_matrix::ProviderInstall::Archive {
            version,
            args,
            targets,
        } => {
            if provider_matrix::normalize_version(version)
                != provider_matrix::normalize_version(release_version)
            {
                anyhow::bail!(
                    "provider matrix version mismatch for {provider_id}: release={release_version} install={version}",
                );
            }
            let target_entry = targets.get(resolved_target_key).ok_or_else(|| {
                anyhow::anyhow!("unsupported provider target {provider_id}: {resolved_target_key}")
            })?;
            *error_package = Some(target_entry.url.clone());
            *error_version = Some(version.clone());
            *error_install_dir_rel =
                Some(format!("providers/agent-servers/{provider_id}/{version}",));
            let managed = install_managed_archive_provider(
                state,
                install_id,
                provider_id,
                version,
                &target_entry.url,
                target_entry.sha256.as_deref(),
                map_archive_kind(target_entry.archive),
                &target_entry.bin_path,
                resolve_install_args(args),
                target,
                stage,
            )
            .await?;
            if archive_bin_requires_node_runtime(
                &target_entry.bin_path,
                Path::new(&managed.command),
            ) {
                *stage = "node";
                for dependency_target in
                    node_runtime_dependency_targets_for_install_target(target, std::env::consts::OS)
                {
                    let node = ensure_node_runtime(
                        state,
                        install_id,
                        provider_id,
                        state.data_root(),
                        dependency_target,
                    )
                    .await
                    .context("ensuring managed Node runtime for archive provider")?;
                    let dep_id = node_runtime_dependency_id(dependency_target);
                    if !dependency_ids.contains(&dep_id) {
                        dependency_ids.push(dep_id.clone());
                    }
                    implicit_managed_dependencies.push((
                        dep_id,
                        node_runtime_dependency_metadata(
                            state.data_root(),
                            &node,
                            dependency_target,
                        ),
                    ));
                }
            }
            Ok(managed)
        }
    }
}
