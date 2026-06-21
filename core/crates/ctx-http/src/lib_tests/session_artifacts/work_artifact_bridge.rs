use super::*;
use chrono::Utc;
use ctx_core::ids::{ArtifactId, WorkEvidenceId, WorkRecordId, WorkRecordLinkId};
use ctx_core::models::{
    RecordFidelity, RecordSource, RecordTrust, WorkEvidence, WorkEvidenceFreshness,
    WorkEvidenceKind, WorkEvidenceStatus, WorkLifecycle, WorkLinkRole, WorkLinkTargetKind,
    WorkRecord, WorkRecordLink, WorkSummaryFreshness, WorkTrustVerdict,
    WORK_OBSERVABILITY_SCHEMA_VERSION,
};

async fn seed_work_with_artifact_link(
    fixture: &SessionArtifactFixture,
    artifact_id: ArtifactId,
) -> WorkRecordId {
    let work_id = WorkRecordId::from_id("wrk_artifact_bridge");
    let now = Utc::now();
    let store = fixture
        .daemon()
        .store_for_workspace(fixture.session.workspace_id)
        .await
        .expect("workspace store");
    store
        .upsert_work_record(&WorkRecord {
            work_id: work_id.clone(),
            workspace_id: fixture.session.workspace_id,
            title: Some("Artifact bridge".to_string()),
            objective: Some("Expose safe Work artifact metadata".to_string()),
            lifecycle: WorkLifecycle::Active,
            primary_repo_root: None,
            primary_branch: None,
            base_commit: None,
            head_commit: None,
            current_diff_fingerprint: None,
            trust_verdict: WorkTrustVerdict::UntrustedLocalCapture,
            summary_freshness: WorkSummaryFreshness::Missing,
            metadata_json: None,
            created_at: now,
            updated_at: now,
            schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
        })
        .await
        .expect("work record");
    store
        .upsert_work_record_link(&WorkRecordLink {
            link_id: WorkRecordLinkId::from_id("wln_artifact_bridge"),
            work_id: work_id.clone(),
            workspace_id: fixture.session.workspace_id,
            target_kind: WorkLinkTargetKind::Artifact,
            target_id: Some(artifact_id.0.to_string()),
            target_json: Some(json!({
                "name": "artifact.txt",
                "absolute_path": "/home/daddy/private/artifact.txt",
                "token": "sk-test-secret",
            })),
            role: WorkLinkRole::Result,
            source: RecordSource::Session,
            fidelity: RecordFidelity::Declared,
            trust: RecordTrust::Low,
            created_at: now,
            updated_at: now,
            schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
        })
        .await
        .expect("work artifact link");
    work_id
}

async fn seed_command_evidence_with_raw_refs(
    fixture: &SessionArtifactFixture,
    work_id: &WorkRecordId,
    artifact_id: ArtifactId,
) {
    let now = Utc::now();
    let store = fixture
        .daemon()
        .store_for_workspace(fixture.session.workspace_id)
        .await
        .expect("workspace store");
    store
        .upsert_work_evidence(&WorkEvidence {
            evidence_id: WorkEvidenceId::from_id("wev_artifact_bridge_command"),
            work_id: work_id.clone(),
            workspace_id: fixture.session.workspace_id,
            kind: WorkEvidenceKind::Command,
            status: WorkEvidenceStatus::ObservedPass,
            freshness: WorkEvidenceFreshness::Fresh,
            claim: Some("captured deterministic command output".to_string()),
            command: Some("printf artifact-body".to_string()),
            argv: vec!["printf".to_string(), "artifact-body".to_string()],
            cwd: Some("/home/daddy/private/project".to_string()),
            exit_code: Some(0),
            repo_root: Some("/home/daddy/private/project".to_string()),
            head_sha: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            branch: Some("main".to_string()),
            fingerprint: None,
            current_fingerprint: None,
            output_ref: Some(json!({
                "kind": "ctx.work.command_output_preview",
                "share_safe": true,
                "stdout_redacted": "artifact-body",
                "stderr_redacted": "",
                "stdout_size_bytes": 13,
                "stderr_size_bytes": 0,
                "stdout_sha256": "6e2f5d4b02f627995f2816073e59ecebcb388a53d964d7292174ce1261761ee4",
                "stderr_sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "preview_limit_bytes": 4096,
                "truncated": false,
                "absolute_path": "/home/daddy/private/output.log"
            })),
            artifact_ref: Some(json!({
                "artifact_id": artifact_id.0.to_string(),
                "absolute_path": "/home/daddy/private/artifact.txt",
                "download_url": "/tmp/private/artifact.txt",
            })),
            source: RecordSource::Session,
            fidelity: RecordFidelity::Exact,
            trust: RecordTrust::Medium,
            started_at: now,
            finished_at: now,
            created_at: now,
            updated_at: now,
            schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
        })
        .await
        .expect("command evidence");
}

async fn seed_unlinked_work(fixture: &SessionArtifactFixture) -> WorkRecordId {
    let work_id = WorkRecordId::from_id("wrk_without_artifact");
    let now = Utc::now();
    let store = fixture
        .daemon()
        .store_for_workspace(fixture.session.workspace_id)
        .await
        .expect("workspace store");
    store
        .upsert_work_record(&WorkRecord {
            work_id: work_id.clone(),
            workspace_id: fixture.session.workspace_id,
            title: Some("Other Work".to_string()),
            objective: None,
            lifecycle: WorkLifecycle::Active,
            primary_repo_root: None,
            primary_branch: None,
            base_commit: None,
            head_commit: None,
            current_diff_fingerprint: None,
            trust_verdict: WorkTrustVerdict::UntrustedLocalCapture,
            summary_freshness: WorkSummaryFreshness::Missing,
            metadata_json: None,
            created_at: now,
            updated_at: now,
            schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
        })
        .await
        .expect("unlinked work record");
    work_id
}

async fn seed_session_artifact(
    fixture: &SessionArtifactFixture,
    name: &str,
    mime_type: &str,
    body: &[u8],
) -> (ArtifactId, std::path::PathBuf) {
    let worktree_root = fixture
        .daemon()
        .session_worktree_root_path_for_test(&fixture.session)
        .await
        .expect("worktree root");
    let artifact_path = worktree_root.join(name);
    std::fs::write(&artifact_path, body).expect("write artifact");

    let res = post_session_artifacts(
        &fixture.app,
        fixture.session.id,
        json!([{
            "absolute_file_path": artifact_path.to_string_lossy(),
            "name": name,
            "mime_type": mime_type
        }]),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let artifacts: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let raw = artifacts[0]["id"].as_str().expect("artifact id");
    let artifact_id = ArtifactId(uuid::Uuid::parse_str(raw).expect("artifact uuid"));
    (artifact_id, artifact_path)
}

async fn get_work_artifact(
    fixture: &SessionArtifactFixture,
    work_id: &WorkRecordId,
    artifact_id: ArtifactId,
) -> axum::response::Response {
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/work/{}/artifacts/{}",
            fixture.session.workspace_id.0, work_id.0, artifact_id.0
        ))
        .body(Body::empty())
        .unwrap();
    fixture.app.clone().oneshot(req).await.unwrap()
}

async fn get_work_inspector(
    fixture: &SessionArtifactFixture,
    work_id: &WorkRecordId,
) -> serde_json::Value {
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/work/{}/inspector",
            fixture.session.workspace_id.0, work_id.0
        ))
        .body(Body::empty())
        .unwrap();
    let res = fixture.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn work_inspector_exposes_typed_safe_artifact_metadata_and_urls() {
    let fixture = build_session_artifact_fixture().await;
    let (artifact_id, _artifact_path) =
        seed_session_artifact(&fixture, "artifact.txt", "text/plain", b"artifact-body\n").await;
    let work_id = seed_work_with_artifact_link(&fixture, artifact_id).await;
    seed_command_evidence_with_raw_refs(&fixture, &work_id, artifact_id).await;

    let inspector = get_work_inspector(&fixture, &work_id).await;
    let artifact = &inspector["artifacts"][0];
    assert_eq!(artifact["artifact_id"], artifact_id.0.to_string());
    assert_eq!(artifact["display_name"], "artifact.txt");
    assert_eq!(artifact["mime_type"], "text/plain");
    assert_eq!(artifact["bytes"], 14);
    assert_eq!(artifact["missing"], false);
    assert_eq!(artifact["render_kind"], "text");
    assert_eq!(
        artifact["download_url"],
        format!(
            "/api/workspaces/{}/work/{}/artifacts/{}",
            fixture.session.workspace_id.0, work_id.0, artifact_id.0
        )
    );
    let serialized = serde_json::to_string(&inspector).unwrap();
    assert!(!serialized.contains("/home/daddy/private"));
    assert!(!serialized.contains("sk-test-secret"));
    assert!(!serialized.contains("absolute_path"));
    assert!(!serialized.contains("artifact_ref"));
    assert!(!serialized.contains("output_ref"));
    assert_eq!(
        inspector["commands"][0]["stdout_sha256"],
        "6e2f5d4b02f627995f2816073e59ecebcb388a53d964d7292174ce1261761ee4"
    );
    assert_eq!(inspector["commands"][0]["stdout_size_bytes"], 13);
    assert!(inspector["evidence"][0].get("artifact_ref").is_none());
    assert!(inspector["evidence"][0].get("output_ref").is_none());
}

#[tokio::test]
async fn work_artifact_route_serves_only_artifacts_linked_to_that_work() {
    let fixture = build_session_artifact_fixture().await;
    let (artifact_id, _artifact_path) =
        seed_session_artifact(&fixture, "artifact.txt", "text/plain", b"artifact-body\n").await;
    let linked_work_id = seed_work_with_artifact_link(&fixture, artifact_id).await;
    let unlinked_work_id = seed_unlinked_work(&fixture).await;

    let res = get_work_artifact(&fixture, &linked_work_id, artifact_id).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/plain")
    );
    assert_eq!(
        res.headers()
            .get("x-content-type-options")
            .and_then(|value| value.to_str().ok()),
        Some("nosniff")
    );
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), b"artifact-body\n");

    let res = get_work_artifact(&fixture, &unlinked_work_id, artifact_id).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn work_inspector_refreshes_session_projection_for_late_artifacts() {
    let fixture = build_session_artifact_fixture().await;
    let (artifact_id, _artifact_path) =
        seed_session_artifact(&fixture, "late-artifact.txt", "text/plain", b"late\n").await;
    let store = fixture
        .daemon()
        .store_for_workspace(fixture.session.workspace_id)
        .await
        .expect("workspace store");
    let work = store
        .find_work_record_by_link(
            fixture.session.workspace_id,
            WorkLinkTargetKind::Session,
            &fixture.session.id.0.to_string(),
        )
        .await
        .expect("find session-linked work")
        .expect("session projection should create Work record");

    let inspector = get_work_inspector(&fixture, &work.work_id).await;
    assert!(
        inspector["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["artifact_id"] == artifact_id.0.to_string()),
        "Inspector should refresh session projection and include late artifacts"
    );
}

#[tokio::test]
async fn work_artifact_route_forces_svg_and_html_to_non_executable_downloads() {
    let fixture = build_session_artifact_fixture().await;
    let (artifact_id, _artifact_path) =
        seed_session_artifact(&fixture, "artifact.svg", "image/svg+xml", br#"<svg></svg>"#).await;
    let work_id = seed_work_with_artifact_link(&fixture, artifact_id).await;

    let inspector = get_work_inspector(&fixture, &work_id).await;
    assert_eq!(inspector["artifacts"][0]["render_kind"], "download_only");
    assert!(inspector["artifacts"][0]["thumbnail_url"].is_null());

    let res = get_work_artifact(&fixture, &work_id, artifact_id).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/octet-stream")
    );
    assert!(
        res.headers()
            .get(header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("attachment")),
        "svg work artifact must be served as an attachment"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn work_artifact_route_rejects_symlink_swap_after_work_authorization() {
    let fixture = build_session_artifact_fixture().await;
    let (artifact_id, artifact_path) =
        seed_session_artifact(&fixture, "artifact.txt", "text/plain", b"artifact-body\n").await;
    let work_id = seed_work_with_artifact_link(&fixture, artifact_id).await;

    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().join("outside.txt");
    std::fs::write(&outside_path, b"outside\n").unwrap();
    std::fs::remove_file(&artifact_path).unwrap();
    std::os::unix::fs::symlink(&outside_path, &artifact_path).unwrap();

    let res = get_work_artifact(&fixture, &work_id, artifact_id).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
