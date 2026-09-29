//! Synthetic lifecycle transactions never contact a service manager or user store.
use super::*;
use crate::upgrade::{
    state::{begin_manual_attempt_locked, write_state_phase_locked, UpgradeLock},
    DaemonRestart, DaemonUpgradeLease, DaemonUpgradePort, ProductBuildIdentity, ReleaseTransport,
    UpgradeEngine, TEST_RELEASE_PROCESS, TEST_SEMANTIC_LAYOUT,
};
use ctx_history_platform::platform_security::{
    create_private_directory_all, restrict_private_file,
};
use std::{fs, time::Duration};

struct UnusedPorts;
impl ReleaseTransport for UnusedPorts {
    fn get_bytes_limited(&self, _: &str, _: usize) -> Result<Vec<u8>> {
        unreachable!()
    }
    fn download_artifact(&self, _: &str, _: &mut fs::File, _: u64, _: Duration) -> Result<u64> {
        unreachable!()
    }
}
impl DaemonUpgradeLease for UnusedPorts {
    fn wait_for_installation_quiescence(&self) -> Result<()> {
        unreachable!()
    }
    fn replacement_restart(&self) -> Option<DaemonRestart<'_>> {
        unreachable!()
    }
    fn resume_with(self, _: &Path) -> Result<()> {
        unreachable!()
    }
    fn transfer_to_replacement_helper(self, _: u32) -> Result<()> {
        unreachable!()
    }
    fn release_for_current_format_reexec(self) -> Result<()> {
        unreachable!()
    }
}
impl DaemonUpgradePort for UnusedPorts {
    type Lease = Self;
    fn begin(&self, _: &Path, _: &str) -> Result<Self> {
        panic!("rejected migration must not quiesce")
    }
    fn begin_current(&self, _: &Path, _: &str, _: &str, _: Option<u64>) -> Result<Self> {
        unreachable!()
    }
    fn mark_replacement_helper_handoff(&self, _: &Path, _: &str, _: u32) -> Result<()> {
        unreachable!()
    }
    fn complete_replacement_handoff(
        &self,
        _: &Path,
        _: &Path,
        _: &str,
        _: Option<DaemonRestart<'_>>,
    ) -> Result<()> {
        unreachable!()
    }
    fn finish_replacement_handoff(&self, _: &Path, _: &str) -> Result<()> {
        unreachable!()
    }
}

fn candidate_args(install: &Path, marker_source: &Path) -> Result<HostedTransactionArgs> {
    let digest = sha256_hex(&fs::read(std::env::current_exe()?)?);
    fs::write(marker_source, marker(install, &digest))?;
    restrict_private_file(marker_source)?;
    Ok(HostedTransactionArgs {
        action: HostedTransactionAction::Install,
        install_path: install.to_owned(),
        attempt_id: Some("ia_reinstall".into()),
        marker_source: Some(marker_source.to_owned()),
        ownership_source: None,
        binary_sha256: Some(digest),
    })
}

fn marker(install: &Path, digest: &str) -> String {
    json!({"schema_version": 1, "manager": "ctx-hosted-installer",
        "install_path": install, "platform": platform_key().unwrap(), "channel": "stable",
        "version": "2.1.1", "sha256": digest})
    .to_string()
}

pub(in crate::upgrade) fn assert_migration_refused_without_changes(
    data: &Path,
    install: &Path,
    journals: &[PathBuf],
    expected_error: &str,
) -> Result<()> {
    let mut paths = vec![
        install.to_owned(),
        install_marker_path(install),
        install.with_file_name(".ctx.upgrade-state.json"),
    ];
    paths.extend_from_slice(journals);
    let before = paths
        .iter()
        .map(fs::read)
        .collect::<std::io::Result<Vec<_>>>()?;
    let engine = UpgradeEngine::new(
        ProductBuildIdentity::new("2.1.1"),
        &UnusedPorts,
        &TEST_RELEASE_PROCESS,
        &TEST_SEMANTIC_LAYOUT,
        &UnusedPorts,
    );
    let error = engine
        .migrate_hosted_install(
            data,
            candidate_args(install, &data.join("candidate-marker.json"))?,
        )
        .unwrap_err();
    assert!(error.to_string().contains(expected_error), "{error:#}");
    for (path, bytes) in paths.iter().zip(before) {
        assert_eq!(fs::read(path)?, bytes, "changed {}", path.display());
    }
    Ok(())
}

#[test]
fn foreign_scheduler_rejects_migration_even_with_a_valid_hosted_journal() -> Result<()> {
    for hosted_journal in [false, true] {
        let temp = tempfile::tempdir()?;
        let install = temp.path().join("install/bin/ctx");
        let data = temp.path().join("data");
        create_private_directory_all(install.parent().unwrap())?;
        create_private_directory_all(&data)?;
        fs::write(&install, b"old executable")?;
        restrict_private_file(&install)?;
        fs::write(
            install_marker_path(&install),
            marker(&install, &sha256_hex(b"old executable")),
        )?;
        restrict_private_file(&install_marker_path(&install))?;
        let mut journals = Vec::new();
        if hosted_journal {
            let args = candidate_args(&install, &data.join("candidate-marker.json"))?;
            migration::set_hosted_install_fault_for_test(Some("binary_replaced"));
            let error = run(args).unwrap_err();
            assert!(
                error.to_string().contains("injected hosted install fault"),
                "{error:#}"
            );
            journals.push(journal_path(&install));
        }
        let attempt = {
            let lock = UpgradeLock::acquire_for_installation(&install)?;
            let attempt = begin_manual_attempt_locked(&data, &lock, "manual_apply")?;
            write_state_phase_locked(&lock, &attempt, "quiescing")?;
            attempt
        };
        assert_migration_refused_without_changes(&data, &install, &journals, "pending upgrade")?;
        let lock = UpgradeLock::acquire_for_installation(&install)?;
        assert!(crate::upgrade::state::write_state_error_locked(
            &data,
            &lock,
            &attempt,
            "error",
            "original owner recovered",
        )?);
    }
    Ok(())
}

#[test]
fn pending_uninstall_rejects_migration_without_poisoning_reinstall() -> Result<()> {
    // Direct retry, rejected intervening install, and state stranded by a released installer.
    for case in ["direct", "rejected_install", "stranded"] {
        let temp = tempfile::tempdir()?;
        let install = temp.path().join("install/bin/ctx");
        let data = temp.path().join("data");
        create_private_directory_all(install.parent().unwrap())?;
        create_private_directory_all(&data)?;
        let source = b"synthetic installed executable";
        fs::write(&install, source)?;
        restrict_private_file(&install)?;
        fs::write(
            install_marker_path(&install),
            marker(&install, &sha256_hex(source)),
        )?;
        restrict_private_file(&install_marker_path(&install))?;
        let state_path = install.with_file_name(".ctx.upgrade-state.json");
        let journal_path = journal_path(&install);
        let helper = uninstall_helper_path(&install);
        {
            let _lock = UpgradeLock::acquire_for_installation(&install)?;
            let mut journal = new_uninstall_journal(&install, "ia_uninstall")?;
            begin_fresh_uninstall(&journal_path, &journal, &helper)?;
            stage_file(&install, &helper, true)?;
            journal.phase = Phase::HelperStaged;
            write_journal(&journal_path, &journal)?;
        }
        let before = fs::read(&journal_path)?;
        if case != "direct" {
            let engine = UpgradeEngine::new(
                ProductBuildIdentity::new("2.1.1"),
                &UnusedPorts,
                &TEST_RELEASE_PROCESS,
                &TEST_SEMANTIC_LAYOUT,
                &UnusedPorts,
            );
            let error = engine
                .migrate_hosted_install(
                    &data,
                    candidate_args(&install, &temp.path().join("candidate.json"))?,
                )
                .unwrap_err();
            assert!(
                error.to_string().contains("pending hosted uninstall"),
                "{error:#}"
            );
            assert!(!state_path.exists());
            assert_eq!(fs::read(&journal_path)?, before);
            // A malformed journal also must fail before any scheduler write.
            fs::write(&journal_path, b"invalid journal")?;
            let error = engine
                .migrate_hosted_install(
                    &data,
                    candidate_args(&install, &temp.path().join("candidate.json"))?,
                )
                .unwrap_err();
            assert!(!error.to_string().is_empty());
            assert!(!state_path.exists());
            assert_eq!(fs::read(&journal_path)?, b"invalid journal");
            fs::write(&journal_path, &before)?;
        }
        if case == "stranded" {
            // Reproduce the old operation's scheduler writes using the real owner.
            let lock = UpgradeLock::acquire_for_installation(&install)?;
            let attempt = begin_manual_attempt_locked(&data, &lock, "hosted_migration")?;
            write_state_phase_locked(&lock, &attempt, "quiescing")?;
        }
        {
            let _lock = UpgradeLock::acquire_for_installation(&install)?;
            let mut journal = required_uninstall_journal(&journal_path, &install)?;
            journal.phase = Phase::Armed;
            write_journal(&journal_path, &journal)?;
            complete_uninstall_commit(&helper, &journal_path, &mut journal, &mut |_| Ok(()))?;
            remove_journal(&journal_path)?;
        }
        assert!(!install.exists());
        assert!(!install_marker_path(&install).exists());
        let args = candidate_args(&install, &temp.path().join("candidate.json"))?;
        let expected_digest = args.binary_sha256.clone().unwrap();
        run(args)?;
        assert_eq!(sha256_hex(&fs::read(&install)?), expected_digest);
        assert!(!journal_path.exists());
        if case == "stranded" {
            let state: Value = serde_json::from_slice(&fs::read(state_path)?)?;
            assert_eq!(state["status"], "error");
            assert_eq!(state["attempt_source"], "hosted_migration");
        }
    }
    Ok(())
}

#[test]
fn removed_migration_recovery_preserves_other_fences() -> Result<()> {
    for case in [
        "manual_apply",
        "applying",
        "installed",
        "marker",
        "upgrade_journal",
        "pair_journal",
        "hosted_journal",
    ] {
        let temp = tempfile::tempdir()?;
        let install = temp.path().join("install/bin/ctx");
        create_private_directory_all(install.parent().unwrap())?;
        fs::write(&install, b"synthetic executable")?;
        restrict_private_file(&install)?;
        let lock = UpgradeLock::acquire_for_installation(&install)?;
        fs::remove_file(&install)?;
        let attempt = begin_manual_attempt_locked(
            temp.path(),
            &lock,
            if case == "manual_apply" {
                "manual_apply"
            } else {
                "hosted_migration"
            },
        )?;
        write_state_phase_locked(
            &lock,
            &attempt,
            if case == "applying" {
                "applying"
            } else {
                "quiescing"
            },
        )?;
        let state_path = install.with_file_name(".ctx.upgrade-state.json");
        let before = fs::read(&state_path)?;
        let witness = match case {
            "installed" => Some(install.clone()),
            "marker" => Some(install_marker_path(&install)),
            "upgrade_journal" => {
                Some(install.with_file_name(".ctx.upgrade-install-transaction.json"))
            }
            "pair_journal" => Some(
                temp.path()
                    .join("install")
                    .join(ctx_managed_pair_engine::MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH),
            ),
            "hosted_journal" => Some(journal_path(&install)),
            _ => None,
        };
        if let Some(path) = witness {
            create_private_directory_all(path.parent().unwrap())?;
            fs::write(&path, b"retained witness")?;
            restrict_private_file(&path)?;
        }
        let _ = crate::upgrade::state::recover_removed_hosted_migration_under_installation_lock(
            &install,
        );
        assert_eq!(fs::read(state_path)?, before, "must preserve {case}");
        assert!(crate::upgrade::state::ensure_legacy_pair_scheduler_terminal(&install).is_err());
    }
    Ok(())
}
