use super::*;

mod process;

pub(in crate::daemon::workspace_runtime) use self::process::run_sandbox_machine_init;

pub(in crate::daemon::workspace_runtime) async fn initialize_sandbox_machine(
    data_root: &Path,
    machine_name: &str,
    memory_mb: Option<u32>,
    observer: Option<&dyn HarnessSetupObserver>,
    last_err: &mut String,
) -> Result<()> {
    let machine_image = if cfg!(target_os = "macos") {
        Some(
            ensure_managed_sandbox_machine_cache(data_root, observer, None)
                .await
                .context("managed sandbox machine cache unavailable")?,
        )
    } else {
        None
    };
    initialize_sandbox_machine_with_image(
        data_root,
        machine_name,
        machine_image.as_deref(),
        memory_mb,
        observer,
        last_err,
    )
    .await
}

pub(in crate::daemon::workspace_runtime) async fn initialize_sandbox_machine_with_image(
    data_root: &Path,
    machine_name: &str,
    machine_image: Option<&Path>,
    memory_mb: Option<u32>,
    observer: Option<&dyn HarnessSetupObserver>,
    last_err: &mut String,
) -> Result<()> {
    let init_outcome =
        run_sandbox_machine_init(data_root, machine_name, machine_image, memory_mb, observer)
            .await
            .context("sandbox machine init")?;
    let out = init_outcome.output;
    let combined = command_output_message(&out);
    if init_outcome.continued_after_machine_present {
        if !combined.is_empty() {
            *last_err = combined;
        }
    } else if !out.status.success() {
        let combined_lc = combined.to_ascii_lowercase();
        if combined_lc.contains("already exists") {
            let message = if combined.is_empty() {
                "sandbox machine init reported existing machine; starting it explicitly".to_string()
            } else {
                format!(
                    "sandbox machine init reported existing machine; starting it explicitly: {combined}"
                )
            };
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                &message,
            );
            if !combined.is_empty() {
                *last_err = combined;
            }
        } else if combined.is_empty() {
            anyhow::bail!("sandbox machine init failed (status: {})", out.status);
        } else {
            anyhow::bail!("sandbox machine init failed: {combined}");
        }
    }

    best_effort_start_machine_after_init(data_root, machine_name, observer, last_err).await
}
