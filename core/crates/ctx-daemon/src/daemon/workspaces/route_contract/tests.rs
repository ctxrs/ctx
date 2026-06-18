use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use tokio::sync::Mutex;

use super::common::{
    file_completions_route_error, route_file_download_error, workspace_delete_route_error,
    workspace_harness_container_ensure_error, workspace_harness_container_status_error,
    workspace_hydration_route_error,
};
use crate::daemon::workspaces::{WorkspaceHarnessContainerError, WorkspaceHydrationError};
use crate::test_support::TestDaemon;
use ctx_core::ids::{ChangeSetId, ContributionId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    AttachmentMode, AttachmentUpdatePolicy, ChangeSet, Contribution, ContributionEndpoint,
    ContributionRole, MergeQueueEntryStatus, RecordFidelity, RecordOrigin, RecordSource,
    RecordTrust, VcsKind, Workspace, WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot,
    WorkspaceAttachment, WorkspaceAttachmentKind, WorkspaceAttachmentStatus, Worktree,
    WorktreeBootstrapStatus,
};
use ctx_route_contracts::workspaces::{
    SyncWorkspaceAttachmentsRouteRequest, UpdateAgentSystemPromptConfigRouteRequest,
    UpdateWorkspaceMergeQueueConfigRequest, UpdateWorkspacePrimaryBranchRequest,
    UpdateWorktreeBootstrapConfigRequest, WorkspaceActiveHeadBatchRouteResponse,
    WorkspaceActiveSnapshotRouteResponse, WorkspaceAgentWorkRouteQuery,
    WorkspaceAttachmentRouteResponse, WorkspaceFileCompletionsRouteQuery,
    WorkspacePromptConfigRouteParams, WorkspaceRouteErrorKind, WorkspaceRouteParams,
    WorkspaceRouteResponse, WorktreeRouteParams, WorktreeRouteResponse,
};
use ctx_store::WorktreeBootstrapResultUpdate;

fn assert_same_json<T, U>(left: T, right: U)
where
    T: Serialize,
    U: Serialize,
{
    assert_eq!(
        serde_json::to_value(left).unwrap(),
        serde_json::to_value(right).unwrap()
    );
}

async fn create_route_contract_workspace(daemon: &TestDaemon, name: &str) -> Workspace {
    daemon
        .global_store()
        .create_workspace(
            name.to_string(),
            daemon.data_root().join(name).to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace")
}

async fn create_route_contract_workspace_with_store(daemon: &TestDaemon, name: &str) -> Workspace {
    let root = daemon.data_root().join(name);
    std::fs::create_dir_all(&root).expect("create workspace root");
    daemon
        .seed_workspace_for_test(name, &root, VcsKind::Git)
        .await
        .expect("seed workspace")
}

fn run_git_for_primary_branch_route_test(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_git_repo_for_primary_branch_route_test(root: &Path) {
    std::fs::create_dir_all(root).expect("create repo root");
    run_git_for_primary_branch_route_test(root, &["init"]);
    run_git_for_primary_branch_route_test(root, &["checkout", "-b", "main"]);
    run_git_for_primary_branch_route_test(root, &["config", "user.email", "test@example.com"]);
    run_git_for_primary_branch_route_test(root, &["config", "user.name", "Test"]);
    std::fs::write(root.join("file.txt"), "hello\n").expect("write fixture file");
    run_git_for_primary_branch_route_test(root, &["add", "."]);
    run_git_for_primary_branch_route_test(root, &["commit", "-m", "init"]);
}

async fn create_route_contract_worktree(
    daemon: &TestDaemon,
    workspace: &Workspace,
    name: &str,
    bootstrap_log_path: Option<String>,
) -> Worktree {
    let root = daemon.data_root().join(name);
    std::fs::create_dir_all(&root).expect("create worktree root");
    let store = daemon
        .store_for_workspace(workspace.id)
        .await
        .expect("workspace store");
    let worktree = store
        .create_worktree(
            workspace.id,
            root.to_string_lossy().to_string(),
            "base-sha".to_string(),
            Some("main".to_string()),
        )
        .await
        .expect("create worktree");
    daemon
        .global_store()
        .upsert_workspace_worktree_index(worktree.id, workspace.id)
        .await
        .expect("index worktree");
    if bootstrap_log_path.is_some() {
        let now = Utc::now();
        store
            .update_worktree_bootstrap_result(WorktreeBootstrapResultUpdate {
                worktree_id: worktree.id,
                status: WorktreeBootstrapStatus::Success,
                started_at: now,
                finished_at: now,
                exit_code: Some(0),
                timeout_sec: Some(30),
                error: None,
                log_path: bootstrap_log_path,
                log_truncated: Some(false),
                command: Some("true".to_string()),
                script_path: None,
            })
            .await
            .expect("update bootstrap result");
    }
    store
        .get_worktree(worktree.id)
        .await
        .expect("load worktree")
        .expect("worktree exists")
}

#[tokio::test]
async fn agent_work_route_lists_workspace_graph_records() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = create_route_contract_workspace_with_store(&daemon, "agent-work").await;
    let store = daemon
        .store_for_workspace(workspace.id)
        .await
        .expect("workspace store");
    let change_set_id = ChangeSetId::new();
    let contribution_id = ContributionId::new();
    store
        .upsert_change_set(&ChangeSet {
            id: change_set_id.clone(),
            workspace_id: workspace.id,
            source_worktree_id: None,
            source: RecordSource::Worktree,
            origin: RecordOrigin::Agent,
            fidelity: RecordFidelity::Diff,
            trust: RecordTrust::High,
            title: Some("Expose agent work graph".to_string()),
            summary: None,
            description: None,
            fingerprint: None,
            base_revision: Some("base".to_string()),
            head_revision: Some("head".to_string()),
            target_branch: Some("main".to_string()),
            pull_requests: Vec::new(),
            source_records: Vec::new(),
            issuer: None,
            created_at: None,
            updated_at: None,
            schema_version: 1,
        })
        .await
        .expect("upsert change set");
    store
        .upsert_contribution(&Contribution {
            id: contribution_id.clone(),
            workspace_id: workspace.id,
            change_set_id: Some(change_set_id.clone()),
            subject: ContributionEndpoint::System {
                label: Some("route-test".to_string()),
            },
            target: ContributionEndpoint::ChangeSet {
                change_set_id: change_set_id.clone(),
            },
            role: ContributionRole::Related,
            source: RecordSource::Manual,
            origin: RecordOrigin::User,
            fidelity: RecordFidelity::Declared,
            trust: RecordTrust::Medium,
            summary: Some("Route contract exposes graph records".to_string()),
            fingerprint: None,
            issuer: None,
            metadata_json: None,
            source_records: Vec::new(),
            created_at: None,
            updated_at: None,
            schema_version: 1,
        })
        .await
        .expect("upsert contribution");

    let response = daemon
        .route_handles()
        .workspace_agent_work
        .list_workspace_agent_work_for_route(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            WorkspaceAgentWorkRouteQuery::default(),
        )
        .await
        .expect("list agent work");

    let value = serde_json::to_value(response).expect("serialize response");
    let change_set_id = change_set_id.0.to_string();
    let contribution_id = contribution_id.0.to_string();
    assert_eq!(
        value
            .pointer("/change_sets/0/id")
            .and_then(serde_json::Value::as_str),
        Some(change_set_id.as_str())
    );
    assert_eq!(
        value
            .pointer("/contributions/0/id")
            .and_then(serde_json::Value::as_str),
        Some(contribution_id.as_str())
    );

    let endpoint_json = serde_json::to_string(&ContributionEndpoint::System {
        label: Some("route-test".to_string()),
    })
    .expect("serialize endpoint");
    let endpoint_filtered = daemon
        .route_handles()
        .workspace_agent_work
        .list_workspace_agent_work_for_route(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            WorkspaceAgentWorkRouteQuery {
                endpoint_json: Some(endpoint_json),
                ..WorkspaceAgentWorkRouteQuery::default()
            },
        )
        .await
        .expect("list endpoint-filtered agent work");
    assert_eq!(endpoint_filtered.contributions.len(), 1);
    assert_eq!(endpoint_filtered.change_sets.len(), 1);
    assert_eq!(
        endpoint_filtered.contributions[0].id.0.as_str(),
        contribution_id.as_str()
    );

    let declared_only_contribution_id = ContributionId::new();
    store
        .upsert_contribution(&Contribution {
            id: declared_only_contribution_id.clone(),
            workspace_id: workspace.id,
            change_set_id: Some(ChangeSetId(change_set_id.clone())),
            subject: ContributionEndpoint::System {
                label: Some("declared-only".to_string()),
            },
            target: ContributionEndpoint::System {
                label: Some("qa".to_string()),
            },
            role: ContributionRole::Context,
            source: RecordSource::Manual,
            origin: RecordOrigin::User,
            fidelity: RecordFidelity::Declared,
            trust: RecordTrust::Medium,
            summary: Some("Linked by change_set_id only".to_string()),
            fingerprint: None,
            issuer: None,
            metadata_json: None,
            source_records: Vec::new(),
            created_at: None,
            updated_at: None,
            schema_version: 1,
        })
        .await
        .expect("upsert declared-only contribution");
    let endpoint_only_contribution_id = ContributionId::new();
    store
        .upsert_contribution(&Contribution {
            id: endpoint_only_contribution_id.clone(),
            workspace_id: workspace.id,
            change_set_id: None,
            subject: ContributionEndpoint::System {
                label: Some("endpoint-only".to_string()),
            },
            target: ContributionEndpoint::ChangeSet {
                change_set_id: ChangeSetId(change_set_id.clone()),
            },
            role: ContributionRole::Result,
            source: RecordSource::Manual,
            origin: RecordOrigin::User,
            fidelity: RecordFidelity::Declared,
            trust: RecordTrust::Medium,
            summary: Some("Linked by endpoint only".to_string()),
            fingerprint: None,
            issuer: None,
            metadata_json: None,
            source_records: Vec::new(),
            created_at: None,
            updated_at: None,
            schema_version: 1,
        })
        .await
        .expect("upsert endpoint-only contribution");

    let change_set_endpoint_json = serde_json::to_string(&ContributionEndpoint::ChangeSet {
        change_set_id: ChangeSetId(change_set_id.clone()),
    })
    .expect("serialize change set endpoint");
    let change_set_endpoint_filtered = daemon
        .route_handles()
        .workspace_agent_work
        .list_workspace_agent_work_for_route(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            WorkspaceAgentWorkRouteQuery {
                endpoint_json: Some(change_set_endpoint_json),
                ..WorkspaceAgentWorkRouteQuery::default()
            },
        )
        .await
        .expect("list change-set endpoint-filtered agent work");
    assert_eq!(change_set_endpoint_filtered.change_sets.len(), 1);
    assert_eq!(change_set_endpoint_filtered.contributions.len(), 3);
    assert!(change_set_endpoint_filtered
        .contributions
        .iter()
        .any(|contribution| contribution.id.0.as_str() == contribution_id.as_str()));
    assert!(change_set_endpoint_filtered
        .contributions
        .iter()
        .any(|contribution| contribution.id == declared_only_contribution_id));
    assert!(change_set_endpoint_filtered
        .contributions
        .iter()
        .any(|contribution| contribution.id == endpoint_only_contribution_id));

    let change_set_filtered = daemon
        .route_handles()
        .workspace_agent_work
        .list_workspace_agent_work_for_route(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            WorkspaceAgentWorkRouteQuery {
                change_set_id: Some(change_set_id.clone()),
                ..WorkspaceAgentWorkRouteQuery::default()
            },
        )
        .await
        .expect("list change-set-filtered agent work");
    assert_eq!(change_set_filtered.change_sets.len(), 1);
    assert_eq!(change_set_filtered.contributions.len(), 3);

    let foreign_workspace_id = WorkspaceId::new();
    let foreign_change_set_id = ChangeSetId::new();
    let foreign_contribution_id = ContributionId::new();
    let now = Utc::now();
    let foreign_change_set = ChangeSet {
        id: foreign_change_set_id.clone(),
        workspace_id: foreign_workspace_id,
        source_worktree_id: None,
        source: RecordSource::Manual,
        origin: RecordOrigin::Imported,
        fidelity: RecordFidelity::Declared,
        trust: RecordTrust::Low,
        title: Some("Foreign workspace graph".to_string()),
        summary: None,
        description: None,
        fingerprint: None,
        base_revision: None,
        head_revision: None,
        target_branch: None,
        pull_requests: Vec::new(),
        source_records: Vec::new(),
        issuer: None,
        created_at: Some(now),
        updated_at: Some(now),
        schema_version: 1,
    };
    let foreign_contribution = Contribution {
        id: foreign_contribution_id.clone(),
        workspace_id: foreign_workspace_id,
        change_set_id: Some(foreign_change_set_id.clone()),
        subject: ContributionEndpoint::System {
            label: Some("foreign".to_string()),
        },
        target: ContributionEndpoint::ChangeSet {
            change_set_id: foreign_change_set_id.clone(),
        },
        role: ContributionRole::Related,
        source: RecordSource::Manual,
        origin: RecordOrigin::Imported,
        fidelity: RecordFidelity::Declared,
        trust: RecordTrust::Low,
        summary: Some("Should not be visible through another workspace route".to_string()),
        fingerprint: None,
        issuer: None,
        metadata_json: None,
        source_records: Vec::new(),
        created_at: Some(now),
        updated_at: Some(now),
        schema_version: 1,
    };
    let mut connection = store
        .pool()
        .acquire()
        .await
        .expect("acquire store connection");
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .expect("disable foreign keys for corrupt-row fixture");
    sqlx::query(
        r#"INSERT INTO change_sets (
             id, workspace_id, source_worktree_id, base_revision, head_revision,
             target_branch, record_json, created_at, updated_at
           )
           VALUES (?, ?, NULL, NULL, NULL, NULL, ?, ?, ?)"#,
    )
    .bind(foreign_change_set_id.0.to_string())
    .bind(foreign_workspace_id.0.to_string())
    .bind(serde_json::to_string(&foreign_change_set).expect("serialize foreign change set"))
    .bind(now.to_rfc3339())
    .bind(now.to_rfc3339())
    .execute(&mut *connection)
    .await
    .expect("insert corrupt foreign change set fixture");
    sqlx::query(
        r#"INSERT INTO contributions (
             id, workspace_id, change_set_id, subject_kind, subject_id, target_kind, target_id,
             record_json, created_at, updated_at
           )
           VALUES (?, ?, ?, 'system', 'foreign', 'change_set', ?, ?, ?, ?)"#,
    )
    .bind(foreign_contribution_id.0.to_string())
    .bind(foreign_workspace_id.0.to_string())
    .bind(foreign_change_set_id.0.to_string())
    .bind(foreign_change_set_id.0.to_string())
    .bind(serde_json::to_string(&foreign_contribution).expect("serialize foreign contribution"))
    .bind(now.to_rfc3339())
    .bind(now.to_rfc3339())
    .execute(&mut *connection)
    .await
    .expect("insert corrupt foreign contribution fixture");
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await
        .expect("restore foreign keys after corrupt-row fixture");

    let foreign_change_set_id_string = foreign_change_set_id.0.to_string();
    let foreign_change_set_filtered = daemon
        .route_handles()
        .workspace_agent_work
        .list_workspace_agent_work_for_route(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            WorkspaceAgentWorkRouteQuery {
                change_set_id: Some(foreign_change_set_id_string.clone()),
                ..WorkspaceAgentWorkRouteQuery::default()
            },
        )
        .await
        .expect("list foreign change-set-filtered agent work");
    assert!(foreign_change_set_filtered.change_sets.is_empty());
    assert!(foreign_change_set_filtered.contributions.is_empty());

    let foreign_endpoint_json = serde_json::to_string(&ContributionEndpoint::ChangeSet {
        change_set_id: ChangeSetId(foreign_change_set_id_string),
    })
    .expect("serialize foreign endpoint");
    let foreign_endpoint_filtered = daemon
        .route_handles()
        .workspace_agent_work
        .list_workspace_agent_work_for_route(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            WorkspaceAgentWorkRouteQuery {
                endpoint_json: Some(foreign_endpoint_json),
                ..WorkspaceAgentWorkRouteQuery::default()
            },
        )
        .await
        .expect("list foreign endpoint-filtered agent work");
    assert!(foreign_endpoint_filtered.change_sets.is_empty());
    assert!(foreign_endpoint_filtered.contributions.is_empty());

    let limited = daemon
        .route_handles()
        .workspace_agent_work
        .list_workspace_agent_work_for_route(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            WorkspaceAgentWorkRouteQuery {
                limit: Some(0),
                ..WorkspaceAgentWorkRouteQuery::default()
            },
        )
        .await
        .expect("list limited agent work");
    assert!(limited.change_sets.is_empty());
    assert!(limited.contributions.is_empty());
}

#[tokio::test]
async fn primary_branch_route_params_reject_invalid_workspace_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_primary_branch_handle_for_test();
    let error = handle
        .workspace_primary_branch_for_route_params(WorkspaceRouteParams::new("not-a-workspace"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid workspace id");
}

#[tokio::test]
async fn primary_branch_route_maps_missing_store_to_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace_id = WorkspaceId::new();
    let handle = daemon.workspace_primary_branch_handle_for_test();
    let error = handle
        .workspace_primary_branch_for_route_params(WorkspaceRouteParams::new(
            workspace_id.0.to_string(),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(error.message(), "workspace not found");
}

#[tokio::test]
async fn primary_branch_route_maps_unset_config_to_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = create_route_contract_workspace_with_store(&daemon, "unset-primary").await;
    let handle = daemon.workspace_primary_branch_handle_for_test();
    let error = handle
        .workspace_primary_branch_for_route_params(WorkspaceRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(
        error.message(),
        "workspace primary branch is not configured"
    );
}

#[tokio::test]
async fn primary_branch_update_refreshes_all_worktrees_best_effort() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let repo_root = daemon.data_root().join("primary-branch-repo");
    init_git_repo_for_primary_branch_route_test(&repo_root);
    let workspace = daemon
        .seed_workspace_for_test("primary", &repo_root, VcsKind::Git)
        .await
        .expect("seed workspace");
    let first = create_route_contract_worktree(&daemon, &workspace, "primary-wt-a", None).await;
    let second = create_route_contract_worktree(&daemon, &workspace, "primary-wt-b", None).await;

    let attempts = Arc::new(Mutex::new(Vec::new()));
    let refresh_attempts = Arc::clone(&attempts);
    let refresh = Arc::new(move |worktree: Worktree| {
        let refresh_attempts = Arc::clone(&refresh_attempts);
        Box::pin(async move {
            refresh_attempts.lock().await.push(worktree.id);
            Err(anyhow::anyhow!("synthetic refresh failure"))
        })
            as std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>
    });
    let handle = daemon.workspace_primary_branch_with_refresh_effect_for_test(refresh);

    let response = handle
        .update_workspace_primary_branch_for_route_params(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            UpdateWorkspacePrimaryBranchRequest {
                primary_branch: "main".to_string(),
            },
        )
        .await
        .expect("update primary branch");
    assert_eq!(response.primary_branch, "main");
    assert_eq!(
        daemon
            .workspace_primary_branch_for_test(workspace.id)
            .await
            .expect("load persisted primary branch")
            .as_deref(),
        Some("main")
    );

    let attempts = attempts.lock().await.clone();
    assert_eq!(attempts.len(), 2);
    assert!(attempts.contains(&first.id));
    assert!(attempts.contains(&second.id));
}

#[tokio::test]
async fn registry_route_params_reject_invalid_workspace_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_registry_handle_for_test();
    let error = handle
        .get_workspace_for_route_params(WorkspaceRouteParams::new("not-a-workspace"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid workspace id");
}

#[tokio::test]
async fn registry_route_maps_missing_workspace_to_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_registry_handle_for_test();
    let error = handle
        .get_workspace_for_route_params(WorkspaceRouteParams::new(uuid::Uuid::new_v4().to_string()))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(error.message(), "workspace not found");
}

#[tokio::test]
async fn delete_workspace_route_params_reject_invalid_workspace_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_deletion_handle_for_test();
    let error = handle
        .delete_workspace_for_route(WorkspaceRouteParams::new("not-a-workspace"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid workspace id");
}

#[tokio::test]
async fn delete_workspace_route_maps_missing_workspace_to_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_deletion_handle_for_test();
    let error = handle
        .delete_workspace_for_route(WorkspaceRouteParams::new(uuid::Uuid::new_v4().to_string()))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(error.message(), "workspace not found");
}

#[tokio::test]
async fn delete_workspace_route_removes_workspace_indexes_and_db_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = create_route_contract_workspace_with_store(&daemon, "delete-success").await;
    let worktree =
        create_route_contract_worktree(&daemon, &workspace, "delete-success-worktree", None).await;
    let workspace_db_dir = daemon
        .data_root()
        .join("db")
        .join("workspaces")
        .join(workspace.id.0.to_string());
    std::fs::create_dir_all(&workspace_db_dir).expect("create workspace db dir");

    daemon
        .workspace_deletion_handle_for_test()
        .delete_workspace_for_route(WorkspaceRouteParams::new(workspace.id.0.to_string()))
        .await
        .expect("delete workspace");

    assert!(daemon
        .global_store()
        .get_workspace(workspace.id)
        .await
        .expect("load workspace")
        .is_none());
    assert!(daemon
        .global_store()
        .get_workspace_id_for_worktree(worktree.id)
        .await
        .expect("load worktree index")
        .is_none());
    assert!(!workspace_db_dir.exists());
    assert!(!daemon.stores().is_workspace_deleting(workspace.id).await);
}

#[tokio::test]
async fn delete_workspace_route_continues_when_workspace_store_is_unavailable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace =
        create_route_contract_workspace_with_store(&daemon, "delete-store-missing").await;
    let worktree =
        create_route_contract_worktree(&daemon, &workspace, "delete-store-missing-worktree", None)
            .await;
    daemon.stores().begin_workspace_delete(workspace.id).await;

    daemon
        .workspace_deletion_handle_for_test()
        .delete_workspace_for_route(WorkspaceRouteParams::new(workspace.id.0.to_string()))
        .await
        .expect("delete workspace with blocked store");

    assert!(!daemon.stores().is_workspace_deleting(workspace.id).await);
    assert!(daemon
        .global_store()
        .get_workspace(workspace.id)
        .await
        .expect("load workspace")
        .is_none());
    assert!(daemon
        .global_store()
        .get_workspace_id_for_worktree(worktree.id)
        .await
        .expect("load worktree index")
        .is_none());
}

#[tokio::test]
async fn delete_workspace_route_finishes_delete_barrier_after_post_begin_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = create_route_contract_workspace_with_store(&daemon, "delete-failure").await;
    let handle = daemon.workspace_deletion_handle_for_test();
    handle.fail_next_delete_after_begin_for_test();

    let error = handle
        .delete_workspace_for_route(WorkspaceRouteParams::new(workspace.id.0.to_string()))
        .await
        .unwrap_err();

    assert_eq!(error.kind(), WorkspaceRouteErrorKind::Internal);
    assert_eq!(error.message(), "failed to delete workspace");
    assert!(!daemon.stores().is_workspace_deleting(workspace.id).await);
    assert!(daemon
        .global_store()
        .get_workspace(workspace.id)
        .await
        .expect("load workspace")
        .is_some());
}

#[test]
fn workspace_route_response_matches_workspace_wire_shape() {
    let workspace = Workspace {
        id: ctx_core::ids::WorkspaceId::new(),
        name: "workspace".to_string(),
        root_path: "/tmp/workspace".to_string(),
        created_at: Utc::now(),
        vcs_kind: Some(VcsKind::Git),
    };
    assert_same_json(WorkspaceRouteResponse::from(workspace.clone()), workspace);
}

#[test]
fn worktree_route_response_matches_worktree_wire_shape() {
    let now = Utc::now();
    let worktree = Worktree {
        id: ctx_core::ids::WorktreeId::new(),
        workspace_id: ctx_core::ids::WorkspaceId::new(),
        root_path: "/tmp/workspace/wt".to_string(),
        base_commit_sha: "abc123".to_string(),
        git_branch: Some("feature".to_string()),
        vcs_kind: Some(VcsKind::Git),
        base_revision: Some("rev-a".to_string()),
        vcs_ref: Some("main".to_string()),
        created_at: now,
        bootstrap_status: Some(WorktreeBootstrapStatus::Success),
        bootstrap_started_at: Some(now),
        bootstrap_finished_at: Some(now),
        bootstrap_exit_code: Some(0),
        bootstrap_timeout_sec: Some(60),
        bootstrap_error: Some("none".to_string()),
        bootstrap_log_path: Some("/tmp/bootstrap.log".to_string()),
        bootstrap_log_truncated: Some(false),
        bootstrap_command: Some("true".to_string()),
        bootstrap_script_path: Some("/tmp/bootstrap.sh".to_string()),
    };
    assert_same_json(WorktreeRouteResponse::from(worktree.clone()), worktree);
}

#[test]
fn workspace_attachment_route_response_matches_attachment_wire_shape() {
    let now = Utc::now();
    let attachment = WorkspaceAttachment {
        id: ctx_core::ids::WorkspaceAttachmentId::new(),
        workspace_id: ctx_core::ids::WorkspaceId::new(),
        kind: WorkspaceAttachmentKind::ReferenceRepo,
        name: "ref".to_string(),
        source: "https://example.test/repo.git".to_string(),
        revision: Some("main".to_string()),
        subpath: None,
        mount_relpath: "refs/ref".to_string(),
        mode: AttachmentMode::Ro,
        update_policy: AttachmentUpdatePolicy::Manual,
        status: WorkspaceAttachmentStatus::Pending,
        last_sync_at: None,
        error_message: Some("waiting".to_string()),
        created_at: now,
        updated_at: now,
    };
    assert_same_json(
        WorkspaceAttachmentRouteResponse::from(attachment.clone()),
        attachment,
    );
}

#[test]
fn active_workspace_route_wrappers_match_active_wire_shape() {
    let workspace_id = ctx_core::ids::WorkspaceId::new();
    let snapshot = WorkspaceActiveSnapshot {
        workspace_id,
        snapshot_rev: 7,
        archived_rev: 3,
        active: ctx_core::models::WorkspaceActivePage {
            tasks: Vec::new(),
            total_count: 0,
        },
    };
    assert_same_json(
        WorkspaceActiveSnapshotRouteResponse::from(snapshot.clone()),
        snapshot,
    );

    let heads = WorkspaceActiveHeadBatch {
        workspace_id,
        snapshot_rev: 7,
        heads: Vec::new(),
    };
    assert_same_json(
        WorkspaceActiveHeadBatchRouteResponse::from(heads.clone()),
        heads,
    );
}

#[test]
fn workspace_route_params_parse_invalid_ids_to_route_errors() {
    let workspace = WorkspaceRouteParams::new("not-a-workspace")
        .parse_workspace_id()
        .unwrap_err();
    assert_eq!(workspace.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(workspace.message(), "invalid workspace id");

    let worktree = WorktreeRouteParams::new("not-a-worktree")
        .parse_worktree_id()
        .unwrap_err();
    assert_eq!(worktree.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(worktree.message(), "invalid worktree id");
}

#[test]
fn workspace_route_error_helpers_preserve_status_classes() {
    let hydration = workspace_hydration_route_error(WorkspaceHydrationError::NotFound);
    assert_eq!(hydration.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(hydration.message(), "workspace not found");

    let deletion = workspace_delete_route_error(super::super::WorkspaceDeleteError::NotFound);
    assert_eq!(deletion.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(deletion.message(), "workspace not found");

    let download = route_file_download_error(crate::daemon::RouteFileDownloadError::NotFound);
    assert_eq!(download.kind(), WorkspaceRouteErrorKind::NotFound);

    let harness_status = workspace_harness_container_status_error(
        WorkspaceHarnessContainerError::ExecutionSettings(
            ctx_settings_service::EffectiveExecutionSettingsError::Internal(anyhow::anyhow!(
                "settings failed"
            )),
        ),
    );
    assert_eq!(harness_status.kind(), WorkspaceRouteErrorKind::Internal);

    let harness_ensure = workspace_harness_container_ensure_error(
        WorkspaceHarnessContainerError::Ensure(anyhow::anyhow!("bad container request")),
    );
    assert_eq!(harness_ensure.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(harness_ensure.message(), "bad container request");
}

#[tokio::test]
async fn worktree_routes_reject_invalid_worktree_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_worktree_handle_for_test();

    let get_error = handle
        .get_worktree_for_route_params(WorktreeRouteParams::new("not-a-worktree"))
        .await
        .unwrap_err();
    assert_eq!(get_error.kind(), WorkspaceRouteErrorKind::BadRequest);

    let log_error = handle
        .download_worktree_bootstrap_logs_for_route_params(WorktreeRouteParams::new(
            "not-a-worktree",
        ))
        .await
        .unwrap_err();
    assert_eq!(log_error.kind(), WorkspaceRouteErrorKind::BadRequest);
}

#[tokio::test]
async fn worktree_routes_map_missing_worktree_to_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_worktree_handle_for_test();
    let missing = WorktreeId::new().0.to_string();

    let get_error = handle
        .get_worktree_for_route_params(WorktreeRouteParams::new(missing.clone()))
        .await
        .unwrap_err();
    assert_eq!(get_error.kind(), WorkspaceRouteErrorKind::NotFound);

    let log_error = handle
        .download_worktree_bootstrap_logs_for_route_params(WorktreeRouteParams::new(missing))
        .await
        .unwrap_err();
    assert_eq!(log_error.kind(), WorkspaceRouteErrorKind::NotFound);
}

#[tokio::test]
async fn worktree_bootstrap_logs_route_rejects_missing_blank_and_outside_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = create_route_contract_workspace_with_store(&daemon, "worktree-logs").await;
    let missing_path =
        create_route_contract_worktree(&daemon, &workspace, "missing-log", None).await;
    let blank_path =
        create_route_contract_worktree(&daemon, &workspace, "blank-log", Some(" ".to_string()))
            .await;
    let outside_path = temp.path().join("outside-bootstrap.log");
    std::fs::write(&outside_path, "outside").expect("write outside log");
    let log_root = ctx_observability::logs::logs_dir(daemon.data_root()).join("worktree-bootstrap");
    std::fs::create_dir_all(&log_root).expect("create bootstrap log root");
    let outside_path = create_route_contract_worktree(
        &daemon,
        &workspace,
        "outside-log",
        Some(outside_path.to_string_lossy().to_string()),
    )
    .await;
    let handle = daemon.workspace_worktree_handle_for_test();

    for worktree in [missing_path, blank_path, outside_path] {
        let error = handle
            .download_worktree_bootstrap_logs_for_route_params(WorktreeRouteParams::new(
                worktree.id.0.to_string(),
            ))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), WorkspaceRouteErrorKind::NotFound);
    }
}

#[tokio::test]
async fn harness_container_routes_reject_invalid_workspace_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_harness_container_handle_for_test();

    let status_error = handle
        .workspace_harness_container_status_for_route_params(WorkspaceRouteParams::new(
            "not-a-workspace",
        ))
        .await
        .unwrap_err();
    assert_eq!(status_error.kind(), WorkspaceRouteErrorKind::BadRequest);

    let stop_error = handle
        .stop_workspace_harness_container_for_route(WorkspaceRouteParams::new("not-a-workspace"))
        .await
        .unwrap_err();
    assert_eq!(stop_error.kind(), WorkspaceRouteErrorKind::BadRequest);

    let ensure_error = handle
        .ensure_workspace_harness_container_for_route(WorkspaceRouteParams::new("not-a-workspace"))
        .await
        .unwrap_err();
    assert_eq!(ensure_error.kind(), WorkspaceRouteErrorKind::BadRequest);
}

#[tokio::test]
async fn harness_container_routes_map_missing_workspace_to_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_harness_container_handle_for_test();
    let missing = WorkspaceId::new().0.to_string();

    let status_error = handle
        .workspace_harness_container_status_for_route_params(WorkspaceRouteParams::new(
            missing.clone(),
        ))
        .await
        .unwrap_err();
    assert_eq!(status_error.kind(), WorkspaceRouteErrorKind::NotFound);

    let stop_error = handle
        .stop_workspace_harness_container_for_route(WorkspaceRouteParams::new(missing.clone()))
        .await
        .unwrap_err();
    assert_eq!(stop_error.kind(), WorkspaceRouteErrorKind::NotFound);

    let ensure_error = handle
        .ensure_workspace_harness_container_for_route(WorkspaceRouteParams::new(missing))
        .await
        .unwrap_err();
    assert_eq!(ensure_error.kind(), WorkspaceRouteErrorKind::NotFound);
}

#[tokio::test]
async fn harness_container_routes_are_hermetic_without_running_container() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = create_route_contract_workspace_with_store(&daemon, "no-container").await;
    let handle = daemon.workspace_harness_container_handle_for_test();
    let params = WorkspaceRouteParams::new(workspace.id.0.to_string());

    let status = handle
        .workspace_harness_container_status_for_route_params(params.clone())
        .await
        .expect("status route");
    assert!(status.is_none());

    let stop_error = handle
        .stop_workspace_harness_container_for_route(params)
        .await
        .unwrap_err();
    assert_eq!(stop_error.kind(), WorkspaceRouteErrorKind::NotFound);
}

#[tokio::test]
async fn ensure_harness_container_preserves_settings_error_mapping() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace =
        create_route_contract_workspace_with_store(&daemon, "invalid-runtime-settings").await;
    daemon
        .seed_invalid_workspace_runtime_settings_document_for_test(workspace.id, "{ not json")
        .await
        .expect("seed invalid runtime settings");
    let handle = daemon.workspace_harness_container_handle_for_test();

    let error = handle
        .ensure_workspace_harness_container_for_route(WorkspaceRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert!(
        error.message().contains("workspace runtime settings"),
        "unexpected error: {}",
        error.message()
    );
}

#[test]
fn workspace_file_completions_query_preserves_http_query_shape() {
    let empty: WorkspaceFileCompletionsRouteQuery =
        serde_json::from_value(serde_json::json!({})).expect("empty query shape");
    assert_eq!(empty, WorkspaceFileCompletionsRouteQuery::default());

    let populated: WorkspaceFileCompletionsRouteQuery = serde_json::from_value(serde_json::json!({
        "query": "src",
        "limit": 25,
    }))
    .expect("populated query shape");
    let (query, limit) = populated.into_parts();
    assert_eq!(query.as_deref(), Some("src"));
    assert_eq!(limit, Some(25));
}

#[test]
fn workspace_file_completion_storage_errors_map_to_507_class() {
    let error = super::super::FileCompletionsError::from_internal_error(
        "resolving data plane",
        anyhow::anyhow!("No space left on device"),
    );
    let route_error = file_completions_route_error(error);
    assert_eq!(
        route_error.kind(),
        WorkspaceRouteErrorKind::InsufficientStorage
    );
}

#[test]
fn workspace_store_route_error_preserves_not_found_vs_unavailable() {
    let not_found = super::super::workspace_store_route_error(
        crate::daemon::WorkspaceStoreAccessError::NotFound,
    );
    assert_eq!(not_found.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(not_found.message(), "workspace not found");

    let unavailable = super::super::workspace_store_route_error(
        crate::daemon::WorkspaceStoreAccessError::Unavailable(anyhow::anyhow!("store offline")),
    );
    assert_eq!(unavailable.kind(), WorkspaceRouteErrorKind::Internal);
}

#[tokio::test]
async fn attachment_route_params_reject_invalid_workspace_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_attachments_handle_for_test();
    let error = handle
        .create_and_sync_workspace_attachment_for_route_params(
            WorkspaceRouteParams::new("not-a-workspace"),
            serde_json::from_value(serde_json::json!({
                "kind": "reference_repo",
                "name": "ref",
                "source": "/tmp/ref"
            }))
            .expect("attachment request"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid workspace id");
}

#[tokio::test]
async fn attachment_routes_treat_deleting_workspace_as_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = create_route_contract_workspace(&daemon, "deleting-attachments").await;
    daemon.stores().begin_workspace_delete(workspace.id).await;
    let handle = daemon.workspace_attachments_handle_for_test();
    let params = || WorkspaceRouteParams::new(workspace.id.0.to_string());

    let list_error = handle
        .list_workspace_attachments_for_route_params(params())
        .await
        .unwrap_err();
    assert_eq!(list_error.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(list_error.message(), "workspace not found");

    let sync_error = handle
        .sync_workspace_attachments_for_route_params(
            params(),
            serde_json::from_value::<SyncWorkspaceAttachmentsRouteRequest>(serde_json::json!({}))
                .expect("sync request"),
        )
        .await
        .unwrap_err();
    assert_eq!(sync_error.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(sync_error.message(), "workspace not found");

    let create_error = handle
        .create_and_sync_workspace_attachment_for_route_params(
            params(),
            serde_json::from_value(serde_json::json!({
                "kind": "reference_repo",
                "name": "ref",
                "source": "/tmp/ref"
            }))
            .expect("attachment request"),
        )
        .await
        .unwrap_err();
    assert_eq!(create_error.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(create_error.message(), "workspace not found");
    daemon.stores().finish_workspace_delete(workspace.id).await;
}

#[tokio::test]
async fn merge_queue_config_route_params_reject_invalid_workspace_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_merge_queue_config_handle_for_test();
    let error = handle
        .workspace_merge_queue_config_for_route_params(WorkspaceRouteParams::new("not-a-workspace"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid workspace id");
}

#[tokio::test]
async fn merge_queue_config_routes_treat_deleting_workspace_as_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = create_route_contract_workspace(&daemon, "deleting-merge-queue-config").await;
    daemon.stores().begin_workspace_delete(workspace.id).await;
    let handle = daemon.workspace_merge_queue_config_handle_for_test();

    let get_error = handle
        .workspace_merge_queue_config_for_route_params(WorkspaceRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
        .unwrap_err();
    assert_eq!(get_error.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(get_error.message(), "workspace not found");

    let post_error = handle
        .update_workspace_merge_queue_config_for_route_params(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            UpdateWorkspaceMergeQueueConfigRequest {
                enabled: true,
                target_branch: Some("main".to_string()),
                verify_command: None,
                push_on_success: None,
                push_remote: None,
                push_branch: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(post_error.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(post_error.message(), "workspace not found");
    daemon.stores().finish_workspace_delete(workspace.id).await;
}

#[tokio::test]
async fn merge_queue_config_routes_map_unavailable_workspace_store_to_internal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = create_route_contract_workspace(&daemon, "unavailable-merge-queue").await;
    daemon
        .cache_rehydration_make_workspace_store_unopenable_for_test(workspace.id)
        .await
        .expect("block workspace store");
    let handle = daemon.workspace_merge_queue_config_handle_for_test();

    let get_error = handle
        .workspace_merge_queue_config_for_route_params(WorkspaceRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
        .unwrap_err();
    assert_eq!(get_error.kind(), WorkspaceRouteErrorKind::Internal);

    let post_error = handle
        .update_workspace_merge_queue_config_for_route_params(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            UpdateWorkspaceMergeQueueConfigRequest {
                enabled: true,
                target_branch: Some("main".to_string()),
                verify_command: None,
                push_on_success: None,
                push_remote: None,
                push_branch: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(post_error.kind(), WorkspaceRouteErrorKind::Internal);
}

#[tokio::test]
async fn merge_queue_config_disable_transition_cancels_queued_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace =
        create_route_contract_workspace_with_store(&daemon, "disable-merge-queue").await;
    let handle = daemon.workspace_merge_queue_config_handle_for_test();
    handle
        .update_workspace_merge_queue_config_for_route_params(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            UpdateWorkspaceMergeQueueConfigRequest {
                enabled: true,
                target_branch: Some("main".to_string()),
                verify_command: None,
                push_on_success: None,
                push_remote: None,
                push_branch: None,
            },
        )
        .await
        .expect("enable merge queue");
    let entry = daemon
        .seed_workspace_merge_queue_queued_entry_for_test(workspace.id, "queued-before-disable")
        .await
        .expect("seed queued entry");

    handle
        .update_workspace_merge_queue_config_for_route_params(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            UpdateWorkspaceMergeQueueConfigRequest {
                enabled: false,
                target_branch: Some("main".to_string()),
                verify_command: None,
                push_on_success: None,
                push_remote: None,
                push_branch: None,
            },
        )
        .await
        .expect("disable merge queue");

    let disabled = daemon
        .load_workspace_merge_queue_entry_for_test(workspace.id, entry.id)
        .await
        .expect("load entry");
    assert_eq!(disabled.status, MergeQueueEntryStatus::Cancelled);
    assert_eq!(
        disabled.error_message.as_deref(),
        Some("merge queue disabled while entry was queued")
    );
}

#[tokio::test]
async fn merge_queue_config_unchanged_disabled_state_does_not_cancel_queued_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace =
        create_route_contract_workspace_with_store(&daemon, "unchanged-merge-queue").await;
    let entry = daemon
        .seed_workspace_merge_queue_queued_entry_for_test(workspace.id, "queued-while-disabled")
        .await
        .expect("seed queued entry");
    let handle = daemon.workspace_merge_queue_config_handle_for_test();

    handle
        .update_workspace_merge_queue_config_for_route_params(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            UpdateWorkspaceMergeQueueConfigRequest {
                enabled: false,
                target_branch: Some("main".to_string()),
                verify_command: None,
                push_on_success: None,
                push_remote: None,
                push_branch: None,
            },
        )
        .await
        .expect("keep merge queue disabled");

    let unchanged = daemon
        .load_workspace_merge_queue_entry_for_test(workspace.id, entry.id)
        .await
        .expect("load entry");
    assert_eq!(unchanged.status, MergeQueueEntryStatus::Queued);
    assert_ne!(
        unchanged.error_message.as_deref(),
        Some("merge queue disabled while entry was queued")
    );
}

#[tokio::test]
async fn worktree_bootstrap_route_params_reject_invalid_workspace_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_prompt_bootstrap_config_handle_for_test();
    let error = handle
        .worktree_bootstrap_config_for_route_params(WorkspaceRouteParams::new("not-a-workspace"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid workspace id");
}

#[tokio::test]
async fn prompt_config_route_params_reject_invalid_workspace_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let handle = daemon.workspace_prompt_bootstrap_config_handle_for_test();
    let error = handle
        .agent_system_prompt_config_for_route(WorkspacePromptConfigRouteParams::new(
            "not-a-workspace",
        ))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid workspace id");
}

#[tokio::test]
async fn prompt_bootstrap_config_routes_treat_deleting_workspace_as_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = create_route_contract_workspace(&daemon, "deleting-prompt-bootstrap").await;
    daemon.stores().begin_workspace_delete(workspace.id).await;
    let handle = daemon.workspace_prompt_bootstrap_config_handle_for_test();

    let bootstrap_error = handle
        .worktree_bootstrap_config_for_route_params(WorkspaceRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
        .unwrap_err();
    assert_eq!(bootstrap_error.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(bootstrap_error.message(), "workspace not found");

    let prompt_error = handle
        .agent_system_prompt_config_for_route(WorkspacePromptConfigRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
        .unwrap_err();
    assert_eq!(prompt_error.kind(), WorkspaceRouteErrorKind::NotFound);
    assert_eq!(prompt_error.message(), "workspace not found");
    daemon.stores().finish_workspace_delete(workspace.id).await;
}

#[tokio::test]
async fn prompt_bootstrap_config_routes_map_unavailable_workspace_store_to_internal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = create_route_contract_workspace(&daemon, "unavailable-prompt-bootstrap").await;
    daemon
        .cache_rehydration_make_workspace_store_unopenable_for_test(workspace.id)
        .await
        .expect("block workspace store");
    let handle = daemon.workspace_prompt_bootstrap_config_handle_for_test();

    let bootstrap_error = handle
        .worktree_bootstrap_config_for_route_params(WorkspaceRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
        .unwrap_err();
    assert_eq!(bootstrap_error.kind(), WorkspaceRouteErrorKind::Internal);

    let prompt_error = handle
        .agent_system_prompt_config_for_route(WorkspacePromptConfigRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
        .unwrap_err();
    assert_eq!(prompt_error.kind(), WorkspaceRouteErrorKind::Internal);
}

#[tokio::test]
async fn prompt_bootstrap_config_routes_preserve_malformed_runtime_settings_statuses() {
    let temp = tempfile::tempdir().expect("tempdir");
    let daemon =
        TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
            .await
            .expect("test daemon");
    let workspace = create_route_contract_workspace(&daemon, "invalid-prompt-bootstrap").await;
    daemon
        .seed_invalid_workspace_runtime_settings_document_for_test(workspace.id, "{ not json")
        .await
        .expect("seed invalid runtime settings");
    let handle = daemon.workspace_prompt_bootstrap_config_handle_for_test();

    let bootstrap_get_error = handle
        .worktree_bootstrap_config_for_route_params(WorkspaceRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
        .unwrap_err();
    assert_eq!(
        bootstrap_get_error.kind(),
        WorkspaceRouteErrorKind::Internal
    );

    let bootstrap_post_error = handle
        .update_worktree_bootstrap_config_for_route_params(
            WorkspaceRouteParams::new(workspace.id.0.to_string()),
            UpdateWorktreeBootstrapConfigRequest {
                setup_command: Some("true".to_string()),
                timeout_sec: Some(30),
                wait_for_completion: Some(true),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        bootstrap_post_error.kind(),
        WorkspaceRouteErrorKind::BadRequest
    );

    let prompt_get_error = handle
        .agent_system_prompt_config_for_route(WorkspacePromptConfigRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
        .unwrap_err();
    assert_eq!(prompt_get_error.kind(), WorkspaceRouteErrorKind::Internal);

    let prompt_post_error = handle
        .update_agent_system_prompt_config_for_route(
            WorkspacePromptConfigRouteParams::new(workspace.id.0.to_string()),
            UpdateAgentSystemPromptConfigRouteRequest {
                system_prompt_append: Some("prompt".to_string()),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(prompt_post_error.kind(), WorkspaceRouteErrorKind::Internal);
}
