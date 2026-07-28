use super::*;

#[test]
fn discovery_accepts_exact_direct_and_workflow_subagents_in_stable_order() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let primary_b = session_path(&projects, "-project-b", "session-b");
    let primary_a = session_path(&projects, "-project-a", "session-a");
    let direct = projects.join("-project-a/session-a/subagents/agent-review.jsonl");
    let workflow_a =
        projects.join("-project-a/session-a/subagents/workflows/run-a/agent-worker.jsonl");
    let workflow_z =
        projects.join("-project-a/session-a/subagents/workflows/run-z/agent-worker.jsonl");
    let lookalikes = [
        projects.join("-project-a/session-a/subagents/review.jsonl"),
        projects.join("-project-a/session-a/subagents/workflow/run-a/agent-fake.jsonl"),
        projects.join("-project-a/session-a/subagents/workflows/agent-loose.jsonl"),
        projects.join("-project-a/session-a/subagents/workflows/run-a/not-agent.jsonl"),
        projects.join("-project-a/session-a/subagents/workflows/run-a/nested/agent-too-deep.jsonl"),
    ];
    for path in [&primary_b, &primary_a, &direct, &workflow_a, &workflow_z]
        .into_iter()
        .chain(lookalikes.iter())
    {
        write_lines(path, &[json!({})]);
    }

    let discovery = discover_projects(&projects).unwrap();
    assert_eq!(discovery.stats.project_directories, 2);
    assert_eq!(discovery.stats.selected_sessions, 5);
    assert_eq!(
        discovery
            .sessions
            .iter()
            .map(|source| source.key.provider_session_id())
            .collect::<Vec<_>>(),
        [
            "session-a",
            "session-a/subagents/agent-review",
            "session-a/subagents/workflows/run-a/agent-worker",
            "session-a/subagents/workflows/run-z/agent-worker",
            "session-b",
        ]
    );
    assert_eq!(discovery.sessions[0].layout, SessionLayout::Primary);
    assert_eq!(discovery.sessions[1].layout, SessionLayout::Subagent);
    assert_eq!(
        discovery.sessions[2].layout,
        SessionLayout::WorkflowSubagent
    );
    for source in &discovery.sessions[1..4] {
        assert_eq!(source.key.parent_provider_session_id(), Some("session-a"));
    }
    assert_eq!(
        discovery.sessions[2].key.workflow_run_id.as_deref(),
        Some("run-a")
    );
    assert!(lookalikes
        .iter()
        .all(|path| discovery.sessions.iter().all(|source| &source.path != path)));
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinks_in_approved_layouts_and_ignores_symlink_lookalikes() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let primary = session_path(&projects, "-project", "session");
    write_lines(&primary, &[json!({})]);
    let real = temp.path().join("real.jsonl");
    write_lines(&real, &[json!({})]);
    let lookalike_dir = projects.join("-project/session/subagents/workflow-lookalike");
    fs::create_dir_all(lookalike_dir.parent().unwrap()).unwrap();
    symlink(temp.path(), &lookalike_dir).unwrap();
    assert_eq!(discover_projects(&projects).unwrap().sessions.len(), 1);

    let selected = projects.join("-project/session/subagents/workflows/run/agent-selected.jsonl");
    fs::create_dir_all(selected.parent().unwrap()).unwrap();
    symlink(&real, &selected).unwrap();
    let error = discover_projects(&projects).unwrap_err();
    assert!(error
        .to_string()
        .contains("symlinked workflow subagent files"));
    fs::remove_file(&selected).unwrap();

    let primary_two = session_path(&projects, "-project", "session-two");
    write_lines(&primary_two, &[json!({})]);
    let selected_session_dir = projects.join("-project/session-two");
    symlink(temp.path(), &selected_session_dir).unwrap();
    let error = discover_projects(&projects).unwrap_err();
    assert!(error.to_string().contains("symlinked session directories"));
}

#[test]
fn discovery_has_deterministic_directory_and_total_traversal_bounds() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let project = projects.join("-project");
    fs::create_dir_all(&project).unwrap();
    for index in 0..=super::super::source::CLAUDE_MAX_DIRECTORY_ENTRIES {
        fs::write(project.join(format!("ignored-{index:05}.txt")), b"").unwrap();
    }
    let error = discover_projects(&projects).unwrap_err();
    assert!(error.to_string().contains("directory exceeds"));
}

#[test]
fn retained_discovery_rejects_same_path_root_replacement() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let primary = session_path(&projects, "-project", "session");
    write_lines(&primary, &[json!({})]);
    let discovery = discover_projects(&projects).unwrap();

    let displaced = temp.path().join("projects-displaced");
    fs::rename(&projects, &displaced).unwrap();
    write_lines(&primary, &[json!({})]);

    assert!(discovery.rediscover().is_err());
}
