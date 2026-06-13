use super::*;

#[test]
fn recent_workspaces_are_deduplicated_and_ordered() {
    let registry = WorkspaceWindowRegistry::default();

    registry.record_recent_workspace("ws-a", Some("Workspace A"));
    registry.record_recent_workspace("ws-b", Some("Workspace B"));
    registry.record_recent_workspace("ws-a", Some("Workspace A Renamed"));

    let recent = registry.recent_workspaces();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].workspace_id, "ws-a");
    assert_eq!(recent[0].label, "Workspace A Renamed");
    assert_eq!(recent[1].workspace_id, "ws-b");
}

#[test]
fn recent_workspaces_are_trimmed_to_max_size() {
    let registry = WorkspaceWindowRegistry::default();
    for idx in 0..(MAX_RECENT_WORKSPACES + 4) {
        registry.record_recent_workspace(&format!("ws-{idx}"), Some(&format!("Workspace {idx}")));
    }

    let recent = registry.recent_workspaces();
    assert_eq!(recent.len(), MAX_RECENT_WORKSPACES);
    assert_eq!(
        recent[0].workspace_id,
        format!("ws-{}", MAX_RECENT_WORKSPACES + 3)
    );
}

#[test]
fn recent_workspace_uses_workspace_id_when_label_missing() {
    let registry = WorkspaceWindowRegistry::default();
    registry.record_recent_workspace("ws-abc", Some("   "));
    let recent = registry.recent_workspaces();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].label, "ws-abc");
}

#[test]
fn set_window_workspaces_replaces_stale_workspace_mappings() {
    let registry = WorkspaceWindowRegistry::default();
    registry.register("window-a", "ws-a");
    registry.register("window-a", "ws-b");
    registry.set_window_workspaces("window-a", vec!["ws-c".to_string()]);

    assert_eq!(registry.window_for_workspace("ws-a"), None);
    assert_eq!(registry.window_for_workspace("ws-b"), None);
    assert_eq!(
        registry.window_for_workspace("ws-c").as_deref(),
        Some("window-a")
    );
}

#[test]
fn workspace_lookup_uses_daemon_target_when_provided() {
    let registry = WorkspaceWindowRegistry::default();
    registry.register_for_daemon("local-window", Some("local"), "ws-shared");
    registry.register_for_daemon("remote-window", Some("ssh|host||8787|"), "ws-shared");

    assert_eq!(registry.window_for_workspace("ws-shared"), None);
    assert_eq!(
        registry
            .window_for_workspace_for_daemon("ws-shared", Some("local"))
            .as_deref(),
        Some("local-window")
    );
    assert_eq!(
        registry
            .window_for_workspace_for_daemon("ws-shared", Some("ssh|host||8787|"))
            .as_deref(),
        Some("remote-window")
    );
}

fn workbench_route_req(
    workspace_id: &str,
    active_task_id: Option<&str>,
    active_session_id: Option<&str>,
    open_tasks: Vec<(&str, Option<&str>)>,
) -> DesktopRecordWorkbenchRouteReq {
    DesktopRecordWorkbenchRouteReq {
        active_session_id: active_session_id.map(str::to_string),
        active_task_id: active_task_id.map(str::to_string),
        open_tasks: open_tasks
            .into_iter()
            .map(|(task_id, session_id)| DesktopWorkbenchRouteTask {
                task_id: task_id.to_string(),
                session_id: session_id.map(str::to_string),
            })
            .collect(),
        workspace_id: workspace_id.to_string(),
        workspace_label: workspace_id.to_string(),
    }
}

#[test]
fn task_lookup_prefers_exact_session_over_task_and_workspace() {
    let registry = WorkspaceWindowRegistry::default();
    registry.register_for_daemon("workspace-window", Some("local"), "ws-a");
    registry.record_workbench_route_for_daemon(
        "task-window",
        Some("local"),
        workbench_route_req("ws-a", Some("task-1"), None, vec![("task-1", None)]),
    );
    registry.record_workbench_route_for_daemon(
        "session-window",
        Some("local"),
        workbench_route_req(
            "ws-a",
            Some("task-1"),
            Some("session-1"),
            vec![("task-1", Some("session-1"))],
        ),
    );

    assert_eq!(
        registry.window_for_task_target("ws-a", "task-1", Some("session-1"), Some("local"), None,),
        WorkspaceTaskWindowLookup::Match {
            window_label: "session-window".to_string(),
            reason: WorkspaceTaskWindowMatchReason::ExactSession,
        }
    );
}

#[test]
fn task_lookup_uses_daemon_target_for_same_workspace_and_task() {
    let registry = WorkspaceWindowRegistry::default();
    registry.record_workbench_route_for_daemon(
        "local-window",
        Some("local"),
        workbench_route_req("ws-shared", Some("task-1"), None, vec![("task-1", None)]),
    );
    registry.record_workbench_route_for_daemon(
        "remote-window",
        Some("ssh|host||8787|"),
        workbench_route_req("ws-shared", Some("task-1"), None, vec![("task-1", None)]),
    );

    assert_eq!(
        registry
            .window_for_task_target("ws-shared", "task-1", None, Some("ssh|host||8787|"), None,),
        WorkspaceTaskWindowLookup::Match {
            window_label: "remote-window".to_string(),
            reason: WorkspaceTaskWindowMatchReason::ExactTask,
        }
    );
}

#[test]
fn duplicate_exact_task_matches_choose_most_recent_route_publish() {
    let registry = WorkspaceWindowRegistry::default();
    registry.record_workbench_route_for_daemon(
        "older-window",
        Some("local"),
        workbench_route_req("ws-a", Some("task-1"), None, vec![("task-1", None)]),
    );
    registry.record_workbench_route_for_daemon(
        "newer-window",
        Some("local"),
        workbench_route_req("ws-a", Some("task-1"), None, vec![("task-1", None)]),
    );

    assert_eq!(
        registry.window_for_task_target("ws-a", "task-1", None, Some("local"), None),
        WorkspaceTaskWindowLookup::Match {
            window_label: "newer-window".to_string(),
            reason: WorkspaceTaskWindowMatchReason::ExactTask,
        }
    );
}

#[test]
fn duplicate_exact_task_matches_prefer_notification_source_window() {
    let registry = WorkspaceWindowRegistry::default();
    registry.record_workbench_route_for_daemon(
        "source-window",
        Some("local"),
        workbench_route_req(
            "ws-a",
            Some("task-1"),
            Some("session-1"),
            vec![("task-1", Some("session-1"))],
        ),
    );
    registry.record_workbench_route_for_daemon(
        "newer-window",
        Some("local"),
        workbench_route_req(
            "ws-a",
            Some("task-1"),
            Some("session-1"),
            vec![("task-1", Some("session-1"))],
        ),
    );

    assert_eq!(
        registry.window_for_task_target(
            "ws-a",
            "task-1",
            Some("session-1"),
            Some("local"),
            Some("source-window"),
        ),
        WorkspaceTaskWindowLookup::Match {
            window_label: "source-window".to_string(),
            reason: WorkspaceTaskWindowMatchReason::ExactSession,
        }
    );
}

#[test]
fn preferred_source_window_does_not_override_better_session_match() {
    let registry = WorkspaceWindowRegistry::default();
    registry.record_workbench_route_for_daemon(
        "source-window",
        Some("local"),
        workbench_route_req("ws-a", Some("task-1"), None, vec![("task-1", None)]),
    );
    registry.record_workbench_route_for_daemon(
        "session-window",
        Some("local"),
        workbench_route_req(
            "ws-a",
            Some("task-1"),
            Some("session-1"),
            vec![("task-1", Some("session-1"))],
        ),
    );

    assert_eq!(
        registry.window_for_task_target(
            "ws-a",
            "task-1",
            Some("session-1"),
            Some("local"),
            Some("source-window"),
        ),
        WorkspaceTaskWindowLookup::Match {
            window_label: "session-window".to_string(),
            reason: WorkspaceTaskWindowMatchReason::ExactSession,
        }
    );
}

#[test]
fn duplicate_workspace_only_matches_are_ambiguous() {
    let registry = WorkspaceWindowRegistry::default();
    registry.register_for_daemon("window-a", Some("local"), "ws-a");
    registry.register_for_daemon("window-b", Some("local"), "ws-a");

    assert_eq!(
        registry.window_for_task_target("ws-a", "missing-task", None, Some("local"), None),
        WorkspaceTaskWindowLookup::AmbiguousWorkspace {
            window_labels: vec!["window-a".to_string(), "window-b".to_string()],
        }
    );
}

#[test]
fn pending_task_routes_are_consumed_once() {
    let registry = WorkspaceWindowRegistry::default();
    let payload = DesktopTaskRoutePayload {
        route_id: "route-1".to_string(),
        session_id: Some("session-1".to_string()),
        task_id: "task-1".to_string(),
        workspace_id: "ws-a".to_string(),
    };

    registry.set_pending_task_route("window-a", payload.clone());

    assert_eq!(
        registry.consume_pending_task_route("window-a"),
        Some(payload)
    );
    assert_eq!(registry.consume_pending_task_route("window-a"), None);
}

#[test]
fn pending_task_route_ack_removes_only_matching_route() {
    let registry = WorkspaceWindowRegistry::default();
    registry.set_pending_task_route(
        "window-a",
        DesktopTaskRoutePayload {
            route_id: "route-1".to_string(),
            session_id: None,
            task_id: "task-1".to_string(),
            workspace_id: "ws-a".to_string(),
        },
    );

    registry.acknowledge_pending_task_route("window-a", "route-other");
    assert_eq!(
        registry
            .consume_pending_task_route("window-a")
            .map(|payload| payload.route_id),
        Some("route-1".to_string())
    );
}

#[test]
fn dock_recent_local_workspaces_are_deduped_and_trimmed() {
    let registry = WorkspaceWindowRegistry::default();
    registry.set_dock_recent_local_workspaces(vec![
        DockRecentLocalWorkspaceEntry {
            label: "Alpha".to_string(),
            root_path: "/tmp/alpha".to_string(),
        },
        DockRecentLocalWorkspaceEntry {
            label: "Alpha Duplicate".to_string(),
            root_path: "/tmp/alpha".to_string(),
        },
        DockRecentLocalWorkspaceEntry {
            label: "  ".to_string(),
            root_path: "/tmp/beta".to_string(),
        },
    ]);

    let entries = registry.dock_recent_local_workspaces();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].label, "Alpha");
    assert_eq!(entries[0].root_path, "/tmp/alpha");
    assert_eq!(entries[1].label, "/tmp/beta");
    assert_eq!(entries[1].root_path, "/tmp/beta");
}
