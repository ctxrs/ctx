use super::client::{
    source_refresh_request_is_unknown, validate_source_refresh_status_response_authority,
    wait_for_published_generation,
};
use super::*;

use std::sync::Arc;

use ctx_history_index::WriterOptions;
use ctx_semantic_model::SharedSemanticRuntime;

use crate::{
    paths_status::{
        daemon_source_backed_refresh_job_path, read_daemon_job_status, write_daemon_job_status,
    },
    query_service::{
        ctx_authenticated_request_handler,
        start_daemon_source_refresh_service_with_request_timeout, DaemonQueryService,
    },
};

fn load_explicit_source_catalog_authority(
    data_root: &Path,
) -> Result<ctx_history_refresh::ExplicitSourceCatalogAuthority> {
    let path = data_root
        .parent()
        .context("test data root has no parent")?
        .join("explicit-source.jsonl");
    std::fs::write(
        &path,
        "{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v2\"}\n",
    )?;
    let source = ctx_history_refresh::explicit_source_for_path(data_root, &path, None, true)?;
    Ok(ctx_history_refresh::upsert_explicit_source(data_root, &source)?.authority)
}

#[cfg(any(unix, windows))]
fn start_source_refresh_service_for_test(
    data_root: &Path,
) -> (DaemonQueryService, Arc<CoreRefreshEngine>) {
    let source_refresh = Arc::new(CoreRefreshEngine::new());
    let wakeup = Arc::new(crate::daemon_wakeup::DaemonWakeup::default());
    let handler = ctx_authenticated_request_handler(
        data_root,
        SharedSemanticRuntime::default(),
        Arc::clone(&source_refresh),
        wakeup,
        &crate::test_support::CONFIG,
    );
    let service = start_daemon_source_refresh_service_with_request_timeout(
        data_root,
        handler,
        StdDuration::from_millis(100),
    )
    .unwrap();
    (service, source_refresh)
}

#[test]
fn every_normal_status_state_requires_exact_response_authority() {
    for state in ["queued", "running", "failed", "published"] {
        let response = compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": "expected-request",
            "request_state": state,
        }));
        validate_source_refresh_status_response_authority(&response, "expected-request").unwrap();

        for (field, value) in [
            ("schema_version", json!(2)),
            ("owner", json!("different-owner")),
            ("request_id", json!("different-request")),
        ] {
            let mut mismatched = response.clone();
            mismatched[field] = value;
            assert!(validate_source_refresh_status_response_authority(
                &mismatched,
                "expected-request"
            )
            .is_err());
        }
    }
}

#[test]
fn typed_unknown_response_requires_exact_request_identity_not_error_text() {
    let unknown = compact_json(json!({
        "ok": false,
        "schema_version": 1,
        "owner": "daemon",
        "request_id": "lost-request",
        "request_state": SOURCE_REFRESH_UNKNOWN_REQUEST_STATE,
        "error_code": SOURCE_REFRESH_UNKNOWN_REQUEST_ERROR_CODE,
        "reason": "request_not_retained_after_restart",
        "retryable": false,
        "error": "arbitrary localized detail",
    }));
    assert!(source_refresh_request_is_unknown(&unknown, "lost-request").unwrap());
    assert!(source_refresh_request_is_unknown(&unknown, "different-request").is_err());
}

#[test]
fn pre_overlay_periodic_job_does_not_block_restart_on_legacy_catalog_commitment() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
    let mut legacy_authority = authority.to_json();
    legacy_authority.as_object_mut().unwrap().remove("entries");
    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(&data_root),
        &json!({
            "schema_version": 1,
            "owner": "daemon",
            "request_id": "legacy-periodic",
            "request_state": "running",
            "operation": "refresh",
            "previous_generation": null,
            "published_generation": null,
            "refresh_scope": {"kind": "all"},
            "requested_explicit_source_catalog": legacy_authority,
            "daemon_mode": "full",
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
        }),
    )
    .unwrap();

    let coordinator = CoreRefreshEngine::new();
    assert!(coordinator
        .recover_interrupted_publication(&data_root)
        .unwrap());
    let recovered = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("recovery job");
    assert_eq!(recovered["request_state"], "admission_pending");
    assert!(recovered.get("requested_explicit_source_catalog").is_none());
}

#[test]
fn legacy_publication_without_source_refresh_metadata_does_not_block_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();

    // A generation published by a pre-control-plane binary carries no
    // source-refresh publication metadata.
    let writer = ctx_history_index::GenerationWriter::open(
        source_backed_index_root(&data_root),
        WriterOptions::default(),
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let commit = writer.commit(|_| true).unwrap();

    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(&data_root),
        &json!({
            "schema_version": 1,
            "owner": "daemon",
            "request_id": "legacy-published",
            "request_state": "published",
            "operation": "refresh",
            "previous_generation": null,
            "published_generation": commit.generation_id,
            "refresh_scope": {"kind": "all"},
            "requested_explicit_source_catalog": authority.to_json(),
            "daemon_mode": "full",
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
        }),
    )
    .unwrap();

    let coordinator = CoreRefreshEngine::new();
    assert!(!coordinator
        .recover_interrupted_publication(&data_root)
        .unwrap());
}

#[test]
fn legacy_terminal_publication_recovers_successor_without_pointer_change() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
    let writer = ctx_history_index::GenerationWriter::open(
        source_backed_index_root(&data_root),
        WriterOptions::default(),
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let commit = writer.commit(|_| true).unwrap();
    let successor = CoreRefreshEngine::new().enqueue(Some(commit.generation_id.clone()));
    let successor_id = successor["request_id"].as_str().unwrap().to_owned();

    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(&data_root),
        &json!({
            "schema_version": 1,
            "owner": "daemon",
            "request_id": "legacy-published-with-successor",
            "request_state": "published",
            "operation": "refresh",
            "previous_generation": commit.generation_id.clone(),
            "published_generation": commit.generation_id,
            "refresh_scope": {"kind": "all"},
            "requested_explicit_source_catalog": authority.to_json(),
            "daemon_mode": "full",
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
            "queued_successors": [successor],
        }),
    )
    .unwrap();

    let coordinator = CoreRefreshEngine::new();
    assert!(coordinator
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        coordinator.status(&successor_id).unwrap()["request_state"],
        "admission_pending"
    );
    assert!(coordinator.has_pending_request());
}

#[test]
fn metadata_free_publication_requeues_a_running_refresh_exhaustively() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
    let writer = ctx_history_index::GenerationWriter::open(
        source_backed_index_root(&data_root),
        WriterOptions::default(),
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let commit = writer.commit(|_| true).unwrap();

    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(&data_root),
        &json!({
            "schema_version": 1,
            "owner": "daemon",
            "request_id": "still-running",
            "request_state": "running",
            "operation": "refresh",
            "previous_generation": null,
            "published_generation": commit.generation_id,
            "refresh_scope": {"kind": "all"},
            "requested_explicit_source_catalog": authority.to_json(),
            "daemon_mode": "full",
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
        }),
    )
    .unwrap();

    let coordinator = CoreRefreshEngine::new();
    assert!(coordinator
        .recover_interrupted_publication(&data_root)
        .unwrap());
    let recovered = coordinator.status("still-running").unwrap();
    assert_eq!(recovered["request_state"], "admission_pending");
    assert_eq!(recovered["reconciliation_demand"], "exhaustive");
}

#[test]
fn metadata_free_publication_requires_the_exact_legacy_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
    let writer = ctx_history_index::GenerationWriter::open(
        source_backed_index_root(&data_root),
        WriterOptions::default(),
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let commit = writer.commit(|_| true).unwrap();

    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(&data_root),
        &json!({
            "schema_version": 1,
            "owner": "daemon",
            "request_id": "mismatched-published",
            "request_state": "published",
            "operation": "refresh",
            "previous_generation": commit.generation_id,
            "published_generation": "different-generation",
            "refresh_scope": {"kind": "all"},
            "requested_explicit_source_catalog": authority.to_json(),
            "daemon_mode": "full",
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
        }),
    )
    .unwrap();

    let error = CoreRefreshEngine::new()
        .recover_interrupted_publication(&data_root)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("legacy Core refresh job names a different published generation"));
}

#[cfg(any(unix, windows))]
#[test]
fn old_wait_request_keeps_exact_identity_across_restart_and_returns_exact_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();

    let prior_process = CoreRefreshEngine::new();
    let old = prior_process
        .handle_ipc_request(
            &data_root,
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "explicit_source_catalog": authority.to_json(),
            }),
        )
        .unwrap()
        .expect("prior-process request");
    let old_request_id = old["request_id"].as_str().unwrap().to_owned();
    let old_requested_at_ms = old["requested_at_ms"].as_i64().unwrap();
    drop(prior_process);

    let (service, source_refresh) = start_source_refresh_service_for_test(&data_root);
    assert!(source_refresh
        .recover_interrupted_publication(&data_root)
        .unwrap());
    let recovered = source_refresh
        .status(&old_request_id)
        .expect("acknowledged request survives restart");
    assert_eq!(recovered["request_id"], old_request_id);
    assert_eq!(recovered["request_state"], "admission_pending");
    assert_eq!(recovered["requested_at_ms"], old_requested_at_ms);
    assert_eq!(recovered["coalesced_requests"], 0);
    assert_eq!(recovered["operation"], old["operation"]);
    assert_eq!(recovered["refresh_scope"], old["refresh_scope"]);
    assert_eq!(recovered["daemon_mode"], old["daemon_mode"]);
    assert_eq!(recovered["trigger"], old["trigger"]);
    assert_eq!(recovered["trigger_provenance"], old["trigger_provenance"]);
    assert_eq!(
        recovered["requested_explicit_source_catalog"],
        old["requested_explicit_source_catalog"]
    );
    let active_request_id = old_request_id.clone();

    assert!(source_refresh
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    let admitted = source_refresh
        .status(&active_request_id)
        .expect("re-admitted exact request");
    let admitted_routes = admitted["refresh_scope"]["routes"]
        .as_array()
        .expect("exact admitted routes")
        .iter()
        .map(|route| {
            ctx_history_index::SourceRouteIdentity::from_sha256(
                route.as_str().expect("exact admitted route").to_owned(),
            )
            .unwrap()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(admitted_routes.len(), 1);
    assert_eq!(
        source_refresh.request_catalog_authority_for_test(&active_request_id),
        Some(authority.clone())
    );

    let run = source_refresh
        .run_next(&data_root)
        .expect("restarted terminal refresh");
    assert!(!run.failed, "{:#}", run.job);
    let expected_generation = run.job["published_generation"]
        .as_str()
        .expect("terminal published generation")
        .to_owned();

    let observation = wait_for_published_generation(
        &data_root,
        old_request_id,
        SourceBackedRefreshMode::Wait,
        ctx_history_refresh::RefreshOperation::Import,
        Some(&authority),
        false,
    )
    .unwrap();

    assert_eq!(
        observation.request_id.as_deref(),
        Some(active_request_id.as_str())
    );
    assert_eq!(observation.pin.generation_id(), expected_generation);
    assert_eq!(
        observation
            .receipt
            .as_ref()
            .map(|receipt| receipt.published_generation.as_str()),
        Some(expected_generation.as_str())
    );
    assert_eq!(
        observation
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.published_explicit_source_catalog.as_ref()),
        Some(&authority)
    );
    drop(service);
}
