use crate::test_support::TestDaemon;
use ctx_core::ids::{RunId, WorkspaceId};
use ctx_core::models::VcsKind;
use ctx_route_contracts::run_archive::{
    requested_batch_item_limit, AcknowledgeRunArchiveIngestBatchRouteBody,
    AcknowledgeRunArchiveIngestBatchRouteRequest, BuildRunArchiveIngestBatchRouteRequest,
    RunArchiveBatchRouteQuery, RunArchiveRouteErrorKind, RunArchiveRouteParams,
};

#[test]
fn requested_batch_item_limit_defaults_and_accepts_boundaries() {
    assert_eq!(requested_batch_item_limit(None).unwrap(), 250);
    assert_eq!(requested_batch_item_limit(Some(1)).unwrap(), 1);
    assert_eq!(requested_batch_item_limit(Some(1_000)).unwrap(), 1_000);
}

#[test]
fn requested_batch_item_limit_rejects_out_of_range_values() {
    for max_items in [0, 1_001] {
        let error = requested_batch_item_limit(Some(max_items)).unwrap_err();
        assert_eq!(error.kind(), RunArchiveRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "max_items must be between 1 and 1000");
    }
}

#[test]
fn route_params_reject_invalid_workspace_id() {
    let params = RunArchiveRouteParams::new("not-a-uuid", RunId::new().0.to_string());

    let error = params.parse().unwrap_err();
    assert_eq!(error.kind(), RunArchiveRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid workspace id");
}

#[test]
fn route_params_reject_invalid_run_id() {
    let params = RunArchiveRouteParams::new(WorkspaceId::new().0.to_string(), "not-a-uuid");

    let error = params.parse().unwrap_err();
    assert_eq!(error.kind(), RunArchiveRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid run id");
}

#[tokio::test]
async fn run_archive_route_maps_missing_workspace_to_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");

    let error = match daemon
        .run_archive_handle_for_test()
        .build_run_archive_ingest_batch_for_route(BuildRunArchiveIngestBatchRouteRequest::new(
            RunArchiveRouteParams::new(
                WorkspaceId::new().0.to_string(),
                RunId::new().0.to_string(),
            ),
            RunArchiveBatchRouteQuery::default(),
        ))
        .await
    {
        Ok(_) => panic!("missing workspace should map to route error"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), RunArchiveRouteErrorKind::NotFound);
    assert_eq!(
        error.message(),
        "workspace not found for run archive ingest"
    );
}

#[tokio::test]
async fn run_archive_route_treats_deleting_workspace_as_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = daemon
        .global_store()
        .create_workspace(
            "deleting-archive".to_string(),
            temp.path().join("workspace").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    daemon.stores().begin_workspace_delete(workspace.id).await;

    let error = match daemon
        .run_archive_handle_for_test()
        .build_run_archive_ingest_batch_for_route(BuildRunArchiveIngestBatchRouteRequest::new(
            RunArchiveRouteParams::new(workspace.id.0.to_string(), RunId::new().0.to_string()),
            RunArchiveBatchRouteQuery::default(),
        ))
        .await
    {
        Ok(_) => panic!("deleting workspace should map to route not found"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), RunArchiveRouteErrorKind::NotFound);
    assert_eq!(
        error.message(),
        "workspace not found for run archive ingest"
    );
    daemon.stores().finish_workspace_delete(workspace.id).await;
}

#[tokio::test]
async fn run_archive_route_maps_unavailable_workspace_store_to_internal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = daemon
        .global_store()
        .create_workspace(
            "unavailable-archive".to_string(),
            temp.path().join("workspace").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    daemon
        .cache_rehydration_make_workspace_store_unopenable_for_test(workspace.id)
        .await
        .expect("block workspace store");

    let error = match daemon
        .run_archive_handle_for_test()
        .build_run_archive_ingest_batch_for_route(BuildRunArchiveIngestBatchRouteRequest::new(
            RunArchiveRouteParams::new(workspace.id.0.to_string(), RunId::new().0.to_string()),
            RunArchiveBatchRouteQuery::default(),
        ))
        .await
    {
        Ok(_) => panic!("unavailable workspace store should map to internal route error"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), RunArchiveRouteErrorKind::Internal);
    assert!(error
        .message()
        .contains("failed to build run archive ingest batch"));
}

#[tokio::test]
async fn run_archive_route_maps_ack_conflict_to_conflict() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = daemon
        .global_store()
        .create_workspace(
            "archive".to_string(),
            temp.path().join("workspace").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    let fixture = daemon
        .seed_org_visible_run_archive_fixture_for_test(workspace.id, temp.path())
        .await
        .expect("seed run archive fixture");
    let handle = daemon.run_archive_handle_for_test();
    let response = handle
        .build_run_archive_ingest_batch_for_route(BuildRunArchiveIngestBatchRouteRequest::new(
            RunArchiveRouteParams::new(workspace.id.0.to_string(), fixture.run_id.0.to_string()),
            RunArchiveBatchRouteQuery::default(),
        ))
        .await
        .expect("build batch");
    let mut batch = response.0.expect("seeded run should produce batch");
    batch.to.session_event_seq += 100;
    batch.to.audit_event_seq += 100;

    let body: AcknowledgeRunArchiveIngestBatchRouteBody =
        serde_json::from_value(serde_json::to_value(&batch).expect("serialize batch"))
            .expect("deserialize route body");
    let error = match handle
        .acknowledge_run_archive_ingest_batch_for_route(
            AcknowledgeRunArchiveIngestBatchRouteRequest::new(
                RunArchiveRouteParams::new(
                    workspace.id.0.to_string(),
                    fixture.run_id.0.to_string(),
                ),
                RunArchiveBatchRouteQuery::default(),
                body,
            ),
        )
        .await
    {
        Ok(_) => panic!("tampered cursor should conflict"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), RunArchiveRouteErrorKind::Conflict);
}
