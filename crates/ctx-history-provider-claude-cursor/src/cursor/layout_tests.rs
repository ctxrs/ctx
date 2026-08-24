use super::layout::*;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

fn write_transcript(projects_root: &Path, project: &str, session_id: &str) -> PathBuf {
    let session_directory = projects_root
        .join(project)
        .join(AGENT_TRANSCRIPTS)
        .join(session_id);
    std::fs::create_dir_all(&session_directory).unwrap();
    let transcript = session_directory.join(format!("{session_id}.jsonl"));
    std::fs::write(&transcript, b"{}\n").unwrap();
    transcript
}

#[test]
fn cursor_discovers_129_and_1258_canonical_transcripts() {
    for expected in [129_usize, 1_258] {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join(PROJECTS);
        for index in 0..expected {
            write_transcript(&projects, "acme", &format!("session-{index:04}"));
        }

        let inventory = discover_cursor_transcripts(&projects);

        assert!(
            inventory.completed,
            "{expected}-transcript inventory failed: {:?}",
            inventory.issues
        );
        assert_eq!(inventory.transcripts.len(), expected);
    }
}

#[test]
fn cursor_never_enumerates_more_than_4096_unrelated_subtree_entries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".cursor");
    let projects = root.join(PROJECTS);
    let canvases = projects.join("acme/canvases");
    std::fs::create_dir_all(&canvases).unwrap();
    for index in 0..4_097 {
        std::fs::write(canvases.join(format!("canvas-{index:04}")), b"state").unwrap();
    }
    let transcript = write_transcript(&projects, "acme", "ordinary-session");

    let inventory = discover_cursor_transcripts(&root);

    assert!(inventory.completed, "{:?}", inventory.issues);
    assert_eq!(inventory.transcripts.len(), 1);
    assert_eq!(inventory.transcripts[0].path(), transcript);
    assert!(
        inventory.stats.entries_visited < 16,
        "unrelated canvas entries leaked into fixed-shape work: {:?}",
        inventory.stats
    );
    assert_eq!(
        probe_cursor_transcript_availability(&root),
        CursorTranscriptAvailability::Found
    );
}

#[test]
fn cursor_preserves_all_six_entry_points_across_reserved_name_collisions() {
    for (root_name, project_name, session_id) in [
        (".cursor", "acme", "session-a"),
        (PROJECTS, AGENT_TRANSCRIPTS, AGENT_TRANSCRIPTS),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(root_name);
        let projects = root.join(PROJECTS);
        let transcript = write_transcript(&projects, project_name, session_id);
        let session = transcript.parent().unwrap().to_path_buf();
        let agent_transcripts = session.parent().unwrap().to_path_buf();
        let project = agent_transcripts.parent().unwrap().to_path_buf();

        for input in [
            root,
            projects,
            project,
            agent_transcripts,
            session,
            transcript.clone(),
        ] {
            let inventory = discover_cursor_transcripts(&input);
            assert!(
                inventory.completed,
                "entry point {} failed: {:?}",
                input.display(),
                inventory.issues
            );
            assert_eq!(
                inventory.transcripts.len(),
                1,
                "entry point {} selected the wrong cardinality",
                input.display()
            );
            assert_eq!(inventory.transcripts[0].path(), transcript);
            assert!(
                inventory.stats.entries_visited < 64,
                "{:?}",
                inventory.stats
            );
            assert_eq!(
                probe_cursor_transcript_availability(&input),
                CursorTranscriptAvailability::Found
            );
        }
    }
}

#[test]
fn cursor_rejects_every_lower_entry_point_outside_literal_projects() {
    let temp = tempfile::tempdir().unwrap();
    let not_projects = temp.path().join("not-projects");
    let transcript = write_transcript(&not_projects, "acme", "session-a");
    let session = transcript.parent().unwrap().to_path_buf();
    let agent_transcripts = session.parent().unwrap().to_path_buf();
    let project = agent_transcripts.parent().unwrap().to_path_buf();

    for input in [
        not_projects,
        project,
        agent_transcripts,
        session,
        transcript,
    ] {
        let inventory = discover_cursor_transcripts(&input);
        assert!(
            !inventory.completed,
            "loose entry point {} unexpectedly completed",
            input.display()
        );
        assert!(
            inventory.has_issue_kind(CursorDiscoveryIssueKind::InvalidLayout),
            "loose entry point {} was not rejected as invalid: {:?}",
            input.display(),
            inventory.issues
        );
        assert!(
            inventory.transcripts.is_empty(),
            "loose entry point {} selected a transcript",
            input.display()
        );
        assert_ne!(
            probe_cursor_transcript_availability(&input),
            CursorTranscriptAvailability::Found,
            "loose entry point {} proved availability",
            input.display()
        );
    }
}

#[test]
fn cursor_missing_input_is_not_found_but_complete_inventory_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join(".cursor").join(PROJECTS);

    assert_eq!(
        probe_cursor_transcript_availability(&missing),
        CursorTranscriptAvailability::NotFound
    );
    let inventory = discover_cursor_transcripts(&missing);
    assert!(!inventory.completed);
    assert!(inventory.has_issue_kind(CursorDiscoveryIssueKind::NotFound));
    assert!(inventory.transcripts.is_empty());
}

#[test]
fn cursor_transcript_limit_plus_one_fails_complete_inventory_but_not_availability() {
    let temp = tempfile::tempdir().unwrap();
    let projects = temp.path().join(PROJECTS);
    for index in 0..=2 {
        write_transcript(&projects, "acme", &format!("session-{index}"));
    }
    let limits = CursorInventoryLimits {
        max_transcripts: 2,
        ..CursorInventoryLimits::default()
    };

    let inventory = discover_cursor_transcripts_with_limits(&projects, limits);

    assert!(!inventory.completed);
    assert!(inventory.has_issue_kind(CursorDiscoveryIssueKind::LimitExceeded));
    assert_eq!(inventory.transcripts.len(), 2);
    assert_eq!(
        probe_cursor_transcript_availability_with_limits(&projects, limits),
        CursorTranscriptAvailability::Found
    );
}

// Creates a non-regular special file (FIFO) at `path`. A FIFO shares the
// exact authority-walk rejection path as the reported Cursor `worker.sock`
// Unix-domain socket (both classify as NON_REGULAR_PROVIDER_SOURCE_REASON),
// and unlike a bound socket it is not constrained by the platform sun_path
// length limit under a deep temp directory. The socket-specific errno mapping
// is covered separately in the root_handle unix tests.
#[cfg(unix)]
fn make_special_file(path: &Path) {
    let raw = CString::new(path.as_os_str().as_bytes()).unwrap();
    let result = unsafe { libc::mkfifo(raw.as_ptr(), 0o600) };
    assert_eq!(result, 0, "mkfifo {}", path.display());
}

#[cfg(unix)]
#[test]
fn cursor_ignores_unrelated_links_and_special_files_but_fails_canonical_ones() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let projects = temp.path().join(PROJECTS);
    let transcript = write_transcript(&projects, "acme", "session-a");
    let session = transcript.parent().unwrap();
    let project = projects.join("acme");
    let canvases = project.join("canvases");
    let terminals = project.join("terminals");
    std::fs::create_dir_all(&canvases).unwrap();
    std::fs::create_dir_all(&terminals).unwrap();
    symlink(temp.path(), canvases.join("linked-state")).unwrap();
    make_special_file(&terminals.join("worker.sock"));
    make_special_file(&session.join("worker.sock"));

    let unrelated = discover_cursor_transcripts(&projects);

    assert!(
        unrelated.completed,
        "unrelated entries must not invalidate completion: {:?}",
        unrelated.issues
    );
    assert_eq!(unrelated.transcripts.len(), 1);
    assert!(!unrelated.has_issue_kind(CursorDiscoveryIssueKind::Symlink));
    assert!(!unrelated.has_issue_kind(CursorDiscoveryIssueKind::SpecialFile));

    let agent_transcripts = project.join(AGENT_TRANSCRIPTS);
    let linked_session = agent_transcripts.join("linked-session");
    std::fs::create_dir_all(&linked_session).unwrap();
    symlink(&transcript, linked_session.join("linked-session.jsonl")).unwrap();
    let special_session = agent_transcripts.join("special-session");
    std::fs::create_dir_all(&special_session).unwrap();
    make_special_file(&special_session.join("special-session.jsonl"));

    let canonical = discover_cursor_transcripts(&projects);

    assert!(!canonical.completed);
    assert!(canonical.has_issue_kind(CursorDiscoveryIssueKind::Symlink));
    assert!(canonical.has_issue_kind(CursorDiscoveryIssueKind::SpecialFile));
    assert_eq!(canonical.transcripts.len(), 1);
}

#[cfg(unix)]
#[test]
fn cursor_availability_is_existential_when_complete_inventory_is_unsafe() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let projects = temp.path().join(PROJECTS);
    let target = temp.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::create_dir_all(&projects).unwrap();
    symlink(&target, projects.join("a-linked-project")).unwrap();
    write_transcript(&projects, "z-valid-project", "ordinary-session");

    let inventory = discover_cursor_transcripts(&projects);

    assert!(!inventory.completed);
    assert!(inventory.has_issue_kind(CursorDiscoveryIssueKind::Symlink));
    assert_eq!(inventory.transcripts.len(), 1);
    assert_eq!(
        probe_cursor_transcript_availability(&projects),
        CursorTranscriptAvailability::Found
    );
}
