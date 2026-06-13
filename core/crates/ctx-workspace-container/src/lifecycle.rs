use super::*;

impl WorkspaceContainerOwner {
    pub async fn ensure_after_machine_ready(
        &self,
        mode: &SandboxCommandMode,
        request: EnsureWorkspaceContainerRequest<'_>,
    ) -> Result<WorkspaceContainer> {
        let EnsureWorkspaceContainerRequest {
            workspace,
            worktree,
            settings,
            daemon_host,
            daemon_port,
            observer,
            readiness,
        } = request;

        let name = workspace_container_name(workspace.id);
        let image = resolve_container_image(settings.image.as_deref());
        if matches!(settings.mount_mode, ContainerMountMode::DiskIsolated) {
            observe_log(
                observer,
                HarnessSetupPhase::ContainerCheck,
                ctx_sandbox_container_runtime::HarnessSetupLogLevel::Info,
                "ensuring workspace volume for disk-isolated mode",
            );
            let _ = ensure_workspace_volume(&self.data_root, mode, workspace.id).await?;
        }
        let mount_plan = container::build_mounts(&self.data_root, workspace, worktree, settings);
        let mut recreate = false;
        observe_phase(
            observer,
            HarnessSetupPhase::ContainerCheck,
            "checking existing workspace container",
        );
        let cached_container = {
            let containers = self.containers.lock().await;
            containers.get(&workspace.id).cloned()
        };
        if let Some(container) = cached_container {
            match cached_container_action(&container, settings, &mount_plan.external_mounts) {
                CachedContainerAction::Reuse => {
                    let exists = container_exists(&self.data_root, mode, &name).await?;
                    let running = if exists {
                        container_running(&self.data_root, mode, &name)
                            .await?
                            .unwrap_or(false)
                    } else {
                        false
                    };
                    if exists && running {
                        observe_log(
                            observer,
                            HarnessSetupPhase::ContainerCheck,
                            ctx_sandbox_container_runtime::HarnessSetupLogLevel::Info,
                            "container already ready in runtime cache",
                        );
                        return Ok(container);
                    }
                    observe_log(
                        observer,
                        HarnessSetupPhase::ContainerCheck,
                        ctx_sandbox_container_runtime::HarnessSetupLogLevel::Info,
                        if exists {
                            "runtime cache entry stale; workspace container is stopped and will be restarted"
                        } else {
                            "runtime cache entry stale; workspace container is missing and will be recreated"
                        },
                    );
                    self.containers.lock().await.remove(&workspace.id);
                }
                CachedContainerAction::Reconfigure => {
                    observe_log(
                        observer,
                        HarnessSetupPhase::ContainerCheck,
                        ctx_sandbox_container_runtime::HarnessSetupLogLevel::Info,
                        "container network policy changed; reconfiguring",
                    );
                }
                CachedContainerAction::Recreate => {
                    observe_log(
                        observer,
                        HarnessSetupPhase::ContainerCheck,
                        ctx_sandbox_container_runtime::HarnessSetupLogLevel::Info,
                        "container configuration changed; recreating",
                    );
                    self.containers.lock().await.remove(&workspace.id);
                    recreate = true;
                }
            }
        }

        if recreate {
            observe_phase(
                observer,
                HarnessSetupPhase::ContainerStartOrCreate,
                "recreating workspace container",
            );
            let mut cmd = sandbox_container_command(&self.data_root, mode)?;
            cmd.arg("rm").arg("-f").arg(&name);
            let _ = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await;
        }

        let mut recreate_for_terminal_contract = false;
        loop {
            let exists = if recreate || recreate_for_terminal_contract {
                false
            } else {
                container_exists(&self.data_root, mode, &name).await?
            };

            if exists {
                let running = container_running(&self.data_root, mode, &name)
                    .await?
                    .unwrap_or(false);
                if !running {
                    observe_phase(
                        observer,
                        HarnessSetupPhase::ContainerStartOrCreate,
                        "starting existing workspace container",
                    );
                    let mut cmd = sandbox_container_command(&self.data_root, mode)?;
                    cmd.arg("start").arg(&name);
                    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
                    if !output.status.success() {
                        let combined = command_output_message(&output);
                        if combined.is_empty() {
                            anyhow::bail!(
                                "container start failed for {name} (status: {})",
                                output.status
                            );
                        }
                        anyhow::bail!("container start failed for {name}: {combined}");
                    }
                } else {
                    observe_log(
                        observer,
                        HarnessSetupPhase::ContainerCheck,
                        ctx_sandbox_container_runtime::HarnessSetupLogLevel::Info,
                        "workspace container already running",
                    );
                }
            } else {
                let requires_front_loaded_image_readiness = !(settings.runtime
                    == ContainerRuntimeKind::NativeContainer
                    && readiness == WorkspaceContainerReadiness::RuntimeReady);
                if requires_front_loaded_image_readiness {
                    self.ensure_container_image_ready(mode, settings, observer)
                        .await?;
                }
                observe_phase(
                    observer,
                    HarnessSetupPhase::ContainerStartOrCreate,
                    "creating workspace container",
                );
                let mut cmd = sandbox_container_command(&self.data_root, mode)?;
                cmd.arg("run").arg("-d").arg("--name").arg(&name);
                cmd.arg("--hostname")
                    .arg(container::workspace_container_hostname(workspace));
                if container::should_use_keep_id_userns() {
                    cmd.arg("--userns=keep-id");
                }
                if let Some(user) = container::container_user() {
                    cmd.arg("--user").arg(user);
                }
                append_sandbox_container_launch_network_args(&mut cmd, settings);
                for mount in &mount_plan.mounts {
                    cmd.arg("--mount").arg(mount);
                }
                cmd.arg(&image);
                cmd.arg("/bin/sh")
                    .arg("-c")
                    .arg("while true; do sleep 100000; done");
                let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
                if !output.status.success() {
                    let combined = command_output_message(&output);
                    let combined_lower = combined.to_ascii_lowercase();
                    let can_adopt_existing = combined_lower.contains("name-store error")
                        || combined_lower.contains("already used by id");
                    if can_adopt_existing && container_exists(&self.data_root, mode, &name).await? {
                        observe_log(
                            observer,
                            HarnessSetupPhase::ContainerStartOrCreate,
                            ctx_sandbox_container_runtime::HarnessSetupLogLevel::Warn,
                            "container create reported an existing name; adopting the existing workspace container",
                        );
                        let running = container_running(&self.data_root, mode, &name)
                            .await?
                            .unwrap_or(false);
                        if !running {
                            observe_log(
                                observer,
                                HarnessSetupPhase::ContainerStartOrCreate,
                                ctx_sandbox_container_runtime::HarnessSetupLogLevel::Info,
                                "adopted workspace container is stopped; starting it",
                            );
                            let mut start = sandbox_container_command(&self.data_root, mode)?;
                            start.arg("start").arg(&name);
                            let output =
                                command_output_with_timeout(start, SANDBOX_OP_TIMEOUT).await?;
                            if !output.status.success() {
                                let combined = command_output_message(&output);
                                if combined.is_empty() {
                                    anyhow::bail!(
                                        "container start failed for {name} (status: {})",
                                        output.status
                                    );
                                }
                                anyhow::bail!("container start failed for {name}: {combined}");
                            }
                        }
                    } else if combined.is_empty() {
                        anyhow::bail!(
                            "container run failed for {name} (status: {})",
                            output.status
                        );
                    } else {
                        anyhow::bail!("container run failed for {name}: {combined}");
                    }
                }
            }

            match container::sync_container_terminal_identity(&self.data_root, mode, &name).await {
                Ok(()) => break,
                Err(err) if container::container_terminal_identity_missing_sudo(&err) => {
                    if recreate_for_terminal_contract {
                        return Err(err.context(
                            "workspace container still lacks terminal sudo support after recreation",
                        ));
                    }
                    if is_default_container_image(&image) {
                        observe_log(
                            observer,
                            HarnessSetupPhase::ImageLoad,
                            ctx_sandbox_container_runtime::HarnessSetupLogLevel::Info,
                            "reloading the default harness image to apply the terminal identity contract",
                        );
                        force_reload_default_container_image(&self.data_root, mode, observer)
                            .await?;
                    }
                    observe_log(
                        observer,
                        HarnessSetupPhase::ContainerStartOrCreate,
                        ctx_sandbox_container_runtime::HarnessSetupLogLevel::Info,
                        "workspace container predates the terminal identity contract; recreating",
                    );
                    let mut cmd = sandbox_container_command(&self.data_root, mode)?;
                    cmd.arg("rm").arg("-f").arg(&name);
                    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
                    if !output.status.success() {
                        let combined = command_output_message(&output);
                        if combined.is_empty() {
                            anyhow::bail!(
                                "container rm failed for {name} while refreshing terminal identity contract (status: {})",
                                output.status
                            );
                        }
                        anyhow::bail!(
                            "container rm failed for {name} while refreshing terminal identity contract: {combined}"
                        );
                    }
                    recreate_for_terminal_contract = true;
                }
                Err(err) => return Err(err),
            }
        }

        if matches!(settings.mount_mode, ContainerMountMode::DiskIsolated) {
            container::verify_disk_isolated_container_mounts(
                &self.data_root,
                mode,
                workspace,
                &name,
            )
            .await?;
        }

        observe_phase(
            observer,
            HarnessSetupPhase::RuntimeNetworkSetup,
            "configuring container network policy",
        );
        let egress_guard = network_policy_transition::apply_container_network_policy(
            &self.data_root,
            mode,
            workspace.id,
            &name,
            settings,
            daemon_host,
            daemon_port,
        )
        .await?
        .egress_guard;
        observe_log(
            observer,
            HarnessSetupPhase::RuntimeNetworkSetup,
            ctx_sandbox_container_runtime::HarnessSetupLogLevel::Info,
            "container network policy configured",
        );

        let container = WorkspaceContainer {
            name: name.clone(),
            mount_mode: settings.mount_mode.clone(),
            network_mode: settings.network_mode.clone(),
            allowlist: settings.allowlist.clone(),
            external_mounts: mount_plan.external_mounts,
            egress_guard,
        };
        self.containers
            .lock()
            .await
            .insert(workspace.id, container.clone());
        Ok(container)
    }

    async fn ensure_container_image_ready(
        &self,
        mode: &SandboxCommandMode,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()> {
        let image = resolve_container_image(settings.image.as_deref());
        observe_phase(
            observer,
            HarnessSetupPhase::ImageCheck,
            "checking harness image availability",
        );
        if container_image_present(&self.data_root, mode, &image).await? {
            observe_log(
                observer,
                HarnessSetupPhase::ImageCheck,
                ctx_sandbox_container_runtime::HarnessSetupLogLevel::Info,
                "harness image already present",
            );
            return Ok(());
        }
        observe_phase(
            observer,
            HarnessSetupPhase::ImageLoad,
            "loading harness image into local sandbox runtime",
        );
        ensure_container_image_available(&self.data_root, mode, &image, observer).await
    }
}
