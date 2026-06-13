use super::*;

impl ExecutionSetupCoordinator {
    pub(super) async fn run_workspace_launch(
        self: Arc<Self>,
        job: Arc<LaunchJob>,
        workspace: Workspace,
        settings: ExecutionSettings,
        daemon_url: String,
    ) {
        #[cfg(test)]
        eprintln!("run_workspace_launch: begin workspace={:?}", workspace.id);
        let launch_started = std::time::Instant::now();
        let observer = LaunchObserver {
            coordinator: Arc::clone(&self),
            job: Arc::clone(&job),
        };
        let is_host_mode = matches!(settings.mode, ExecutionMode::Host);
        let run_result = if is_host_mode {
            Ok(())
        } else {
            async {
                let join_shared_runtime = self.prewarm.runtime_is_running(&settings, false).await;
                #[cfg(test)]
                eprintln!("run_workspace_launch: join_shared_runtime={join_shared_runtime}");

                if join_shared_runtime {
                    let reusable_container_exists = self
                        .harness
                        .workspace_container_exists(workspace.id)
                        .await
                        .context("failed to probe existing workspace container")?;
                    #[cfg(test)]
                    eprintln!(
                        "run_workspace_launch: reusable_container_exists={reusable_container_exists}"
                    );
                    if reusable_container_exists {
                        #[cfg(test)]
                        eprintln!("run_workspace_launch: ensure_workspace_container_with_observer");
                        return self
                            .harness
                            .ensure_workspace_container_with_observer(
                                &workspace,
                                &settings,
                                &daemon_url,
                                Some(&observer),
                            )
                            .await
                            .context("container runtime failed");
                    }

                    let _runtime_activity = self.harness.begin_runtime_operation();
                    self.harness
                        .ensure_container_machine_ready(&settings.container, Some(&observer))
                        .await
                        .context("sandbox runtime unavailable and execution mode is sandbox")?;

                    let joined_shared_runtime = match self
                        .prewarm
                        .attach_runtime_if_running(&settings, false, Some(&observer))
                        .await
                    {
                        Ok(joined) => joined,
                        Err(err) => {
                            observer.on_log(
                                HarnessSetupPhase::MachineStartOrInit,
                                HarnessSetupLogLevel::Warn,
                                &format!(
                                    "shared runtime warmup failed, continuing with direct launch: {}",
                                    format_error_chain(&err)
                                ),
                            );
                            false
                        }
                    };

                    if joined_shared_runtime {
                        let launch_ready = ctx_harness_runtime::selected_runtime_launch_ready(
                            &self.data_root,
                            &settings.container,
                        )
                        .await
                        .context("failed to inspect container runtime after shared warmup")?;
                        if launch_ready {
                            return self
                                .harness
                                .ensure_workspace_container_after_runtime_ready_with_observer(
                                    &workspace,
                                    &settings,
                                    &daemon_url,
                                    Some(&observer),
                                )
                                .await
                                .context("container runtime failed");
                        }
                    }

                    self.harness
                        .ensure_workspace_container_after_machine_ready_with_observer(
                            &workspace,
                            &settings,
                            &daemon_url,
                            Some(&observer),
                        )
                        .await
                        .context("container runtime failed")
                } else {
                    #[cfg(test)]
                    eprintln!("run_workspace_launch: direct ensure_workspace_container_with_observer");
                    self.harness
                        .ensure_workspace_container_with_observer(
                            &workspace,
                            &settings,
                            &daemon_url,
                            Some(&observer),
                        )
                        .await
                        .context("container runtime failed")
                }
            }
            .await
        };

        match run_result {
            Ok(()) => {
                #[cfg(test)]
                eprintln!("run_workspace_launch: success");
                if !matches!(settings.mode, ExecutionMode::Host) {
                    self.refresh_startup_prewarm_metadata_after_successful_container_launch(
                        &settings.container,
                    )
                    .await;
                    self.emit_phase(
                        &job,
                        HarnessSetupPhase::Ready,
                        ctx_harness_runtime::workspace_launch_ready_message(
                            &settings.container.runtime,
                        ),
                    );
                }
                let terminal = job.mark_terminal(ExecutionLaunchState::Ready, None);
                if let Some(completed) = terminal.completed_phase {
                    self.record_phase_metric(completed.phase, completed.elapsed_ms, "ready");
                }
                let _ = job.tx.send(ExecutionLaunchStreamEvent::LaunchComplete {
                    snapshot: terminal.snapshot.clone(),
                });
                self.record_launch_metric(launch_started.elapsed().as_millis() as u64, "ready");
            }
            Err(err) => {
                #[cfg(test)]
                eprintln!("run_workspace_launch: error={err:#}");
                let message = format_error_chain(&err);
                let phase = job
                    .current_phase()
                    .unwrap_or(HarnessSetupPhase::ContainerStartOrCreate);
                self.emit_log(&job, phase, HarnessSetupLogLevel::Error, &message);
                let terminal =
                    job.mark_terminal(ExecutionLaunchState::Error, Some(message.clone()));
                if let Some(completed) = terminal.completed_phase {
                    self.record_phase_metric(completed.phase, completed.elapsed_ms, "error");
                }
                let _ = job.tx.send(ExecutionLaunchStreamEvent::LaunchError {
                    snapshot: terminal.snapshot.clone(),
                });
                self.record_launch_metric(launch_started.elapsed().as_millis() as u64, "error");

                self.events.emit_event(
                    "error",
                    "execution.launch_error",
                    Some(json!({
                        "job_id": terminal.snapshot.job_id,
                        "workspace_id": terminal.snapshot.workspace_id,
                        "phase": terminal.snapshot.current_phase,
                        "error": message,
                    })),
                );
            }
        }

        self.clear_running_launch(workspace.id, &job.job_id).await;
    }

    pub(super) async fn run_runtime_prewarm(
        self: Arc<Self>,
        shared_job: Arc<SharedPrewarmLaunchJob>,
        settings: ExecutionSettings,
    ) {
        let launch_started = std::time::Instant::now();
        let job = shared_job.job();
        let observer = LaunchObserver {
            coordinator: Arc::clone(&self),
            job: Arc::clone(&job),
        };
        let is_host_mode = matches!(settings.mode, ExecutionMode::Host);
        let runtime_target = ctx_harness_runtime::runtime_prewarm_target(&settings.container);
        if is_host_mode {
            if let Some(terminal) = shared_job.complete_ready() {
                if let Some(completed) = terminal.completed_phase {
                    self.record_phase_metric(completed.phase, completed.elapsed_ms, "ready");
                }
                let _ = job.tx.send(ExecutionLaunchStreamEvent::LaunchComplete {
                    snapshot: terminal.snapshot.clone(),
                });
                self.record_launch_metric(launch_started.elapsed().as_millis() as u64, "ready");
            }
            self.clear_running_prewarm(&shared_job).await;
            return;
        }

        let run_result: Result<RequestedPrewarmScope> = async {
            let mut runtime_ready = false;
            let mut launch_ready = false;
            let mut builder_ready = false;

            loop {
                let requested_scope = shared_job.requested_scope();
                if requested_scope.requires_launch_ready_runtime() && !launch_ready {
                    if !ctx_harness_runtime::local_runtime_available(
                        &self.data_root,
                        &settings.container.runtime,
                    ) {
                        return Err(anyhow::anyhow!("local sandbox runtime unavailable"));
                    }
                    let gate = self.compute_prewarm_gate(&settings.container).await?;
                    self.force_reload_stale_default_container_image_if_needed(
                        &settings.container,
                        &gate,
                        Some(&observer),
                    )
                    .await?;
                    let _artifact_warmup = self.harness.begin_prewarm_artifact_activity();
                    self.prewarm
                        .ensure_runtime(&settings, true, Some(&observer))
                        .await?;
                    self.validate_runtime_prewarm_completion(&settings, &runtime_target, true)
                        .await?;
                    runtime_ready = true;
                    launch_ready = true;
                    continue;
                }

                if requested_scope.runtime_requested() && !runtime_ready {
                    if !ctx_harness_runtime::local_runtime_available(
                        &self.data_root,
                        &settings.container.runtime,
                    ) {
                        return Err(anyhow::anyhow!("local sandbox runtime unavailable"));
                    }
                    let gate = self.compute_prewarm_gate(&settings.container).await?;
                    self.force_reload_stale_default_container_image_if_needed(
                        &settings.container,
                        &gate,
                        Some(&observer),
                    )
                    .await?;
                    let _artifact_warmup = self.harness.begin_prewarm_artifact_activity();
                    self.prewarm
                        .ensure_runtime(&settings, false, Some(&observer))
                        .await?;
                    self.validate_runtime_prewarm_completion(&settings, &runtime_target, false)
                        .await?;
                    runtime_ready = true;
                    continue;
                }

                if requested_scope.builder_requested() && !builder_ready {
                    self.wait_for_builder_completion(observer.clone()).await?;
                    builder_ready = true;
                    continue;
                }

                if let Some(requested_scope) = shared_job
                    .reserve_ready_completion_if_scope_satisfied(
                        runtime_ready,
                        launch_ready,
                        builder_ready,
                    )
                {
                    return Ok(requested_scope);
                }

                tokio::task::yield_now().await;
            }
        }
        .await;

        match run_result {
            Ok(requested_scope) => {
                let runtime_kind = &settings.container.runtime;
                let ready_message = runtime_prewarm_ready_phase_message(
                    requested_scope.runtime_requested(),
                    runtime_kind,
                    requested_scope.requires_launch_ready_runtime(),
                );
                self.emit_phase(&job, HarnessSetupPhase::Ready, ready_message);
                let terminal = shared_job.mark_reserved_ready_terminal();
                if let Some(completed) = terminal.completed_phase {
                    self.record_phase_metric(completed.phase, completed.elapsed_ms, "ready");
                }
                let _ = job.tx.send(ExecutionLaunchStreamEvent::LaunchComplete {
                    snapshot: terminal.snapshot.clone(),
                });
                self.record_launch_metric(launch_started.elapsed().as_millis() as u64, "ready");
            }
            Err(err) => {
                self.finish_runtime_prewarm_error(shared_job, job, launch_started, err)
                    .await;
                return;
            }
        }

        self.clear_running_prewarm(&shared_job).await;
    }

    async fn validate_runtime_prewarm_completion(
        &self,
        settings: &ExecutionSettings,
        runtime_target: &str,
        requires_launch_ready_runtime: bool,
    ) -> Result<()> {
        if requires_launch_ready_runtime {
            match ctx_harness_runtime::selected_runtime_launch_readiness_state(
                &self.data_root,
                &settings.container,
            )
            .await
            {
                Ok((true, true)) => Ok(()),
                Ok((vm_ready, image_ready)) => Err(anyhow::anyhow!(
                    ctx_harness_runtime::launch_ready_gap_message(
                        settings.container.runtime.clone(),
                        runtime_target,
                        vm_ready,
                        image_ready,
                    )
                )),
                Err(err) => Err(err),
            }
        } else {
            match ctx_harness_runtime::selected_runtime_state(&self.data_root, &settings.container)
                .await
            {
                Ok((machine_ready, image_present)) if machine_ready && image_present => Ok(()),
                Ok((machine_ready, _image_present)) if machine_ready => Err(anyhow::anyhow!(
                    "runtime prewarm completed but runtime target '{runtime_target}' is still unavailable in the local sandbox runtime"
                )),
                Ok((_machine_ready, _image_present)) => Err(anyhow::anyhow!(
                    "runtime prewarm downloaded startup artifacts for '{runtime_target}', but the local sandbox runtime still needs first-launch startup"
                )),
                Err(err) => Err(err),
            }
        }
    }

    async fn finish_runtime_prewarm_error(
        &self,
        shared_job: Arc<SharedPrewarmLaunchJob>,
        job: Arc<LaunchJob>,
        launch_started: std::time::Instant,
        err: anyhow::Error,
    ) {
        let message = format_error_chain(&err);
        let phase = job.current_phase().unwrap_or(HarnessSetupPhase::ImageLoad);
        self.emit_log(&job, phase, HarnessSetupLogLevel::Error, &message);
        if let Some(terminal) = shared_job.complete_error(message.clone()) {
            if let Some(completed) = terminal.completed_phase {
                self.record_phase_metric(completed.phase, completed.elapsed_ms, "error");
            }
            let _ = job.tx.send(ExecutionLaunchStreamEvent::LaunchError {
                snapshot: terminal.snapshot.clone(),
            });
            self.record_launch_metric(launch_started.elapsed().as_millis() as u64, "error");

            self.events.emit_event(
                "error",
                "execution.runtime_prewarm_error",
                Some(json!({
                    "job_id": terminal.snapshot.job_id,
                    "phase": terminal.snapshot.current_phase,
                    "error": message,
                })),
            );
        }
        self.clear_running_prewarm(&shared_job).await;
    }

    async fn wait_for_builder_completion(self: &Arc<Self>, observer: LaunchObserver) -> Result<()> {
        let coordinator = Arc::clone(self);
        let builder_observer = observer;
        tokio::spawn(async move {
            coordinator
                .prewarm
                .ensure_builder(Some(&builder_observer))
                .await
        })
        .await
        .map_err(|err| anyhow::anyhow!("builder warmup task join failed: {err}"))?
    }

    async fn clear_running_prewarm(&self, shared_job: &Arc<SharedPrewarmLaunchJob>) {
        let mut inner = self.inner.lock().await;
        inner
            .prewarm_jobs
            .remove_if_current(shared_job.key(), shared_job);
    }

    pub(super) fn emit_phase(&self, job: &Arc<LaunchJob>, phase: HarnessSetupPhase, message: &str) {
        if job.is_terminal() {
            return;
        }
        let update = job.transition_phase(phase, message);
        if let Some(completed) = update.completed_phase {
            self.record_phase_metric(completed.phase, completed.elapsed_ms, "running");
        }
        if update.snapshot_changed {
            let _ = job.tx.send(ExecutionLaunchStreamEvent::LaunchSnapshot {
                snapshot: update.snapshot,
            });
        }
        if let Some(line) = update.line {
            let _ = job.tx.send(ExecutionLaunchStreamEvent::LaunchLog {
                job_id: job.job_id.clone(),
                line,
            });
        }
    }

    pub(super) fn emit_progress(&self, job: &Arc<LaunchJob>, progress: HarnessSetupProgressUpdate) {
        if job.is_terminal() {
            return;
        }
        let update = job.set_progress(progress);
        if update.snapshot_changed {
            let _ = job.tx.send(ExecutionLaunchStreamEvent::LaunchSnapshot {
                snapshot: update.snapshot,
            });
        }
    }

    pub(super) fn emit_log(
        &self,
        job: &Arc<LaunchJob>,
        phase: HarnessSetupPhase,
        level: HarnessSetupLogLevel,
        message: &str,
    ) {
        if job.is_terminal() {
            return;
        }
        let update = job.push_log(phase, level, message);
        if let Some(line) = update.line {
            let _ = job.tx.send(ExecutionLaunchStreamEvent::LaunchLog {
                job_id: job.job_id.clone(),
                line,
            });
        }
    }

    fn record_phase_metric(&self, phase: HarnessSetupPhase, elapsed_ms: u64, result: &'static str) {
        self.metrics
            .record_phase_duration(phase, elapsed_ms, result);
    }

    fn record_launch_metric(&self, elapsed_ms: u64, result: &'static str) {
        self.metrics.record_launch_duration(elapsed_ms, result);
    }

    async fn clear_running_launch(&self, workspace_id: WorkspaceId, job_id: &str) {
        let mut inner = self.inner.lock().await;
        if inner
            .running_launch_by_workspace
            .get(&workspace_id)
            .map(String::as_str)
            == Some(job_id)
        {
            inner.running_launch_by_workspace.remove(&workspace_id);
        }
    }
}
