use chrono::Utc;
use serde::Serialize;

use super::*;

use ctx_core::models::{
    AttachmentMode, AttachmentUpdatePolicy, ChangeSet, Contribution, ContributionEndpoint,
    ContributionRole, RecordFidelity, RecordOrigin, RecordSource, RecordTrust, VcsKind, Workspace,
    WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot, WorkspaceAttachment,
    WorkspaceAttachmentKind, WorkspaceAttachmentStatus, Worktree, WorktreeBootstrapStatus,
};

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
fn harness_container_route_response_preserves_wire_shape() {
    let response = WorkspaceHarnessContainerStatusRouteResponse {
        name: "ctx-harness".to_string(),
        running: true,
        known: true,
        mount_mode: Some(WorkspaceHarnessContainerMountModeRouteValue::DiskIsolated),
        network_mode: Some(WorkspaceHarnessContainerNetworkModeRouteValue::Allowlist),
        allowlist: vec!["api.example.test".to_string()],
        egress_guard: Some(true),
    };

    assert_eq!(
        serde_json::to_value(response).unwrap(),
        serde_json::json!({
            "name": "ctx-harness",
            "running": true,
            "known": true,
            "mount_mode": "disk_isolated",
            "network_mode": "allowlist",
            "allowlist": ["api.example.test"],
            "egress_guard": true,
        })
    );

    let legacy = WorkspaceHarnessContainerStatusRouteResponse {
        name: "legacy".to_string(),
        running: false,
        known: false,
        mount_mode: Some(WorkspaceHarnessContainerMountModeRouteValue::Legacy),
        network_mode: Some(WorkspaceHarnessContainerNetworkModeRouteValue::LlmOnly),
        allowlist: Vec::new(),
        egress_guard: None,
    };

    assert_eq!(
        serde_json::to_value(legacy).unwrap(),
        serde_json::json!({
            "name": "legacy",
            "running": false,
            "known": false,
            "mount_mode": "legacy",
            "network_mode": "llm_only",
            "allowlist": [],
            "egress_guard": null,
        })
    );
}

#[test]
fn workspace_agent_work_route_response_preserves_graph_wire_shape() {
    let workspace_id = ctx_core::ids::WorkspaceId::new();
    let task_id = ctx_core::ids::TaskId::new();
    let change_set_id = ctx_core::ids::ChangeSetId::new();
    let contribution_id = ctx_core::ids::ContributionId::new();
    let change_set = ChangeSet {
        id: change_set_id.clone(),
        workspace_id,
        source_worktree_id: None,
        source: RecordSource::Worktree,
        origin: RecordOrigin::Agent,
        fidelity: RecordFidelity::Diff,
        trust: RecordTrust::High,
        title: Some("Unify work graph".to_string()),
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
    };
    let contribution = Contribution {
        id: contribution_id,
        workspace_id,
        change_set_id: Some(change_set_id.clone()),
        subject: ContributionEndpoint::Task {
            task_id: Some(task_id),
            id: None,
        },
        target: ContributionEndpoint::ChangeSet {
            change_set_id: change_set_id.clone(),
        },
        role: ContributionRole::Related,
        source: RecordSource::Manual,
        origin: RecordOrigin::User,
        fidelity: RecordFidelity::Declared,
        trust: RecordTrust::Medium,
        summary: Some("Task produced the change set".to_string()),
        fingerprint: None,
        issuer: None,
        metadata_json: None,
        source_records: Vec::new(),
        created_at: None,
        updated_at: None,
        schema_version: 1,
    };

    assert_eq!(
        serde_json::to_value(WorkspaceAgentWorkRouteResponse::new(
            vec![change_set.clone()],
            vec![contribution.clone()]
        ))
        .unwrap(),
        serde_json::json!({
            "change_sets": [serde_json::to_value(change_set).unwrap()],
            "contributions": [serde_json::to_value(contribution).unwrap()]
        })
    );
}

#[test]
fn workspace_agent_work_route_query_preserves_wire_shape() {
    assert_eq!(
        serde_json::to_value(WorkspaceAgentWorkRouteQuery {
            change_set_id: Some("chg-1".to_string()),
            endpoint_json: Some(r#"{"kind":"task","id":"task-1"}"#.to_string()),
            limit: Some(25),
        })
        .unwrap(),
        serde_json::json!({
            "change_set_id": "chg-1",
            "endpoint_json": "{\"kind\":\"task\",\"id\":\"task-1\"}",
            "limit": 25
        })
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
fn workspace_stream_route_params_and_errors_preserve_status_contract() {
    let workspace = WorkspaceStreamRouteParams::new("not-a-workspace")
        .parse_workspace_id()
        .unwrap_err();
    assert_eq!(workspace.kind(), WorkspaceStreamRouteErrorKind::BadRequest);
    assert_eq!(workspace.message(), "invalid workspace id");

    let not_found = WorkspaceStreamRouteError::not_found("workspace not found");
    assert_eq!(not_found.kind(), WorkspaceStreamRouteErrorKind::NotFound);
    assert_eq!(not_found.message(), "workspace not found");

    let internal = WorkspaceStreamRouteError::internal("store failed");
    assert_eq!(internal.kind(), WorkspaceStreamRouteErrorKind::Internal);
    assert_eq!(internal.message(), "store failed");
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
fn workspace_management_route_dtos_preserve_wire_shape() {
    let create: CreateWorkspaceRequest = serde_json::from_value(serde_json::json!({
        "root_path": "/tmp/workspace",
        "name": "workspace"
    }))
    .expect("create request");
    assert_eq!(create.root_path, "/tmp/workspace");
    assert_eq!(create.name.as_deref(), Some("workspace"));

    let create_without_name: CreateWorkspaceRequest = serde_json::from_value(serde_json::json!({
        "root_path": "/tmp/workspace"
    }))
    .expect("create request without name");
    assert_eq!(create_without_name.name, None);

    let update: UpdateWorkspacePrimaryBranchRequest =
        serde_json::from_value(serde_json::json!({"primary_branch": "main"}))
            .expect("primary branch request");
    assert_eq!(update.primary_branch, "main");

    assert_same_json(
        WorkspacePrimaryBranchSnapshot {
            primary_branch: "main".to_string(),
        },
        serde_json::json!({"primary_branch": "main"}),
    );
    assert_same_json(
        WorkspaceConfigUpdateResult { ok: true },
        serde_json::json!({"ok": true}),
    );
}

#[test]
fn workspace_config_route_dtos_preserve_wire_shape() {
    let execution: UpdateWorkspaceExecutionConfigRequest =
        serde_json::from_value(serde_json::json!({
            "environment": "sandbox",
            "network_mode": "allowlist",
            "allowlist": ["api.example.test"],
            "unknown": "ignored"
        }))
        .expect("execution request");
    assert_eq!(execution.environment, "sandbox");
    assert_eq!(execution.network_mode.as_deref(), Some("allowlist"));
    assert_eq!(
        execution.allowlist.as_deref(),
        Some(["api.example.test".to_string()].as_slice())
    );

    assert_same_json(
        WorkspaceExecutionConfigRouteSnapshot {
            source: "workspace".to_string(),
            environment: "sandbox".to_string(),
            network_mode: Some("allowlist".to_string()),
            allowlist: Some(vec!["api.example.test".to_string()]),
        },
        serde_json::json!({
            "source": "workspace",
            "environment": "sandbox",
            "network_mode": "allowlist",
            "allowlist": ["api.example.test"]
        }),
    );

    let merge_queue: UpdateWorkspaceMergeQueueConfigRequest =
        serde_json::from_value(serde_json::json!({
            "enabled": true,
            "target_branch": " main ",
            "verify_command": "pnpm test",
            "push_on_success": true,
            "push_remote": "origin",
            "push_branch": "dev",
            "unknown": "ignored"
        }))
        .expect("merge queue request");
    assert!(merge_queue.enabled);
    assert_eq!(merge_queue.target_branch.as_deref(), Some(" main "));
    assert_eq!(merge_queue.verify_command.as_deref(), Some("pnpm test"));
    assert_eq!(merge_queue.push_on_success, Some(true));
    assert_eq!(merge_queue.push_remote.as_deref(), Some("origin"));
    assert_eq!(merge_queue.push_branch.as_deref(), Some("dev"));

    assert_same_json(
        WorkspaceMergeQueueConfigRouteResponse {
            enabled: false,
            target_branch: "main".to_string(),
            verify_command: None,
            push_on_success: false,
            push_remote: "origin".to_string(),
            push_branch: "main".to_string(),
        },
        serde_json::json!({
            "enabled": false,
            "target_branch": "main",
            "push_on_success": false,
            "push_remote": "origin",
            "push_branch": "main"
        }),
    );

    assert_same_json(
        WorkspaceMergeQueueConfigRouteResponse {
            enabled: false,
            target_branch: "main".to_string(),
            verify_command: Some("pnpm test".to_string()),
            push_on_success: false,
            push_remote: "origin".to_string(),
            push_branch: "main".to_string(),
        },
        serde_json::json!({
            "enabled": false,
            "target_branch": "main",
            "verify_command": "pnpm test",
            "push_on_success": false,
            "push_remote": "origin",
            "push_branch": "main"
        }),
    );

    let bootstrap: UpdateWorktreeBootstrapConfigRequest =
        serde_json::from_value(serde_json::json!({
            "setup_command": "pnpm install",
            "timeout_sec": 30,
            "wait_for_completion": true,
            "unknown": "ignored"
        }))
        .expect("bootstrap request");
    assert_eq!(bootstrap.setup_command.as_deref(), Some("pnpm install"));
    assert_eq!(bootstrap.timeout_sec, Some(30));
    assert_eq!(bootstrap.wait_for_completion, Some(true));

    assert_same_json(
        WorkspaceWorktreeBootstrapConfigRouteResponse {
            setup_command: None,
            timeout_sec: None,
            wait_for_completion: None,
        },
        serde_json::json!({}),
    );
}

#[test]
fn workspace_prompt_and_provider_route_dtos_preserve_wire_shape() {
    let provider_params =
        WorkspaceProviderModelPreferenceRouteParams::new("not-a-workspace", "codex");
    let error = provider_params.parse_workspace_id().unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid workspace id");
    assert_eq!(provider_params.provider_id(), "codex");

    let prompt_params = WorkspacePromptConfigRouteParams::new("not-a-workspace");
    let error = prompt_params.parse_workspace_id().unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid workspace id");

    let provider: UpdateWorkspaceProviderModelPreferenceRouteRequest =
        serde_json::from_value(serde_json::json!({"unknown": "ignored"}))
            .expect("provider preference request");
    assert_eq!(provider.preferred_model_id, None);

    let provider: UpdateWorkspaceProviderModelPreferenceRouteRequest =
        serde_json::from_value(serde_json::json!({
            "preferred_model_id": " gpt-5.4/xhigh "
        }))
        .expect("provider preference request");
    assert_eq!(
        provider.preferred_model_id.as_deref(),
        Some(" gpt-5.4/xhigh ")
    );

    assert_same_json(
        WorkspaceProviderModelPreferenceRouteResponse::new("codex", None),
        serde_json::json!({
            "provider_id": "codex"
        }),
    );

    let agent: UpdateAgentSystemPromptConfigRouteRequest =
        serde_json::from_value(serde_json::json!({"unknown": "ignored"}))
            .expect("agent prompt request");
    assert_eq!(agent.system_prompt_append, None);

    let subagent: UpdateSubagentSystemPromptConfigRouteRequest =
        serde_json::from_value(serde_json::json!({"unknown": "ignored"}))
            .expect("subagent prompt request");
    assert_eq!(subagent.system_prompt_append, None);

    assert_same_json(
        AgentSystemPromptConfigRouteResponse::new(
            "Default",
            Some("Configured".to_string()),
            Some("Configured".to_string()),
            "config",
        ),
        serde_json::json!({
            "default_append": "Default",
            "configured_append": "Configured",
            "effective_append": "Configured",
            "source": "config"
        }),
    );

    assert_same_json(
        SubagentSystemPromptConfigRouteResponse::new(
            "Subagent default",
            None,
            Some("Subagent default".to_string()),
            "default",
        ),
        serde_json::json!({
            "default_append": "Subagent default",
            "configured_append": null,
            "effective_append": "Subagent default",
            "source": "default"
        }),
    );
}

#[test]
fn workspace_attachment_requests_preserve_validation_contracts() {
    let sync: SyncWorkspaceAttachmentsRouteRequest =
        serde_json::from_value(serde_json::json!({})).expect("sync request");
    assert!(!sync.refresh());
    let sync: SyncWorkspaceAttachmentsRouteRequest =
        serde_json::from_value(serde_json::json!({"refresh": true})).expect("sync request");
    assert!(sync.refresh());

    let create: CreateWorkspaceAttachmentRouteRequest = serde_json::from_value(serde_json::json!({
        "kind": "reference_repo",
        "name": "ref",
        "source": "https://example.test/repo.git",
        "revision": "main",
        "mount_relpath": "refs/ref",
        "mode": "ro",
        "update_policy": "manual"
    }))
    .expect("create request");
    let spec = create.into_spec().expect("valid spec");
    assert_eq!(spec.kind, WorkspaceAttachmentKind::ReferenceRepo);
    assert_eq!(spec.name, "ref");
    assert_eq!(spec.source, "https://example.test/repo.git");
    assert_eq!(spec.revision.as_deref(), Some("main"));
    assert_eq!(spec.subpath, None);
    assert_eq!(spec.mount_relpath.as_deref(), Some("refs/ref"));
    assert_eq!(spec.mode, Some(AttachmentMode::Ro));
    assert_eq!(spec.update_policy, Some(AttachmentUpdatePolicy::Manual));

    let error =
        serde_json::from_value::<CreateWorkspaceAttachmentRouteRequest>(serde_json::json!({
            "kind": "reference_repo",
            "name": " ",
            "source": "/tmp/ref"
        }))
        .expect("create request")
        .into_spec()
        .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "name and source are required");

    let delete: DeleteWorkspaceAttachmentRouteRequest = serde_json::from_value(serde_json::json!({
        "kind": "reference_repo",
        "name": "ref"
    }))
    .expect("delete request");
    let spec = delete.into_spec().expect("valid spec");
    assert_eq!(spec.kind, WorkspaceAttachmentKind::ReferenceRepo);
    assert_eq!(spec.name, "ref");

    let error = serde_json::from_value::<DeleteWorkspaceAttachmentRouteRequest>(
        serde_json::json!({"kind": "reference_repo", "name": ""}),
    )
    .expect("delete request")
    .into_spec()
    .unwrap_err();
    assert_eq!(error.kind(), WorkspaceRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "name is required");
}
