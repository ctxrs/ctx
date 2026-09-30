//! Cross-owner Linux regression; no daemon processes or service managers run.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::Duration,
};

use anyhow::{bail, Result};
use ctx_daemon_runtime::read_installation_restart_records;
use ctx_history_platform::platform_security::{
    create_private_directory_all, restrict_private_file,
};
use ctx_upgrade_engine::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const PRIOR: &str = "ua_interrupted_hosted_migration";
const FOREIGN: &str = "ua_unrelated_migration";

struct Daemon {
    registrations: PathBuf,
    root_a: PathBuf,
    root_b: PathBuf,
    resume_prior: bool,
    attempt: Mutex<Option<String>>,
}

struct Lease<'a> {
    daemon: &'a Daemon,
    attempt: String,
}

fn acknowledge(
    registrations: &Path,
    name: &str,
    root: &Path,
    attempt: &str,
    interval: u64,
) -> Result<()> {
    let path = registrations.join(name);
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "schema_version": 1, "registration_id": name, "status": "acknowledged",
            "attempt_id": attempt, "data_root": root, "trigger_command": "search",
            "loop_interval_explicit": true, "loop_interval_seconds": interval,
        }))?,
    )?;
    restrict_private_file(&path)?;
    Ok(())
}

impl Daemon {
    fn assert_restart_set(&self, attempt: &str) -> Result<()> {
        let selected =
            read_installation_restart_records(&self.registrations, attempt, true, 3_600)?;
        let roots: Vec<_> = selected
            .iter()
            .map(|record| (record.data_root.as_path(), record.loop_interval_seconds))
            .collect();
        assert_eq!(
            roots,
            [
                (self.root_a.as_path(), Some(7)),
                (self.root_b.as_path(), Some(23))
            ],
            "retry lost acknowledged roots or their explicit cadence"
        );
        let foreign = read_installation_restart_records(&self.registrations, FOREIGN, true, 3_600)?;
        assert_eq!(foreign.len(), 1);
        assert_ne!(foreign[0].data_root, self.root_a);
        assert_ne!(foreign[0].data_root, self.root_b);
        Ok(())
    }
}

impl DaemonUpgradeLease for Lease<'_> {
    fn wait_for_installation_quiescence(&self) -> Result<()> {
        Ok(())
    }
    fn replacement_restart(&self) -> Option<DaemonRestart<'_>> {
        None
    }
    fn resume_with(self, executable: &Path) -> Result<()> {
        let state: Value = serde_json::from_slice(&fs::read(
            executable.with_file_name(".ctx.upgrade-state.json"),
        )?)?;
        assert_eq!(state["status"], "applied");
        assert_eq!(state["attempt_id"], self.attempt);
        self.daemon.assert_restart_set(&self.attempt)
    }
    fn transfer_to_replacement_helper(self, _: u32) -> Result<()> {
        unreachable!()
    }
    fn release_for_current_format_reexec(self) -> Result<()> {
        unreachable!()
    }
}

impl<'a> DaemonUpgradePort for &'a Daemon {
    type Lease = Lease<'a>;
    fn begin(&self, _: &Path, _: &str) -> Result<Self::Lease> {
        unreachable!()
    }
    fn begin_for_installation(
        &self,
        root: &Path,
        attempt: &str,
        install: &Path,
    ) -> Result<Self::Lease> {
        assert_eq!(root, self.root_a);
        assert!(try_acquire_managed_installation_mutation(install)?.is_none());
        *self.attempt.lock().unwrap() = Some(attempt.to_owned());
        if self.resume_prior {
            // Observe the real reader first: the old U2 path returns no roots.
            self.assert_restart_set(attempt)?;
            assert_eq!(attempt, PRIOR);
        } else {
            assert_ne!(
                attempt, PRIOR,
                "a new migration needs a fresh scheduler identity"
            );
            acknowledge(&self.registrations, "a.json", &self.root_a, attempt, 7)?;
            acknowledge(&self.registrations, "b.json", &self.root_b, attempt, 23)?;
            self.assert_restart_set(attempt)?;
        }
        Ok(Lease {
            daemon: self,
            attempt: attempt.to_owned(),
        })
    }
    fn begin_current(&self, _: &Path, _: &str, _: &str, _: Option<u64>) -> Result<Self::Lease> {
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

struct Unused;
impl ReleaseTransport for Unused {
    fn get_bytes_limited(&self, _: &str, _: usize) -> Result<Vec<u8>> {
        unreachable!()
    }
    fn download_artifact(&self, _: &str, _: &mut fs::File, _: u64, _: Duration) -> Result<u64> {
        unreachable!()
    }
}
impl ReleaseProcessPort for Unused {
    fn sanitize_release_authority_env<'a>(&self, _: &'a mut Command) -> &'a mut Command {
        unreachable!()
    }
}
impl SemanticLayoutPort for Unused {
    fn native_accelerator(&self) -> Option<SemanticAccelerator> {
        unreachable!()
    }
    fn managed_model_snapshot_dir(&self, _: &Path) -> PathBuf {
        unreachable!()
    }
    fn worker_cache_dir(&self, _: &Path) -> PathBuf {
        unreachable!()
    }
    fn runtime_cache_dir(&self, _: &Path) -> PathBuf {
        unreachable!()
    }
    fn model_contract_matches(&self, _: &SemanticModelContract<'_>) -> bool {
        unreachable!()
    }
    fn provisioning_model_path_count(&self) -> usize {
        unreachable!()
    }
    fn provisioning_model_path_matches(&self, _: &str) -> bool {
        unreachable!()
    }
    fn required_model_file_count(&self, _: SemanticModelVariant) -> usize {
        unreachable!()
    }
    fn required_model_file_matches(
        &self,
        _: SemanticModelVariant,
        _: &str,
        _: u64,
        _: &str,
    ) -> bool {
        unreachable!()
    }
    fn provisioning_coreml_asset_matches(&self, _: &str, _: &str, _: &str) -> bool {
        unreachable!()
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// A single test owns the fault environment variable in this test process.
#[test]
fn hosted_retry_keeps_all_acknowledged_roots_and_explicit_intervals() -> Result<()> {
    for case in [
        "released_gap",
        "released_prior_target",
        "published_gap",
        "quiescing_journal",
        "applied_journal",
        "fresh",
        "completed_other_target",
        "foreign",
    ] {
        let temp = tempfile::tempdir()?;
        let install = temp.path().join("install/bin/ctx");
        let root_a = temp.path().join("root-a");
        let root_b = temp.path().join("root-b");
        let registrations = temp.path().join("acknowledgements");
        for path in [install.parent().unwrap(), &root_a, &root_b, &registrations] {
            create_private_directory_all(path)?;
        }
        let candidate = fs::read(std::env::current_exe()?)?;
        let candidate_digest = digest(&candidate);
        let installed = if matches!(
            case,
            "fresh" | "completed_other_target" | "released_prior_target"
        ) {
            b"prior executable".as_slice()
        } else {
            candidate.as_slice()
        };
        fs::write(&install, installed)?;
        restrict_private_file(&install)?;
        let marker = |sha: &str| json!({"schema_version":1,"manager":"ctx-hosted-installer","install_path":install,"platform":"linux-x64","channel":"stable","version":"2.2.1","sha256":sha});
        let marker_path = install.with_file_name("ctx.install.json");
        fs::write(
            &marker_path,
            serde_json::to_vec(&marker(&digest(installed)))?,
        )?;
        restrict_private_file(&marker_path)?;
        let marker_source = temp.path().join("candidate-marker.json");
        fs::write(
            &marker_source,
            serde_json::to_vec(&marker(&candidate_digest))?,
        )?;
        restrict_private_file(&marker_source)?;
        let args = || HostedTransactionArgs {
            action: HostedTransactionAction::Install,
            install_path: install.clone(),
            attempt_id: Some("ia_candidate_retry".into()),
            marker_source: Some(marker_source.clone()),
            ownership_source: None,
            binary_sha256: Some(candidate_digest.clone()),
        };
        let journal = install.with_file_name(".ctx.hosted-install-transaction.json");
        if matches!(case, "quiescing_journal" | "applied_journal") {
            std::env::set_var("CTX_HOSTED_INSTALL_FAIL_AFTER_FOR_TESTS", "committed");
            let prepared = run_hosted_transaction(args());
            std::env::remove_var("CTX_HOSTED_INSTALL_FAIL_AFTER_FOR_TESTS");
            assert!(prepared
                .unwrap_err()
                .to_string()
                .contains("injected hosted install fault"));
            assert!(journal.exists());
        }
        let state_path = install.with_file_name(".ctx.upgrade-state.json");
        if case != "fresh" {
            fs::write(
                &state_path,
                serde_json::to_vec(&json!({
                    "schema_version":1, "attempt_id": PRIOR,
                    "attempt_source": if case == "foreign" { "manual_apply" } else { "hosted_migration" },
                    "status": if matches!(case, "applied_journal" | "published_gap" | "completed_other_target") { "applied" } else { "quiescing" },
                }))?,
            )?;
            restrict_private_file(&state_path)?;
        }
        let daemon = Daemon {
            registrations,
            root_a,
            root_b,
            resume_prior: !matches!(case, "fresh" | "completed_other_target"),
            attempt: Mutex::new(None),
        };
        acknowledge(
            &daemon.registrations,
            "foreign.json",
            &temp.path().join("foreign-root"),
            FOREIGN,
            41,
        )?;
        if daemon.resume_prior {
            acknowledge(&daemon.registrations, "a.json", &daemon.root_a, PRIOR, 7)?;
            acknowledge(&daemon.registrations, "b.json", &daemon.root_b, PRIOR, 23)?;
            // Reproduce the reviewed omission with the real selector: a new
            // scheduler identity cannot see either already-stopped root.
            assert!(read_installation_restart_records(
                &daemon.registrations,
                "ua_replacement_attempt",
                true,
                3_600,
            )?
            .is_empty());
            daemon.assert_restart_set(PRIOR)?;
        }
        let daemon_port = &daemon;
        let engine = UpgradeEngine::new(
            ProductBuildIdentity::new("2.2.1"),
            &Unused,
            &Unused,
            &Unused,
            &daemon_port,
        );
        let before = if case == "foreign" {
            Some(fs::read(&state_path)?)
        } else {
            None
        };
        let result = engine.migrate_hosted_install(&daemon.root_a, args());
        if let Some(before) = before {
            assert!(result.unwrap_err().to_string().contains("pending upgrade"));
            assert!(daemon.attempt.lock().unwrap().is_none());
            assert_eq!(fs::read(&state_path)?, before);
            daemon.assert_restart_set(PRIOR)?;
        } else {
            result.map_err(|error| anyhow::anyhow!("{case}: {error:#}"))?;
            let attempt = daemon.attempt.lock().unwrap().clone().unwrap();
            daemon.assert_restart_set(&attempt)?;
            assert!(!journal.exists(), "{case}");
            let state: Value = serde_json::from_slice(&fs::read(&state_path)?)?;
            assert_eq!(state["status"], "applied", "{case}");
            assert_eq!(state["attempt_id"], attempt, "{case}");
            if digest(&fs::read(&install)?) != candidate_digest {
                bail!("{case}: candidate changed");
            }
        }
    }
    Ok(())
}
