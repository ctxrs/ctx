use super::*;

impl ManagedPairEngine {
    pub fn new(install_root: impl Into<PathBuf>) -> Result<Self> {
        let install_root = install_root.into();
        filesystem::validate_absolute_root(&install_root, "managed-pair install root")?;
        Ok(Self { install_root })
    }

    pub fn install_root(&self) -> &Path {
        &self.install_root
    }

    pub fn begin(&self, verifier: &dyn ManagedPairVerifier) -> Result<ManagedPairAttempt> {
        let layout = Layout::open(&self.install_root, true)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        reject_legacy_transaction(&self.install_root)?;
        if uninstall::present(&layout)? {
            bail!("managed_pair_uninstall_active: managed-pair uninstall must finish first");
        }
        if let Some(journal) = journal::read(&layout)? {
            recover_for_new_attempt_locked(&layout, journal, verifier)?;
        }
        if let Some(attempt_id) = attempt::read_begin(&layout)? {
            let candidate_root = if filesystem::candidate_exists(&layout, &attempt_id)? {
                filesystem::candidate_root(&self.install_root, &attempt_id)?
            } else {
                filesystem::create_candidate(&self.install_root, &attempt_id)?
            };
            return Ok(ManagedPairAttempt {
                attempt_id,
                candidate_root,
            });
        }
        let attempt_id = Uuid::new_v4().simple().to_string();
        attempt::write_begin(&layout, &attempt_id)?;
        let candidate_root = filesystem::create_candidate(&self.install_root, &attempt_id)?;
        Ok(ManagedPairAttempt {
            attempt_id,
            candidate_root,
        })
    }

    pub fn stage_attempt(
        &self,
        attempt_id: &str,
        verifier: &dyn ManagedPairVerifier,
    ) -> Result<ManagedPairPrepared> {
        let candidate_root = filesystem::candidate_root(&self.install_root, attempt_id)?;
        let result = self.stage_with_fault(
            &candidate_root,
            verifier,
            Some(attempt_id.to_owned()),
            &|_| {},
        );
        let cleanup: Result<()> = (|| -> Result<()> {
            let layout = Layout::open(&self.install_root, false)?;
            let _lock = filesystem::acquire_lock(&layout)?;
            if attempt::read_begin(&layout)?.as_deref() == Some(attempt_id) {
                filesystem::remove_candidate(&layout, attempt_id)?;
                attempt::remove_begin(&layout, attempt_id)?;
                layout.remove_empty_candidate_base()?;
            }
            if result.is_err() && journal::read(&layout)?.is_none() {
                attempt::write_terminal(
                    &layout,
                    attempt_id,
                    TerminalOutcome::Failed,
                    Some("staging_failed"),
                )?;
            }
            Ok(())
        })();
        match (result, cleanup) {
            (Ok(prepared), Ok(())) => Ok(prepared),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error).context("clean managed-pair candidate"),
            (Err(error), Err(cleanup)) => Err(anyhow!(
                "stage managed pair: {error:#}; candidate cleanup failed: {cleanup:#}"
            )),
        }
    }

    pub fn status(&self, attempt_id: &str) -> Result<ManagedPairTransactionStatus> {
        if !journal::valid_attempt_id(attempt_id) {
            bail!("managed-pair attempt ID is invalid");
        }
        match std::fs::symlink_metadata(&self.install_root) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ManagedPairTransactionStatus::Absent);
            }
            Err(error) => return Err(error).context("inspect managed-pair install root"),
        }
        let layout = Layout::open(&self.install_root, false)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        reject_legacy_transaction(&self.install_root)?;
        if let Some(journal) = journal::read(&layout)? {
            if journal.attempt_id != attempt_id {
                bail!("a different managed-pair transaction is active");
            }
            return Ok(match journal.phase() {
                Phase::Staging => ManagedPairTransactionStatus::Staging,
                Phase::Staged => ManagedPairTransactionStatus::Staged,
                Phase::Deferred => ManagedPairTransactionStatus::Deferred,
                Phase::Activating => ManagedPairTransactionStatus::Activating,
                Phase::Committed => ManagedPairTransactionStatus::Committed,
                Phase::RollingBack => ManagedPairTransactionStatus::RollingBack,
            });
        }
        if attempt::read_begin(&layout)?.as_deref() == Some(attempt_id) {
            return Ok(ManagedPairTransactionStatus::Begun);
        }
        Ok(match attempt::read_terminal(&layout)? {
            Some(receipt) if receipt.attempt_id == attempt_id => match receipt.outcome {
                TerminalOutcome::Committed => ManagedPairTransactionStatus::Committed,
                TerminalOutcome::Aborted => ManagedPairTransactionStatus::Aborted,
                TerminalOutcome::Failed => ManagedPairTransactionStatus::Failed,
            },
            _ => ManagedPairTransactionStatus::Absent,
        })
    }

    pub fn abort(&self, attempt_id: &str) -> Result<bool> {
        if !journal::valid_attempt_id(attempt_id) {
            bail!("managed-pair attempt ID is invalid");
        }
        let layout = Layout::open(&self.install_root, false)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        reject_legacy_transaction(&self.install_root)?;
        if uninstall::present(&layout)? {
            bail!("managed_pair_uninstall_active: managed-pair uninstall must finish first");
        }
        let mut changed = false;
        if let Some(mut journal) = journal::read(&layout)? {
            if journal.attempt_id != attempt_id {
                bail!("a different managed-pair transaction is active");
            }
            if matches!(journal.phase(), Phase::Activating | Phase::Committed) {
                bail!("managed-pair activation can no longer be aborted");
            }
            rollback(&layout, &mut journal)?;
            attempt::write_terminal(
                &layout,
                attempt_id,
                TerminalOutcome::Aborted,
                Some("aborted"),
            )?;
            changed = true;
        }
        if attempt::read_begin(&layout)?.as_deref() == Some(attempt_id) {
            filesystem::remove_candidate(&layout, attempt_id)?;
            attempt::remove_begin(&layout, attempt_id)?;
            layout.remove_empty_candidate_base()?;
            attempt::write_terminal(
                &layout,
                attempt_id,
                TerminalOutcome::Aborted,
                Some("aborted"),
            )?;
            changed = true;
        }
        Ok(changed)
    }

    pub fn prepare_uninstall(
        &self,
        verifier: &dyn ManagedPairVerifier,
    ) -> Result<ManagedPairUninstallAttempt> {
        let layout = Layout::open(&self.install_root, false)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        reject_legacy_transaction(&self.install_root)?;
        Ok(ManagedPairUninstallAttempt {
            attempt_id: uninstall::prepare(&layout, verifier)?,
        })
    }

    pub fn run_post_exit_uninstall_after_parent_exit(
        &self,
        attempt_id: &str,
        parent_pid: u32,
        parent_creation_time: Option<u64>,
    ) -> Result<bool> {
        if !journal::valid_attempt_id(attempt_id) {
            bail!("managed-pair uninstall attempt ID is invalid");
        }
        #[cfg(windows)]
        {
            let creation = parent_creation_time
                .filter(|value| *value != 0)
                .ok_or_else(|| anyhow!("managed-pair parent creation identity is absent"))?;
            filesystem::wait_for_parent_exit(parent_pid, creation)?;
        }
        #[cfg(unix)]
        {
            let _ = parent_creation_time;
            wait_for_unix_parent_exit(parent_pid)?;
        }
        let layout = Layout::open(&self.install_root, false)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        reject_legacy_transaction(&self.install_root)?;
        uninstall::execute(&layout, attempt_id)
    }

    /// Stages the two fixed components, the exact signed envelope, and a
    /// deterministic local state marker without changing active files.
    pub fn stage(
        &self,
        candidate_root: &Path,
        verifier: &dyn ManagedPairVerifier,
    ) -> Result<ManagedPairPrepared> {
        self.stage_with_fault(candidate_root, verifier, None, &|_| {})
    }

    /// Activates directly on Unix. Windows records a durable deferred request;
    /// the caller must launch a helper that invokes `run_post_exit_swapper`.
    pub fn activate(
        &self,
        prepared: &ManagedPairPrepared,
        verifier: &dyn ManagedPairVerifier,
    ) -> Result<ManagedPairActivation> {
        let layout = Layout::open(&self.install_root, false)?;
        #[cfg(windows)]
        let _ = verifier;
        #[cfg(windows)]
        let _lock = filesystem::acquire_lock(&layout)?;
        #[allow(unused_mut)]
        let mut journal = journal::read(&layout)?
            .ok_or_else(|| anyhow!("managed-pair transaction disappeared before activation"))?;
        require_prepared(&journal, prepared)?;
        match journal.phase {
            Phase::Staged => {}
            Phase::Deferred if cfg!(windows) => {
                let parent_pid = journal
                    .parent_pid
                    .ok_or_else(|| anyhow!("deferred managed-pair transaction has no parent"))?;
                return Ok(ManagedPairActivation::PostExitRequired {
                    attempt_id: journal.attempt_id,
                    parent_pid,
                });
            }
            _ => bail!("managed-pair transaction is not ready for activation"),
        }

        #[cfg(windows)]
        {
            journal.phase = Phase::Deferred;
            journal.parent_pid = Some(process::id());
            journal.parent_creation_time = Some(filesystem::current_process_creation_identity()?);
            journal::write(&layout, &mut journal)?;
            return Ok(ManagedPairActivation::PostExitRequired {
                attempt_id: journal.attempt_id,
                parent_pid: process::id(),
            });
        }

        #[cfg(not(windows))]
        {
            self.commit(&layout, &journal.attempt_id, verifier, &|_| {})?;
            Ok(ManagedPairActivation::Activated)
        }
    }

    /// Entry point for a separately launched post-exit swapper. It reopens the
    /// durable journal and independently invokes the verifier before commit.
    pub fn run_post_exit_swapper(&self, verifier: &dyn ManagedPairVerifier) -> Result<()> {
        let layout = Layout::open(&self.install_root, false)?;
        let journal = journal::read(&layout)?
            .ok_or_else(|| anyhow!("managed-pair post-exit transaction is absent"))?;
        let attempt_id = journal.attempt_id.clone();
        #[cfg(windows)]
        {
            if journal.phase != Phase::Staged
                && journal.phase != Phase::Deferred
                && journal.phase != Phase::Activating
            {
                bail!("Windows managed-pair transaction is not deferred");
            }
            if journal.phase == Phase::Deferred {
                let parent_pid = journal
                    .parent_pid
                    .ok_or_else(|| anyhow!("Windows managed-pair transaction has no parent PID"))?;
                let parent_creation_time = journal.parent_creation_time.ok_or_else(|| {
                    anyhow!("Windows managed-pair transaction has no parent creation identity")
                })?;
                filesystem::wait_for_parent_exit(parent_pid, parent_creation_time)?;
            }
        }
        #[cfg(not(windows))]
        if journal.phase != Phase::Staged && journal.phase != Phase::Activating {
            bail!("managed-pair transaction is not staged for post-exit activation");
        }
        self.commit(&layout, &attempt_id, verifier, &|_| {})
    }

    pub fn run_post_exit_swapper_after_parent_exit(
        &self,
        attempt_id: &str,
        verifier: &dyn ManagedPairVerifier,
        parent_pid: u32,
        parent_creation_time: Option<u64>,
    ) -> Result<()> {
        #[cfg(windows)]
        {
            let creation = parent_creation_time
                .filter(|value| *value != 0)
                .ok_or_else(|| anyhow!("managed-pair parent creation identity is absent"))?;
            filesystem::wait_for_parent_exit(parent_pid, creation)?;
        }
        #[cfg(unix)]
        {
            let _ = parent_creation_time;
            wait_for_unix_parent_exit(parent_pid)?;
        }
        let layout = Layout::open(&self.install_root, false)?;
        self.commit(&layout, attempt_id, verifier, &|_| {})
    }

    /// Resolves an interrupted operation idempotently. A visible new state
    /// marker commits the complete pair; every earlier interruption rolls back.
    pub fn resume(&self, verifier: &dyn ManagedPairVerifier) -> Result<ManagedPairRecovery> {
        let layout = match Layout::open(&self.install_root, false) {
            Ok(layout) => layout,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(ManagedPairRecovery::None)
            }
            Err(error) => return Err(error),
        };
        let _lock = filesystem::acquire_lock(&layout)?;
        if uninstall::present(&layout)? {
            bail!("managed_pair_uninstall_active: managed-pair uninstall must finish first");
        }
        let Some(mut journal) = journal::read(&layout)? else {
            return Ok(ManagedPairRecovery::None);
        };
        journal.validate_for(&layout)?;
        match journal.phase {
            Phase::Staging | Phase::RollingBack => {
                rollback(&layout, &mut journal)?;
                Ok(ManagedPairRecovery::RolledBack)
            }
            Phase::Staged => {
                verify_staged(&layout, &journal, verifier)?;
                Ok(ManagedPairRecovery::Staged {
                    prepared: ManagedPairPrepared {
                        attempt_id: journal.attempt_id,
                        identity: journal.identity,
                    },
                })
            }
            Phase::Deferred => {
                verify_staged(&layout, &journal, verifier)?;
                Ok(ManagedPairRecovery::PostExitRequired {
                    attempt_id: journal.attempt_id,
                    parent_pid: journal.parent_pid.ok_or_else(|| {
                        anyhow!("deferred managed-pair transaction has no parent PID")
                    })?,
                })
            }
            Phase::Activating => {
                if active_matches_staged(&layout, &journal, verifier)? {
                    journal.phase = Phase::Committed;
                    journal::write(&layout, &mut journal)?;
                    finish_committed(&layout, &journal)?;
                    Ok(ManagedPairRecovery::Activated)
                } else {
                    rollback(&layout, &mut journal)?;
                    Ok(ManagedPairRecovery::RolledBack)
                }
            }
            Phase::Committed => {
                validate_active(&layout, verifier)?;
                finish_committed(&layout, &journal)?;
                Ok(ManagedPairRecovery::Activated)
            }
        }
    }

    /// Independently verifies all four active fixed slots.
    pub fn validate_active(
        &self,
        verifier: &dyn ManagedPairVerifier,
    ) -> Result<VerifiedManagedPairIdentity> {
        let layout = Layout::open(&self.install_root, false)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        validate_active(&layout, verifier)
    }

    pub(super) fn stage_with_fault(
        &self,
        candidate_root: &Path,
        verifier: &dyn ManagedPairVerifier,
        fixed_attempt_id: Option<String>,
        fault: &dyn Fn(&str),
    ) -> Result<ManagedPairPrepared> {
        filesystem::validate_absolute_root(candidate_root, "managed-pair candidate root")?;
        let layout = Layout::open(&self.install_root, true)?;
        let _lock = filesystem::acquire_lock(&layout)?;
        if uninstall::present(&layout)? {
            bail!("managed_pair_uninstall_active: managed-pair uninstall must finish first");
        }
        if let Some(expected_attempt) = fixed_attempt_id.as_deref() {
            if attempt::read_begin(&layout)?.as_deref() != Some(expected_attempt) {
                bail!("managed-pair stage attempt is not the active Core-generated attempt");
            }
        }
        let candidate = Layout::open_candidate(candidate_root)?;
        layout.revalidate()?;
        candidate.revalidate()?;
        if journal::read(&layout)?.is_some() {
            bail!("an interrupted managed-pair transaction must be resumed first");
        }

        let envelope = filesystem::read_regular(
            &candidate.target(Slot::Envelope),
            MAX_ENVELOPE_BYTES,
            "managed-pair signed envelope",
        )?;
        candidate.revalidate()?;
        let identity = verifier
            .verify_signed_envelope(&envelope.bytes)
            .context("verify managed-pair signed envelope")?;
        validate_verified_identity(&identity)?;
        validate_retained_pair(&layout, verifier, &identity)?;

        let state = ManagedPairState::new(identity.clone(), &envelope);
        let state_bytes = state.to_bytes()?;
        if state_bytes.len() as u64 > MAX_STATE_BYTES {
            bail!("managed-pair state exceeds its bound");
        }
        let attempt_id = fixed_attempt_id.unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let mut journal = Journal::new(
            attempt_id.clone(),
            identity.clone(),
            envelope.stamp.sha256.clone(),
            envelope.stamp.size_bytes,
        );
        for slot in Slot::ALL {
            journal.original[slot.index()] =
                filesystem::stamp_optional(&layout.target(slot), max_bytes(slot), slot.label())?;
            filesystem::require_absent(&layout.staged(slot, &attempt_id), slot.label())?;
            filesystem::require_absent(&layout.backup(slot, &attempt_id), slot.label())?;
        }
        journal::write_initial(&layout, &mut journal)?;
        fault("journal");

        let staged_result = (|| {
            layout.revalidate()?;
            candidate.revalidate()?;
            journal.staged[Slot::Core.index()] = Some(filesystem::copy_verified(
                &candidate.target(Slot::Core),
                &layout.staged(Slot::Core, &attempt_id),
                identity.core(),
                true,
                Slot::Core.label(),
            )?);
            journal::write(&layout, &mut journal)?;
            fault("stage_core");
            layout.revalidate()?;
            candidate.revalidate()?;
            journal.staged[Slot::Companion.index()] = Some(filesystem::copy_verified(
                &candidate.target(Slot::Companion),
                &layout.staged(Slot::Companion, &attempt_id),
                identity.companion(),
                true,
                Slot::Companion.label(),
            )?);
            journal::write(&layout, &mut journal)?;
            fault("stage_companion");
            layout.revalidate()?;
            candidate.revalidate()?;
            journal.staged[Slot::Envelope.index()] = Some(filesystem::write_new(
                &layout.staged(Slot::Envelope, &attempt_id),
                &envelope.bytes,
                false,
                Slot::Envelope.label(),
            )?);
            journal::write(&layout, &mut journal)?;
            fault("stage_envelope");
            layout.revalidate()?;
            candidate.revalidate()?;
            journal.staged[Slot::State.index()] = Some(filesystem::write_new(
                &layout.staged(Slot::State, &attempt_id),
                &state_bytes,
                false,
                Slot::State.label(),
            )?);
            journal::write(&layout, &mut journal)?;
            fault("stage_state");
            journal.phase = Phase::Staged;
            journal::write(&layout, &mut journal)?;
            layout.revalidate()?;
            candidate.revalidate()
        })();
        if let Err(error) = staged_result {
            let rollback_error = rollback(&layout, &mut journal).err();
            return match rollback_error {
                Some(rollback_error) => Err(anyhow!(
                    "stage managed pair: {error:#}; rollback failed: {rollback_error:#}"
                )),
                None => Err(error),
            };
        }
        Ok(ManagedPairPrepared {
            attempt_id,
            identity,
        })
    }

    pub(super) fn commit(
        &self,
        layout: &Layout,
        expected_attempt_id: &str,
        verifier: &dyn ManagedPairVerifier,
        fault: &dyn Fn(&str),
    ) -> Result<()> {
        let _lock = filesystem::acquire_lock(layout)?;
        if !journal::valid_attempt_id(expected_attempt_id) {
            bail!("managed-pair expected attempt ID is invalid");
        }
        if let Some(receipt) = attempt::read_terminal(layout)? {
            if receipt.attempt_id == expected_attempt_id
                && receipt.outcome == TerminalOutcome::Committed
            {
                return Ok(());
            }
        }
        let mut journal = journal::read(layout)?
            .ok_or_else(|| anyhow!("managed-pair transaction disappeared before commit"))?;
        if journal.attempt_id != expected_attempt_id {
            bail!("managed-pair transaction changed before commit");
        }
        journal.validate_for(layout)?;
        match journal.phase {
            Phase::Staged | Phase::Deferred => {
                verify_staged(layout, &journal, verifier)?;
                validate_retained_pair(layout, verifier, &journal.identity)?;
                verify_originals(layout, &journal)?;
                journal.phase = Phase::Activating;
                journal::write(layout, &mut journal)?;
                fault("activating");
            }
            Phase::Activating => verify_staged_or_published(layout, &journal)?,
            Phase::Committed => {
                validate_active(layout, verifier)?;
                return finish_committed(layout, &journal);
            }
            Phase::Staging | Phase::RollingBack => {
                bail!("managed-pair transaction is not ready for activation")
            }
        }
        let bound = journal::read(layout)?
            .ok_or_else(|| anyhow!("managed-pair transaction disappeared before mutation"))?;
        if bound.attempt_id != expected_attempt_id || bound.phase != Phase::Activating {
            bail!("managed-pair expected attempt changed immediately before mutation");
        }
        journal = bound;

        let result = (|| {
            for slot in Slot::BACKUP_ORDER {
                layout.revalidate()?;
                if let Some(expected) = journal.original[slot.index()].as_ref() {
                    let backup = match filesystem::stamp_optional(
                        &layout.backup(slot, &journal.attempt_id),
                        max_bytes(slot),
                        slot.label(),
                    )? {
                        Some(actual) => {
                            require_same_content(&actual, expected, slot.label())?;
                            actual
                        }
                        None => filesystem::copy_exact(
                            &layout.target(slot),
                            &layout.backup(slot, &journal.attempt_id),
                            expected,
                            max_bytes(slot),
                            matches!(slot, Slot::Core | Slot::Companion),
                            slot.label(),
                        )?,
                    };
                    journal.backups[slot.index()] = Some(backup);
                    journal::write(layout, &mut journal)?;
                }
                fault(slot.backup_fault());
                layout.revalidate()?;
            }
            for slot in Slot::PUBLISH_ORDER {
                layout.revalidate()?;
                let expected = journal.staged[slot.index()].as_ref().ok_or_else(|| {
                    anyhow!("managed-pair journal has no staged {}", slot.label())
                })?;
                if filesystem::matches_stamp(
                    &layout.target(slot),
                    expected,
                    max_bytes(slot),
                    slot.label(),
                )? {
                    filesystem::require_absent(
                        &layout.staged(slot, &journal.attempt_id),
                        slot.label(),
                    )?;
                } else if journal.original[slot.index()].is_some() {
                    filesystem::durable_replace(
                        &layout.staged(slot, &journal.attempt_id),
                        &layout.target(slot),
                        expected,
                        max_bytes(slot),
                        slot.label(),
                    )?;
                } else {
                    filesystem::rename_exact(
                        &layout.staged(slot, &journal.attempt_id),
                        &layout.target(slot),
                        expected,
                        max_bytes(slot),
                        slot.label(),
                    )?;
                }
                fault(slot.publish_fault());
                layout.revalidate()?;
            }
            Ok(())
        })();

        if let Err(error) = result {
            if active_matches_staged(layout, &journal, verifier)? {
                journal.phase = Phase::Committed;
                journal::write(layout, &mut journal)?;
                finish_committed(layout, &journal)?;
                return Ok(());
            }
            let rollback_error = rollback(layout, &mut journal).err();
            return match rollback_error {
                Some(rollback_error) => Err(anyhow!(
                    "activate managed pair: {error:#}; rollback failed: {rollback_error:#}"
                )),
                None => Err(error),
            };
        }

        validate_active(layout, verifier)?;
        journal.phase = Phase::Committed;
        journal::write(layout, &mut journal)?;
        finish_committed(layout, &journal)
    }
}
