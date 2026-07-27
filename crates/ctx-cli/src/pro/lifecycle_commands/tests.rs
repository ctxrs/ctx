use std::cell::Cell;

use super::*;

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

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
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
    let canonical = root.path().join("work.sqlite");
    fs::write(&graph, b"encrypted graph").unwrap();
    fs::write(&canonical, b"canonical history").unwrap();
    (root, helper, graph, canonical)
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
}

#[test]
fn ordinary_uninstall_preserves_local_pro_data_and_history() {
    let (root, helper, graph, canonical) = fixture();
    run_uninstall(root.path(), None, UninstallDataDisposition::Keep, true).unwrap();
    assert!(!helper.exists());
    assert_eq!(fs::read(graph).unwrap(), b"encrypted graph");
    assert_eq!(fs::read(canonical).unwrap(), b"canonical history");
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
    let (root, helper, graph, canonical) = fixture();
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
    assert!(canonical.exists());
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
        fs::write(empty.path().join("work.sqlite"), b"canonical history").unwrap();
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
        assert_eq!(
            fs::read(empty.path().join("work.sqlite")).unwrap(),
            b"canonical history"
        );
    }
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
    let (root, helper, graph, canonical) = fixture();
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
    assert!(canonical.exists());
}

fn pro_status(access_state: &str) -> ProStatus {
    let locked = access_state == "locked";
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
        ) -> Result<ProSetupPlan> {
            bail!("unused")
        }

        fn manage(&mut self, _data_root: &Path) -> Result<ProManagePlan> {
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
    run_manage_with_opener(
        root.path(),
        &mut ManageService,
        false,
        true,
        &mut telemetry,
        &opener,
    )
    .unwrap();
    assert_eq!(calls.get(), 0);
}

#[test]
fn ordinary_uninstall_then_delete_and_repeated_delete_are_idempotent() {
    let (root, helper, graph, canonical) = fixture();
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
    assert!(canonical.exists());

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
    assert!(canonical.exists());
}

#[test]
fn tty_uninstall_prompt_is_exact_and_defaults_to_delete() {
    let mut input = std::io::Cursor::new(b"\n".to_vec());
    let mut output = Vec::new();
    assert_eq!(
        prompt_uninstall_data_disposition(&mut input, &mut output).unwrap(),
        UninstallDataDisposition::Delete
    );
    assert_eq!(
        String::from_utf8(output).unwrap(),
        format!("{UNINSTALL_DATA_PROMPT} ")
    );
}

#[test]
fn tty_uninstall_prompt_can_preserve_data_and_reprompts_invalid_input() {
    let mut input = std::io::Cursor::new(b"maybe\nn\n".to_vec());
    let mut output = Vec::new();
    assert_eq!(
        prompt_uninstall_data_disposition(&mut input, &mut output).unwrap(),
        UninstallDataDisposition::Keep
    );
    assert_eq!(
        String::from_utf8(output).unwrap(),
        format!("{UNINSTALL_DATA_PROMPT} Please answer y or n.\n{UNINSTALL_DATA_PROMPT} ")
    );
}
