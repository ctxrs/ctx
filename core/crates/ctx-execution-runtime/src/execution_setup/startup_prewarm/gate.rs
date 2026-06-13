use super::*;

impl ExecutionSetupCoordinator {
    #[allow(dead_code)]
    pub(in crate::execution_setup) async fn compute_prewarm_gate(
        &self,
        settings: &crate::ContainerExecutionSettings,
    ) -> Result<PrewarmGate> {
        let (machine_ready, image_present) = self.startup_runtime_state(settings).await?;
        self.compute_prewarm_gate_with_runtime_state(settings, machine_ready, image_present)
            .await
    }

    pub(super) async fn compute_prewarm_gate_with_runtime_state(
        &self,
        settings: &crate::ContainerExecutionSettings,
        machine_ready: bool,
        image_present: bool,
    ) -> Result<PrewarmGate> {
        let target = ctx_harness_runtime::runtime_prewarm_target(settings);
        let metadata = read_prewarm_metadata(&self.data_root).await?;
        let bundled_image_fingerprint = match settings.runtime {
            crate::ContainerRuntimeKind::NativeContainer => {
                bundled_image_fingerprint(&target).await?
            }
            crate::ContainerRuntimeKind::SharedVmContainer => None,
        };

        let image_ref_changed = metadata
            .as_ref()
            .map(|meta| meta.image_ref != target)
            .unwrap_or(false);
        let bundled_image_digest_changed = match metadata.as_ref() {
            Some(meta) => meta.bundled_image_fingerprint != bundled_image_fingerprint,
            None => image_present && bundled_image_fingerprint.is_some(),
        };

        let needs_prewarm = needs_prewarm(
            machine_ready,
            image_present,
            image_ref_changed,
            bundled_image_digest_changed,
        );

        Ok(PrewarmGate {
            machine_ready,
            image_present,
            image_ref_changed,
            bundled_image_digest_changed,
            needs_prewarm,
            bundled_image_fingerprint,
        })
    }

    pub(crate) async fn startup_runtime_state(
        &self,
        settings: &crate::ContainerExecutionSettings,
    ) -> Result<(bool, bool)> {
        match settings.runtime {
            crate::ContainerRuntimeKind::NativeContainer => {
                let target = ctx_harness_runtime::resolve_container_image(settings);
                let machine_ready = normalize_container_engine_ready_for_gate(
                    ctx_harness_runtime::sandbox_engine_ready(&self.data_root).await,
                )?;
                let image_present = if machine_ready {
                    ctx_harness_runtime::container_image_present(&self.data_root, &target).await?
                } else {
                    false
                };
                Ok((machine_ready, image_present))
            }
            crate::ContainerRuntimeKind::SharedVmContainer => {
                ctx_harness_runtime::selected_runtime_launch_readiness_state(
                    &self.data_root,
                    settings,
                )
                .await
            }
        }
    }

    pub(in crate::execution_setup) async fn force_reload_stale_default_container_image_if_needed(
        &self,
        settings: &crate::ContainerExecutionSettings,
        gate: &PrewarmGate,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<bool> {
        if !gate.machine_ready || !gate.image_present || !gate.bundled_image_digest_changed {
            return Ok(false);
        }
        if !matches!(
            settings.runtime,
            crate::ContainerRuntimeKind::NativeContainer
        ) {
            return Ok(false);
        }
        let target = ctx_harness_runtime::runtime_prewarm_target(settings);
        if !ctx_sandbox_container_runtime::is_default_container_image(&target) {
            return Ok(false);
        }
        ctx_sandbox_container_runtime::force_reload_default_container_image(
            &self.data_root,
            &ctx_sandbox_container_runtime::SandboxCommandMode::NativeContainer,
            observer,
        )
        .await?;
        Ok(true)
    }
}
