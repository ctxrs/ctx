use std::{
    cell::Cell,
    io,
    sync::{Arc, Mutex},
};

use super::*;
use crate::ui::{ColorMode, RenderContext, StreamKind, TestContext};

const CORE_GENERATION_SENTINEL: &[u8] = b"generation-bound Core snapshot authority";
const SEMANTIC_INDEX_SENTINEL: &[u8] = b"v0.26 disposable semantic index";

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn test_ui(width: usize) -> (Ui, SharedWriter, SharedWriter) {
    let stdout = SharedWriter::default();
    let stderr = SharedWriter::default();
    let stdout_copy = stdout.clone();
    let stderr_copy = stderr.clone();
    let stdout_context = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never),
    );
    let stderr_context = RenderContext::for_test(
        TestContext::tty(StreamKind::Stderr, width).color(ColorMode::Never),
    );
    (
        Ui::with_writers(stdout, stdout_context, stderr, stderr_context),
        stdout_copy,
        stderr_copy,
    )
}

#[test]
fn lifecycle_human_recovery_preserves_manage_context() {
    let manage = ProArgs {
        command: Some(ProCommand::Manage(ProManageArgs {
            no_open: false,
            format: JsonOutputFormat::Text,
        })),
        format: JsonOutputFormat::Text,
        referral: None,
    };
    assert_eq!(render::human_retry_command(&manage), "ctx pro manage");

    let manage_without_browser = ProArgs {
        command: Some(ProCommand::Manage(ProManageArgs {
            no_open: true,
            format: JsonOutputFormat::Text,
        })),
        format: JsonOutputFormat::Text,
        referral: None,
    };
    assert_eq!(
        render::human_retry_command(&manage_without_browser),
        "ctx pro manage --no-open"
    );

    let setup = ProArgs {
        command: None,
        format: JsonOutputFormat::Text,
        referral: None,
    };
    assert_eq!(render::human_retry_command(&setup), "ctx pro");
}

fn run_uninstall(
    data_root: &Path,
    service: Option<&mut dyn ProDeletionService>,
    disposition: UninstallDataDisposition,
    json_output: bool,
) -> Result<serde_json::Value> {
    let (mut ui, _, _) = test_ui(80);
    super::run_uninstall(data_root, service, disposition, json_output, &mut ui)
}

#[derive(Default)]
struct RecordingDeletion {
    calls: Vec<&'static str>,
    fail_graph_key_deletion: bool,
}

impl ProDeletionService for RecordingDeletion {
    fn delete_commercial_credentials(&mut self, _data_root: &Path) -> Result<()> {
        self.calls.push("delete_commercial_credentials");
        Ok(())
    }

    fn delete_graph_data(&mut self, data_root: &Path) -> Result<()> {
        self.calls.push("delete_graph_data");
        if self.fail_graph_key_deletion {
            bail!("key_store_unavailable: simulated deletion failure");
        }
        let graph = ctx_pro_host_protocol::ProFilesystemLayout::new(data_root).graph_path();
        if graph.exists() {
            fs::remove_file(graph)?;
        }
        Ok(())
    }

    fn finish_deletion(&mut self, data_root: &Path) -> Result<()> {
        if default_helper_path(data_root).exists() {
            bail!("invalid_request: cleanup phase finished before helper deletion");
        }
        self.calls.push("finish_deletion");
        Ok(())
    }
}

struct EpochStorageFixture {
    core_generation: PathBuf,
    semantic_index: PathBuf,
}

impl EpochStorageFixture {
    fn write(data_root: &Path) -> Self {
        let core_generation = data_root
            .join("search/lexical")
            .join("ctx-generations")
            .join("core-generation.sentinel");
        let semantic_index = data_root
            .join("search/semantic")
            .join("fresh-epoch.sentinel");
        fs::create_dir_all(core_generation.parent().unwrap()).unwrap();
        fs::create_dir_all(semantic_index.parent().unwrap()).unwrap();
        fs::write(&core_generation, CORE_GENERATION_SENTINEL).unwrap();
        fs::write(&semantic_index, SEMANTIC_INDEX_SENTINEL).unwrap();
        Self {
            core_generation,
            semantic_index,
        }
    }

    fn assert_preserved(&self) {
        assert_eq!(
            fs::read(&self.core_generation).unwrap(),
            CORE_GENERATION_SENTINEL
        );
        assert_eq!(
            fs::read(&self.semantic_index).unwrap(),
            SEMANTIC_INDEX_SENTINEL
        );
    }
}

struct LifecycleFixture {
    root: tempfile::TempDir,
    helper: PathBuf,
    graph: PathBuf,
    epoch: EpochStorageFixture,
}

fn fixture() -> LifecycleFixture {
    let root = tempfile::tempdir().unwrap();
    let helper = default_helper_path(root.path());
    fs::create_dir_all(helper.parent().unwrap()).unwrap();
    ctx_history_core::platform_security::restrict_private_directory(
        &ProFilesystemLayout::new(root.path()).pro_root(),
    )
    .unwrap();
    write_local_pro_initialization_indicator(root.path()).unwrap();
    fs::write(&helper, b"helper").unwrap();
    let graph = ctx_pro_host_protocol::ProFilesystemLayout::new(root.path()).graph_path();
    fs::write(&graph, b"encrypted graph").unwrap();
    let epoch = EpochStorageFixture::write(root.path());
    LifecycleFixture {
        root,
        helper,
        graph,
        epoch,
    }
}

#[test]
fn repair_required_setup_rejects_a_missing_artifact() {
    let error = setup_artifact(&SetupInstallation::RepairRequired, None)
        .err()
        .unwrap()
        .to_string();
    assert_eq!(
        error,
        "invalid_response: Pro setup returned no helper artifact for an install or repair"
    );
}

#[test]
fn staged_activation_requires_exact_protocol_authorization_and_status() {
    let mut smoke = HelperSmoke {
        protocol_version: PROTOCOL_VERSION,
        protocol_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        helper_version: "0.1.0".to_owned(),
        capabilities: [Capability::EntitlementAuthorization, Capability::Status]
            .into_iter()
            .collect(),
    };
    validate_staged_helper(&smoke).unwrap();

    smoke
        .capabilities
        .remove(&Capability::EntitlementAuthorization);
    assert_eq!(
        validate_staged_helper(&smoke).unwrap_err().to_string(),
        "protocol_mismatch: staged Pro helper failed the activation smoke contract"
    );

    smoke
        .capabilities
        .insert(Capability::EntitlementAuthorization);
    smoke.protocol_fingerprint = "0".repeat(64);
    assert_eq!(
        validate_staged_helper(&smoke).unwrap_err().to_string(),
        "protocol_mismatch: staged Pro helper failed the activation smoke contract"
    );
}

#[test]
fn ordinary_uninstall_preserves_local_pro_data_and_fresh_epoch_authority() {
    let LifecycleFixture {
        root,
        helper,
        graph,
        epoch,
    } = fixture();
    run_uninstall(root.path(), None, UninstallDataDisposition::Keep, true).unwrap();
    assert!(!helper.exists());
    assert_eq!(fs::read(graph).unwrap(), b"encrypted graph");
    epoch.assert_preserved();
    assert!(preserved_data_marker_is_set(root.path()));
    let status = lifecycle_status_json(root.path());
    assert_eq!(status["state"], "uninstalled_data_preserved");
    assert_eq!(
        status["next_action"]["reason"],
        "restore_preserved_pro_data"
    );
}

#[test]
fn delete_data_confirms_key_before_removing_graph_and_credentials() {
    let LifecycleFixture {
        root,
        helper,
        graph,
        epoch,
    } = fixture();
    let mut service = RecordingDeletion::default();
    run_uninstall(
        root.path(),
        Some(&mut service),
        UninstallDataDisposition::Delete,
        true,
    )
    .unwrap();
    assert_eq!(
        service.calls,
        [
            "delete_graph_data",
            "delete_commercial_credentials",
            "finish_deletion",
        ]
    );
    assert!(!helper.exists());
    assert!(!graph.exists());
    epoch.assert_preserved();
    assert_eq!(
        uninstall_payload(true, LocalProDataOutcome::Deleted),
        json!({
            "schema_version": 1,
            "payload_type": "pro_uninstall",
            "uninstalled": true,
            "helper_removed": true,
            "local_pro_data": "deleted",
            "canonical_history_preserved": true,
            "next_action": {
                "command": "ctx pro",
                "reason": "rebuild_pro_data",
            },
        })
    );
}

#[test]
fn never_pro_missing_and_empty_roots_are_truthful_idempotent_noops() {
    for disposition in [
        UninstallDataDisposition::Delete,
        UninstallDataDisposition::Keep,
    ] {
        let parent = tempfile::tempdir().unwrap();
        let missing = parent.path().join("missing");
        let mut missing_service = RecordingDeletion::default();
        let value = run_uninstall(
            &missing,
            (disposition == UninstallDataDisposition::Delete)
                .then_some(&mut missing_service as &mut dyn ProDeletionService),
            disposition,
            true,
        )
        .unwrap();
        assert_eq!(value["local_pro_data"], "absent");
        assert_eq!(value["helper_removed"], false);
        assert_eq!(value["next_action"], serde_json::Value::Null);
        assert!(missing_service.calls.is_empty());
        assert!(!missing.exists());

        let empty = tempfile::tempdir().unwrap();
        crate::identity::installation_id(empty.path()).unwrap();
        let epoch = EpochStorageFixture::write(empty.path());
        let mut empty_service = RecordingDeletion::default();
        let value = run_uninstall(
            empty.path(),
            (disposition == UninstallDataDisposition::Delete)
                .then_some(&mut empty_service as &mut dyn ProDeletionService),
            disposition,
            true,
        )
        .unwrap();
        assert_eq!(value["local_pro_data"], "absent");
        assert_eq!(value["helper_removed"], false);
        assert_eq!(value["next_action"], serde_json::Value::Null);
        assert!(empty_service.calls.is_empty());
        assert!(!ProFilesystemLayout::new(empty.path()).pro_root().exists());
        epoch.assert_preserved();
    }

    let pristine = tempfile::tempdir().unwrap();
    crate::identity::installation_id(pristine.path()).unwrap();
    let mut production = LocalDeletionService::production();
    let value = run_uninstall(
        pristine.path(),
        Some(&mut production),
        UninstallDataDisposition::Delete,
        true,
    )
    .unwrap();
    assert_eq!(value["local_pro_data"], "absent");
    assert!(!ProFilesystemLayout::new(pristine.path())
        .pro_root()
        .exists());
}

#[test]
fn interrupted_artifact_fetch_retains_cleanup_evidence_until_verified_uninstall() {
    let root = tempfile::tempdir().unwrap();
    crate::identity::installation_id(root.path()).unwrap();
    let result = with_pro_initialization(root.path(), || -> Result<()> {
        assert!(local_pro_initialization_indicator_exists(root.path())?);
        bail!("artifact_download_failed: simulated interrupted fetch");
    });
    assert!(result
        .unwrap_err()
        .to_string()
        .starts_with("artifact_download_failed:"));
    assert!(local_pro_initialization_indicator_exists(root.path()).unwrap());

    let mut deletion = RecordingDeletion::default();
    let value = run_uninstall(
        root.path(),
        Some(&mut deletion),
        UninstallDataDisposition::Delete,
        true,
    )
    .unwrap();
    assert_eq!(
        deletion.calls,
        [
            "delete_graph_data",
            "delete_commercial_credentials",
            "finish_deletion",
        ]
    );
    assert_eq!(value["local_pro_data"], "absent");
    assert_eq!(value["next_action"], serde_json::Value::Null);
    assert!(!local_pro_initialization_indicator_exists(root.path()).unwrap());

    let mut repeated = RecordingDeletion::default();
    let value = run_uninstall(
        root.path(),
        Some(&mut repeated),
        UninstallDataDisposition::Delete,
        true,
    )
    .unwrap();
    assert!(repeated.calls.is_empty());
    assert_eq!(value["local_pro_data"], "absent");
}

#[cfg(target_os = "linux")]
#[test]
fn no_native_partial_bootstrap_delete_data_cleans_and_retries_without_helper_ipc() {
    let root = tempfile::tempdir().unwrap();
    crate::identity::installation_id(root.path()).unwrap();
    let result = with_pro_initialization(root.path(), || -> Result<()> {
        crate::pro::credential_vault::PlatformCredentialVault::store_file_fallback_installation_key_for_test(
            root.path(),
            crate::pro::credential_vault::CredentialVaultNamespace::Production,
            [0x42; ctx_pro_host_protocol::INSTALLATION_PUBLIC_KEY_BYTES],
        )?;
        bail!("artifact_download_failed: simulated failure before helper installation")
    });
    assert!(result
        .unwrap_err()
        .to_string()
        .starts_with("artifact_download_failed:"));

    let pro = ProFilesystemLayout::new(root.path()).pro_root();
    let selector = pro.join(".ctx-pro.credential-backend-v1");
    let file_vault = pro.join(".ctx-pro.credentials-v1");
    assert_eq!(
        fs::read(&selector).unwrap(),
        b"ctx-pro-credential-backend-v1:file\n"
    );
    assert!(file_vault.is_dir());
    assert!(!default_helper_path(root.path()).exists());
    assert!(!ProFilesystemLayout::new(root.path()).graph_path().exists());

    let mut deletion = LocalDeletionService::production();
    let value = run_uninstall(
        root.path(),
        Some(&mut deletion),
        UninstallDataDisposition::Delete,
        true,
    )
    .unwrap();
    assert_eq!(value["local_pro_data"], "absent");
    assert_eq!(value["helper_removed"], false);
    assert_eq!(value["next_action"], serde_json::Value::Null);
    for path in [
        selector.clone(),
        file_vault.clone(),
        pro.join(".ctx-pro-key-store-v1"),
        pro.join(".ctx-pro.initialized"),
        pro.join(".ctx-pro.graph-key-cleanup.json"),
        ProFilesystemLayout::new(root.path()).bin_dir(),
    ] {
        assert!(
            !path.exists(),
            "partial Pro artifact remained: {}",
            path.display()
        );
    }

    let mut entries_before_retry = fs::read_dir(&pro)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    entries_before_retry.sort();
    let mut retry = LocalDeletionService::production();
    let value = run_uninstall(
        root.path(),
        Some(&mut retry),
        UninstallDataDisposition::Delete,
        true,
    )
    .unwrap();
    assert_eq!(value["local_pro_data"], "absent");
    assert!(!selector.exists());
    assert!(!file_vault.exists());
    let mut entries_after_retry = fs::read_dir(&pro)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    entries_after_retry.sort();
    assert_eq!(entries_after_retry, entries_before_retry);
}

#[test]
fn interrupted_deletion_blocks_setup_and_keep_until_delete_retry() {
    let root = tempfile::tempdir().unwrap();
    let installation_id = crate::identity::installation_id(root.path()).unwrap();
    let layout = ProFilesystemLayout::new(root.path());
    fs::create_dir(layout.pro_root()).unwrap();
    ctx_history_core::platform_security::restrict_private_directory(&layout.pro_root()).unwrap();
    let phase = layout.pro_root().join(".ctx-pro.graph-key-cleanup.json");
    crate::pro::local_deletion::write_empty_graph_key_cleanup_phase_for_test(
        root.path(),
        &installation_id,
    )
    .unwrap();

    let mut operation_called = false;
    let error = with_pro_initialization(root.path(), || -> Result<()> {
        operation_called = true;
        Ok(())
    })
    .unwrap_err();
    assert!(error.to_string().starts_with("key_store_unavailable:"));
    assert!(!operation_called);

    let error = run_uninstall(root.path(), None, UninstallDataDisposition::Keep, true).unwrap_err();
    assert!(error.to_string().starts_with("key_store_unavailable:"));
    assert!(phase.exists());
}

#[test]
fn helper_without_graph_still_triggers_vault_cleanup_but_reports_absent() {
    let root = tempfile::tempdir().unwrap();
    crate::identity::installation_id(root.path()).unwrap();
    let layout = ProFilesystemLayout::new(root.path());
    fs::create_dir(layout.pro_root()).unwrap();
    ctx_history_core::platform_security::restrict_private_directory(&layout.pro_root()).unwrap();
    fs::create_dir(layout.bin_dir()).unwrap();
    fs::write(layout.helper_path(), b"helper").unwrap();

    let mut deletion = RecordingDeletion::default();
    let value = run_uninstall(
        root.path(),
        Some(&mut deletion),
        UninstallDataDisposition::Delete,
        true,
    )
    .unwrap();
    assert_eq!(
        deletion.calls,
        [
            "delete_graph_data",
            "delete_commercial_credentials",
            "finish_deletion",
        ]
    );
    assert_eq!(value["local_pro_data"], "absent");
    assert_eq!(value["helper_removed"], true);
    assert_eq!(value["next_action"], serde_json::Value::Null);
}

#[test]
fn keep_data_marks_only_real_graph_data() {
    let root = tempfile::tempdir().unwrap();
    let pro_root = ProFilesystemLayout::new(root.path()).pro_root();
    fs::create_dir(&pro_root).unwrap();
    let stale_marker = ProFilesystemLayout::new(root.path()).preserved_data_marker_path();
    fs::write(&stale_marker, PRESERVED_DATA_MARKER_CONTENT).unwrap();

    assert!(!preserved_data_marker_is_set(root.path()));
    let value = run_uninstall(root.path(), None, UninstallDataDisposition::Keep, true).unwrap();
    assert_eq!(value["local_pro_data"], "absent");
    assert_eq!(value["next_action"], serde_json::Value::Null);
    assert!(!stale_marker.exists());
    assert!(!preserved_data_marker_is_set(root.path()));
}

#[test]
fn failed_key_deletion_preserves_helper_graph_and_credentials() {
    let LifecycleFixture {
        root,
        helper,
        graph,
        epoch,
    } = fixture();
    let mut service = RecordingDeletion {
        fail_graph_key_deletion: true,
        ..RecordingDeletion::default()
    };
    let error = run_uninstall(
        root.path(),
        Some(&mut service),
        UninstallDataDisposition::Delete,
        true,
    )
    .unwrap_err();
    assert!(error.to_string().starts_with("key_store_unavailable:"));
    assert_eq!(service.calls, ["delete_graph_data"]);
    assert!(helper.exists());
    assert!(graph.exists());
    epoch.assert_preserved();
}

fn pro_status(access_state: &str) -> ProStatus {
    let locked = access_state == "locked";
    let operations = std::collections::BTreeSet::from([
        ProOperation::FileBlame,
        ProOperation::CommitBlame,
        ProOperation::PullRequestBlame,
    ]);
    ProStatus {
        schema_version: 1,
        installed: true,
        ready: !locked,
        materialized: true,
        helper_path: PathBuf::from("/redacted"),
        helper_version: Some("0.26.0".to_owned()),
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec!["status".to_owned()],
        error_code: locked.then(|| "entitlement_expired".to_owned()),
        projection_currentness: Some(CoreProjectionCurrentness::Current),
        materialized_coverage: Some(MaterializedCoverage::Complete),
        repository_coverage: Some(RepositoryCoverage {
            repository_candidate_events: 6,
            logical_binding_events: 5,
            certified_live_root_access_events: 4,
            file_evidence_events: 3,
            exact_commit_evidence_events: 2,
            exact_pull_request_evidence_events: 1,
        }),
        core_preparation_peak_workers: Some(4),
        storage_evidence: Some(ProStorageEvidence {
            graph_manifest_schema: 3,
            flat_format_version: 2,
            materializer_checkpoint_version: 3,
            journal_pack_format_version: 3,
            legacy_journals_written: 0,
            journal_pages_written: 2,
            journal_packs_written: 1,
            journal_finish_activity: ctx_pro_host_protocol::JournalFinishActivity {
                worker_limit: 1,
                peak_workers: 1,
                started_after_preparation: true,
            },
        }),
        supported_operations: Some(operations.clone()),
        available_operations: Some(operations),
        access_state: Some(access_state.to_owned()),
        refresh_after_unix: Some(100),
        access_deadline_unix: Some(200),
        grace_deadline_unix: Some(300),
        setup_repairability: ProSetupRepairability::NotNeeded,
    }
}

#[test]
fn lifecycle_status_keeps_readiness_separate_from_access_transitions() {
    for access_state in [
        "trial",
        "active",
        "canceling_paid",
        "offline_grace",
        "locked",
    ] {
        let value = lifecycle_status_value(pro_status(access_state), false);
        assert_eq!(value["access_state"], access_state);
        assert_eq!(value["refresh_after_unix"], 100);
        assert_eq!(value["access_deadline_unix"], 200);
        assert_eq!(value["grace_deadline_unix"], 300);
        assert_eq!(
            value["state"],
            if access_state == "locked" {
                "locked"
            } else {
                "ready"
            }
        );
    }
}

#[test]
fn lifecycle_status_fails_closed_for_invalid_helper_response_axes() {
    for error_code in ["invalid_response", "protocol_mismatch"] {
        let mut contradictory = pro_status("active");
        contradictory.error_code = Some(error_code.to_owned());

        let value = lifecycle_status_value(contradictory, false);
        assert_eq!(value["ready"], false, "{error_code}");
        assert_eq!(value["materialized"], false, "{error_code}");
    }
}

#[test]
fn lifecycle_status_preserves_terminal_quiet_coverage_without_fallback() {
    for coverage in [MaterializedCoverage::Empty, MaterializedCoverage::Abstained] {
        let mut helper = pro_status("active");
        helper.ready = false;
        helper.materialized_coverage = Some(coverage);
        helper.repository_coverage = Some(RepositoryCoverage::default());
        helper.available_operations = Some(std::collections::BTreeSet::new());

        let value = lifecycle_status_value(helper, false);
        assert_eq!(value["state"], "not_blame_ready", "{coverage:?}");
        assert_eq!(value["ready"], false, "{coverage:?}");
        assert_eq!(value["materialized"], true, "{coverage:?}");
        assert_eq!(value["error_code"], serde_json::Value::Null, "{coverage:?}");
        assert_eq!(value["projection_currentness"], "current", "{coverage:?}");
        assert_eq!(
            value["materialized_coverage"],
            if coverage == MaterializedCoverage::Empty {
                "empty"
            } else {
                "abstained"
            },
            "{coverage:?}"
        );
        assert_eq!(
            value["repository_coverage"],
            serde_json::json!({
                "repository_candidate_events": 0,
                "logical_binding_events": 0,
                "certified_live_root_access_events": 0,
                "file_evidence_events": 0,
                "exact_commit_evidence_events": 0,
                "exact_pull_request_evidence_events": 0,
            }),
            "{coverage:?}"
        );
        assert_eq!(
            value["supported_operations"],
            serde_json::json!(["file_blame", "commit_blame", "pull_request_blame"]),
            "{coverage:?}"
        );
        assert_eq!(value["available_operations"], serde_json::json!([]));
        assert_eq!(value["next_action"]["command"], serde_json::Value::Null);
        assert_eq!(
            value["next_action"]["reason"],
            "no_available_blame_operations"
        );
    }
}

#[test]
fn lifecycle_status_renders_repository_readiness_axes_independently() {
    let value = lifecycle_status_value(pro_status("active"), false);

    assert_eq!(
        value["repository_coverage"]["repository_candidate_events"],
        6
    );
    assert_eq!(value["repository_coverage"]["logical_binding_events"], 5);
    assert_eq!(
        value["repository_coverage"]["certified_live_root_access_events"],
        4
    );
    assert_eq!(value["repository_coverage"]["file_evidence_events"], 3);
    assert_eq!(
        value["repository_coverage"]["exact_commit_evidence_events"],
        2
    );
    assert_eq!(
        value["repository_coverage"]["exact_pull_request_evidence_events"],
        1
    );
    assert_eq!(value["core_preparation_peak_workers"], 4);
    assert_eq!(value["storage_evidence"]["graph_manifest_schema"], 3);
    assert_eq!(value["storage_evidence"]["flat_format_version"], 2);
    assert_eq!(
        value["storage_evidence"]["materializer_checkpoint_version"],
        3
    );
    assert_eq!(value["storage_evidence"]["journal_pack_format_version"], 3);
    assert_eq!(value["storage_evidence"]["legacy_journals_written"], 0);
    assert_eq!(value["storage_evidence"]["journal_pages_written"], 2);
    assert_eq!(value["storage_evidence"]["journal_packs_written"], 1);
    assert_eq!(
        value["storage_evidence"]["journal_finish_activity"]["worker_limit"],
        1
    );
    assert_eq!(
        value["storage_evidence"]["journal_finish_activity"]["peak_workers"],
        1
    );
    assert_eq!(
        value["storage_evidence"]["journal_finish_activity"]["started_after_preparation"],
        true
    );

    let mut without_evidence = pro_status("active");
    without_evidence.storage_evidence = None;
    assert_eq!(
        lifecycle_status_value(without_evidence, false)["storage_evidence"],
        serde_json::Value::Null
    );
}

#[test]
fn ready_materialized_setup_replay_skips_commercial_mutation_only_for_current_access() {
    for access_state in ["trial", "active", "canceling_paid"] {
        assert_eq!(
            reusable_setup_access_state(&pro_status(access_state), false, None).as_deref(),
            Some(access_state)
        );
    }

    assert!(reusable_setup_access_state(&pro_status("active"), true, None).is_none());
    assert!(
        reusable_setup_access_state(&pro_status("active"), false, Some("agent-smith")).is_none()
    );
    assert!(reusable_setup_access_state(&pro_status("offline_grace"), false, None).is_none());

    let mut stale = pro_status("active");
    stale.materialized = false;
    assert!(reusable_setup_access_state(&stale, false, None).is_none());
    stale.materialized = true;
    stale.ready = false;
    assert!(reusable_setup_access_state(&stale, false, None).is_none());
}

#[test]
fn lifecycle_status_distinguishes_invalid_artifacts_from_never_installed() {
    let mut invalid = pro_status("active");
    invalid.installed = false;
    invalid.ready = false;
    invalid.materialized = false;
    invalid.helper_version = None;
    invalid.capabilities.clear();
    invalid.error_code = Some("invalid_response".to_owned());
    invalid.access_state = None;
    invalid.refresh_after_unix = None;
    invalid.access_deadline_unix = None;
    invalid.grace_deadline_unix = None;
    invalid.setup_repairability = ProSetupRepairability::Automated;

    let value = lifecycle_status_value(invalid.clone(), false);
    assert_eq!(value["state"], "repair_required");
    assert_eq!(value["installed"], false);
    assert_eq!(value["error_code"], "invalid_response");
    assert_eq!(value["next_action"]["command"], "ctx pro");
    assert_eq!(value["next_action"]["reason"], "helper_artifacts_invalid");

    invalid.setup_repairability = ProSetupRepairability::ManualDiagnosis;
    let value = lifecycle_status_value(invalid.clone(), false);
    assert_eq!(value["state"], "unavailable");
    assert_eq!(value["next_action"]["command"], serde_json::Value::Null);
    assert_eq!(value["next_action"]["reason"], "manual_diagnosis_required");

    invalid.error_code = Some("pro_not_installed".to_owned());
    invalid.setup_repairability = ProSetupRepairability::NotNeeded;
    let value = lifecycle_status_value(invalid, false);
    assert_eq!(value["state"], "not_setup");
    assert_eq!(value["next_action"]["reason"], "helper_missing");
}

#[test]
fn manage_json_has_one_exact_nonsecret_access_shape() {
    let plan = ProManagePlan {
        portal_url: "https://billing.example.test/session".to_owned(),
        access_state: "canceling_paid".to_owned(),
        refresh_after_unix: Some(100),
        access_deadline_unix: Some(200),
        grace_deadline_unix: Some(300),
    };
    assert_eq!(
        manage_payload(&plan, false),
        json!({
            "schema_version": 1,
            "payload_type": "pro_manage",
            "portal_url": "https://billing.example.test/session",
            "browser_opened": false,
            "access_state": "canceling_paid",
            "refresh_after_unix": 100,
            "access_deadline_unix": 200,
            "grace_deadline_unix": 300,
        })
    );
    for access_state in ["trial", "active", "canceling_paid"] {
        validate_access_status(access_state, None, Some(200), None).unwrap();
    }
    validate_access_status("offline_grace", None, Some(200), Some(300)).unwrap();
    validate_access_status("locked", None, None, None).unwrap();
    assert!(validate_access_status("none", None, None, None).is_err());
    assert!(!serde_json::to_string_pretty(&manage_payload(&plan, false))
        .unwrap()
        .contains('\u{1b}'));
}

#[test]
fn manage_json_never_invokes_the_browser_opener() {
    struct ManageService;

    impl ProLifecycleService for ManageService {
        fn release_trust(&self) -> Result<ReleaseTrust> {
            bail!("unused")
        }

        fn setup(
            &mut self,
            _data_root: &Path,
            _installed_version: Option<&str>,
            _trial_only: bool,
            _referral_codename: Option<&str>,
            _ui: &mut Ui,
            human_output: bool,
            _browser_enabled: bool,
        ) -> Result<ProSetupPlan> {
            assert!(!human_output);
            bail!("unused")
        }

        fn manage(
            &mut self,
            _data_root: &Path,
            _ui: &mut Ui,
            human_output: bool,
            browser_enabled: bool,
        ) -> Result<ProManagePlan> {
            assert!(!human_output);
            assert!(!browser_enabled);
            Ok(ProManagePlan {
                portal_url: "https://billing.example.test/session".to_owned(),
                access_state: "active".to_owned(),
                refresh_after_unix: Some(100),
                access_deadline_unix: Some(200),
                grace_deadline_unix: None,
            })
        }
    }

    let root = tempfile::tempdir().unwrap();
    let calls = Cell::new(0);
    let opener = |_: &str| {
        calls.set(calls.get() + 1);
        Ok(())
    };
    let mut telemetry = ProLifecycleTelemetryV1::new(ProLifecycleOperationV1::Manage);
    let (mut ui, stdout, stderr) = test_ui(80);
    run_manage_with_opener(
        root.path(),
        &mut ManageService,
        false,
        true,
        &mut telemetry,
        &mut ui,
        &opener,
    )
    .unwrap();
    assert_eq!(calls.get(), 0);
    assert!(stdout.text().is_empty());
    assert!(stderr.text().is_empty());
}

#[test]
fn manage_no_open_routes_the_primary_result_to_ui_stdout_only() {
    struct ManageService;

    impl ProLifecycleService for ManageService {
        fn release_trust(&self) -> Result<ReleaseTrust> {
            bail!("unused")
        }

        fn setup(
            &mut self,
            _data_root: &Path,
            _installed_version: Option<&str>,
            _trial_only: bool,
            _referral_codename: Option<&str>,
            _ui: &mut Ui,
            human_output: bool,
            _browser_enabled: bool,
        ) -> Result<ProSetupPlan> {
            assert!(human_output);
            bail!("unused")
        }

        fn manage(
            &mut self,
            _data_root: &Path,
            _ui: &mut Ui,
            human_output: bool,
            browser_enabled: bool,
        ) -> Result<ProManagePlan> {
            assert!(human_output);
            assert!(!browser_enabled);
            Ok(ProManagePlan {
                portal_url: "https://billing.example.test/session".to_owned(),
                access_state: "trial".to_owned(),
                refresh_after_unix: Some(100),
                access_deadline_unix: Some(200),
                grace_deadline_unix: None,
            })
        }
    }

    let root = tempfile::tempdir().unwrap();
    let calls = Cell::new(0);
    let opener = |_: &str| {
        calls.set(calls.get() + 1);
        Ok(())
    };
    let mut telemetry = ProLifecycleTelemetryV1::new(ProLifecycleOperationV1::Manage);
    let (mut ui, stdout, stderr) = test_ui(80);
    run_manage_with_opener(
        root.path(),
        &mut ManageService,
        true,
        false,
        &mut telemetry,
        &mut ui,
        &opener,
    )
    .unwrap();
    assert_eq!(calls.get(), 0);
    assert!(stdout
        .text()
        .starts_with("✓ ctx Pro account management is ready\n"));
    assert!(stdout
        .text()
        .contains("Management link  https://billing.example.test/session"));
    assert!(stdout
        .text()
        .contains("Enabled; no aggregate facts recorded yet"));
    assert!(stdout
        .text()
        .contains("Continue with ctx Pro for $20/month."));
    assert!(stdout.text().contains("ctx pro manage"));
    assert!(stderr.text().is_empty());
}

#[test]
fn uninstall_human_routes_the_durable_result_to_ui_stdout() {
    let LifecycleFixture { root, epoch, .. } = fixture();
    let (mut ui, stdout, stderr) = test_ui(80);
    super::run_uninstall(
        root.path(),
        None,
        UninstallDataDisposition::Keep,
        false,
        &mut ui,
    )
    .unwrap();
    assert!(stdout.text().starts_with("✓ ctx Pro was removed\n"));
    assert!(stdout
        .text()
        .contains("Pro graph              Preserved locally"));
    assert!(stdout.text().contains("ctx pro\n"));
    assert!(stderr.text().is_empty());
    epoch.assert_preserved();
}

#[test]
fn ordinary_uninstall_then_delete_and_repeated_delete_are_idempotent() {
    let LifecycleFixture {
        root,
        helper,
        graph,
        epoch,
    } = fixture();
    run_uninstall(root.path(), None, UninstallDataDisposition::Keep, true).unwrap();
    assert!(!helper.exists());
    assert!(graph.exists());

    let mut first = RecordingDeletion::default();
    run_uninstall(
        root.path(),
        Some(&mut first),
        UninstallDataDisposition::Delete,
        true,
    )
    .unwrap();
    assert!(!graph.exists());
    assert!(!preserved_data_marker_is_set(root.path()));
    epoch.assert_preserved();

    let mut repeated = RecordingDeletion::default();
    let value = run_uninstall(
        root.path(),
        Some(&mut repeated),
        UninstallDataDisposition::Delete,
        true,
    )
    .unwrap();
    assert!(repeated.calls.is_empty());
    assert_eq!(value["local_pro_data"], "absent");
    assert_eq!(value["next_action"], serde_json::Value::Null);
    epoch.assert_preserved();
}

#[test]
fn tty_uninstall_prompt_is_exact_and_defaults_to_delete() {
    let mut input = std::io::Cursor::new(b"\n".to_vec());
    let (mut ui, stdout, stderr) = test_ui(80);
    assert_eq!(
        prompt_uninstall_data_disposition(&mut input, &mut ui).unwrap(),
        UninstallDataDisposition::Delete
    );
    assert_eq!(
        stderr.text(),
        "! Delete all local Pro data? It can be rebuilt if you set up Pro again. [Y/n]\n\
         Canonical ctx history is always preserved.\n"
    );
    assert!(stdout.text().is_empty());
}

#[test]
fn tty_uninstall_prompt_can_preserve_data_and_reprompts_invalid_input() {
    let mut input = std::io::Cursor::new(b"maybe\nn\n".to_vec());
    let (mut ui, stdout, stderr) = test_ui(80);
    assert_eq!(
        prompt_uninstall_data_disposition(&mut input, &mut ui).unwrap(),
        UninstallDataDisposition::Keep
    );
    assert_eq!(
        stderr.text(),
        "! Delete all local Pro data? It can be rebuilt if you set up Pro again. [Y/n]\n\
         Canonical ctx history is always preserved.\n\
         ! Please answer y or n.\n\
         ! Delete all local Pro data? It can be rebuilt if you set up Pro again. [Y/n]\n\
         Canonical ctx history is always preserved.\n"
    );
    assert!(stdout.text().is_empty());
}
