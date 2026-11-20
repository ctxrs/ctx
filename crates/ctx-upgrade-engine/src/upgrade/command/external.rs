//! Lifecycle handoff for an installer that publishes its own executable.

use std::{
    fs,
    io::{self, BufRead as _, IsTerminal as _, Read as _, Write as _},
    os::unix::fs::MetadataExt as _,
    path::Path,
};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;

use crate::upgrade::{
    install::{
        canonical_executable, current_install_path,
        ensure_hosted_transaction_inactive_under_installation_lock, installation_is_unmanaged_at,
        installation_transaction_exists, validate_mutable_executable,
    },
    state::{
        begin_manual_attempt_locked, write_state_error_locked, write_state_phase_locked,
        UpgradeLock, EXTERNAL_REPLACEMENT_SOURCE,
    },
    DaemonUpgradeLease, DaemonUpgradePort, UpgradeEngine,
};

impl<D: DaemonUpgradePort + ?Sized> UpgradeEngine<'_, D> {
    /// Quiesce an unmanaged installation while its owner publishes a verified candidate.
    ///
    /// The original executable and candidate must remain accessible until publication.
    /// The callback returns whether the owner requested commit. The observed executable
    /// determines which image to resume, even when the caller disconnects.
    pub fn with_external_replacement(
        &self,
        data_root: &Path,
        command_path: &Path,
        candidate_path: &Path,
        publish: impl FnOnce() -> Result<bool>,
    ) -> Result<bool> {
        let installed = canonical_executable(command_path)?;
        let candidate = canonical_executable(candidate_path)?;
        let running = current_install_path()?;
        if running != installed && running != candidate {
            bail!("run the installed or staged ctx executable to coordinate replacement");
        }
        let lock = UpgradeLock::acquire_for_installation(&installed)?;
        if canonical_executable(command_path)? != installed {
            bail!("the command path changed before the installation was locked");
        }
        ensure_hosted_transaction_inactive_under_installation_lock(&installed)?;
        if !installation_is_unmanaged_at(&installed) || installation_transaction_exists(&installed)?
        {
            bail!("external replacement requires an unmanaged ctx installation without a pending transaction");
        }
        validate_mutable_executable(&candidate, false)?;
        if !installation_is_unmanaged_at(&candidate) {
            bail!("external replacement candidate belongs to a managed ctx installation");
        }
        let original_identity = identity(&installed)?;
        let candidate_identity = identity(&candidate)?;
        if original_identity == candidate_identity {
            bail!("external replacement requires a different executable");
        }
        self.prepare_data_root(data_root)?;
        let attempt = begin_manual_attempt_locked(data_root, &lock, EXTERNAL_REPLACEMENT_SOURCE)?;
        write_state_phase_locked(&lock, &attempt, "quiescing")?;

        let publication = (|| {
            let handoff =
                self.daemon
                    .begin_for_installation(data_root, attempt.id(), &installed)?;
            let request = publish();
            let selected = canonical_executable(command_path)?;
            validate_mutable_executable(&selected, false)?;
            let selected_identity = identity(&selected)?;
            let replaced = selected_identity == candidate_identity;
            if !replaced && selected_identity != original_identity {
                bail!(
                    "the command path resolves to neither the original nor the staged executable"
                );
            }
            Ok((handoff, request, selected, replaced))
        })();

        let (handoff, request, selected, replaced) = publication.map_err(|error| {
            if let Err(write_error) =
                write_state_error_locked(data_root, &lock, &attempt, "error", &format!("{error:#}"))
            {
                return anyhow!(
                    "{error:#}; also could not finish the upgrade state: {write_error:#}"
                );
            }
            error
        })?;
        // The installation fence must be terminal before secondary roots can
        // start. The native handoff still owns the origin root until it is ready.
        write_state_phase_locked(
            &lock,
            &attempt,
            if replaced { "applied" } else { "up_to_date" },
        )?;
        handoff
            .resume_with(&selected)
            .context("resume ctx after external replacement")?;
        if request? && !replaced {
            bail!("commit requested before the staged executable was published");
        }
        Ok(replaced)
    }

    /// Coordinate external replacement over redirected stdin and stdout JSON pipes.
    pub fn run_external_replacement_stdio(
        &self,
        data_root: &Path,
        command_path: &Path,
        candidate_path: &Path,
    ) -> Result<()> {
        if io::stdin().is_terminal() || io::stdout().is_terminal() {
            bail!("external replacement requires redirected stdin and stdout");
        }
        let mut output = io::stdout().lock();
        let replaced =
            self.with_external_replacement(data_root, command_path, candidate_path, || {
                writeln!(
                    output,
                    "{}",
                    json!({"schema_version": 1, "status": "ready"})
                )?;
                output.flush()?;
                let mut input = Vec::new();
                io::stdin().lock().take(7).read_until(b'\n', &mut input)?;
                match input.as_slice() {
                    b"commit\n" => Ok(true),
                    b"abort\n" | b"" => Ok(false),
                    _ => bail!("expected commit or abort on the external replacement pipe"),
                }
            })?;
        let status = if replaced { "committed" } else { "aborted" };
        writeln!(output, "{}", json!({"schema_version": 1, "status": status}))?;
        Ok(output.flush()?)
    }
}

fn identity(path: &Path) -> Result<(u64, u64)> {
    let metadata = fs::metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}
