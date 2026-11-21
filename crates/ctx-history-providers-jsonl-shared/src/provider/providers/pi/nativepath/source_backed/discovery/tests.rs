use super::*;

#[test]
fn systemic_and_consistency_errors_escape_leaf_isolation() {
    let temp = tempfile::tempdir().unwrap();
    let authority = ProviderSourceRoot::open(temp.path()).unwrap();
    let path = temp.path().join("session.jsonl");
    fs::write(&path, b"{}\n").unwrap();
    for error in [
        CaptureError::Io(io::Error::new(
            io::ErrorKind::OutOfMemory,
            "allocation failed",
        )),
        CaptureError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "source disappeared",
        )),
        CaptureError::Io(io::Error::other("resource exhausted")),
        CaptureError::SystemIo {
            operation: "write inventory spill",
            source: io::Error::new(io::ErrorKind::PermissionDenied, "spill denied"),
        },
        CaptureError::SystemInvariant("invalid inventory"),
        CaptureError::SourceChangedDuringCapture,
        CaptureError::InvalidProviderTranscriptPath {
            path: path.clone(),
            reason: "Pi transcripts must remain below their selected authority",
        },
    ] {
        let expected_kind = std::mem::discriminant(&error);
        let expected_detail = error.to_string();
        let returned =
            reject_unreadable_file(&authority, &path, error, SourceAnchorScope::Unqualified)
                .unwrap_err();
        assert_eq!(std::mem::discriminant(&returned), expected_kind);
        assert_eq!(returned.to_string(), expected_detail);
    }
}

#[test]
fn omp_edit_journal_changes_jsonl_is_skipped_without_failing_import() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let project = sessions.join("-tmp-proj");
    let session_id = "2026-11-16T00-00-00-000Z_01a01e7d-c7ed-7000-a578-c944045672fe";
    let artifacts_dir = project.join(session_id);
    fs::create_dir_all(&artifacts_dir).unwrap();

    let transcript = project.join(format!("{session_id}.jsonl"));
    fs::write(
        &transcript,
        concat!(
            r#"{"type":"title","v":1,"title":"Minimal reproduction","source":"auto","updatedAt":"2026-11-16T00:00:00.000Z","pad":""}"#,
            "\n",
            r#"{"type":"session","version":3,"id":"01a01e7d-c7ed-7000-a578-c944045672fe","timestamp":"2026-11-16T00:00:00.000Z","cwd":"/tmp/proj"}"#,
            "\n",
            r#"{"type":"message","id":"11111111","parentId":null,"timestamp":"2026-11-16T00:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"Hello"}],"timestamp":1794787201000}}"#,
            "\n"
        ),
    )
    .unwrap();
    let journal = artifacts_dir.join("changes.jsonl");
    fs::write(
        &journal,
        "{\"type\":\"call\",\"agent\":\"Main\",\"tool\":\"bash\",\"start\":1794787203000,\"label\":\"echo hi\",\"end\":1794787203100}\n",
    )
    .unwrap();

    let adapter = PiJsonlAdapter::<()>::default();
    let inventory = discover(&adapter, &sessions).unwrap();
    assert_eq!(inventory.accepted_len(), 1);
    assert_eq!(inventory.quarantined_leaves().count(), 0);

    // Explicitly passing an OMP session artifacts directory directly as root still
    // skips changes.jsonl without failing or quarantining it.
    let artifacts_inventory = discover(&adapter, &artifacts_dir).unwrap();
    assert_eq!(artifacts_inventory.accepted_len(), 0);
    assert_eq!(artifacts_inventory.quarantined_leaves().count(), 0);
    let artifacts_membership =
        observe_terminal_membership(&artifacts_dir, &artifacts_inventory).unwrap();
    assert_eq!(artifacts_membership.unbound_routes().count(), 0);

    // An explicitly targeted changes.jsonl file is still validated and rejected.
    let explicit = discover(&adapter, &journal).unwrap();
    assert_eq!(explicit.accepted_len(), 0);
    assert_eq!(explicit.quarantined_leaves().count(), 1);
    let explicit_membership = observe_terminal_membership(&journal, &explicit).unwrap();
    let explicit_routes: Vec<PathBuf> = explicit_membership
        .unbound_routes()
        .map(|(path, _, _)| path.to_path_buf())
        .collect();
    assert_eq!(explicit_routes, vec![fs::canonicalize(&journal).unwrap()]);

    // Non-journal invalid JSONL files beside sessions still surface as quarantined.
    fs::write(project.join("corrupted.jsonl"), "{\"not\":\"a session\"}\n").unwrap();
    let with_corrupted = discover(&adapter, &sessions).unwrap();
    assert_eq!(with_corrupted.accepted_len(), 1);
    assert_eq!(with_corrupted.quarantined_leaves().count(), 1);
}

#[test]
fn terminal_membership_ignores_omp_changes_jsonl_churn_and_preserves_all_leaf_classes() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let project = sessions.join("-tmp-proj");
    let session_id = "2026-11-16T00-00-00-000Z_01a01e7d-c7ed-7000-a578-c944045672fe";
    let artifacts_dir = project.join(session_id);
    fs::create_dir_all(&artifacts_dir).unwrap();

    // 1. Accepted session leaf.
    let transcript = project.join(format!("{session_id}.jsonl"));
    fs::write(
        &transcript,
        concat!(
            r#"{"type":"session","version":3,"id":"01a01e7d-c7ed-7000-a578-c944045672fe","timestamp":"2026-11-16T00:00:00.000Z","cwd":"/tmp/proj"}"#,
            "\n",
            r#"{"type":"message","id":"11111111","parentId":null,"timestamp":"2026-11-16T00:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"Hello"}],"timestamp":1794787201000}}"#,
            "\n"
        ),
    )
    .unwrap();

    // 2. Quarantined observed leaf (malformed JSONL).
    let corrupted = project.join("corrupted.jsonl");
    fs::write(&corrupted, "{\"not\":\"a session\"}\n").unwrap();

    // 3. Pending leaf (OMP title slot only, session header not written yet).
    let pending = project.join("pending.jsonl");
    fs::write(
        &pending,
        r#"{"type":"title","v":1,"title":"Starting","source":"auto","updatedAt":"2026-11-16T00:00:00.000Z","pad":""}"#,
    )
    .unwrap();

    // 4. Quarantined unobserved leaf (symlink on Unix).
    #[cfg(unix)]
    let linked = {
        let outside = temp.path().join("outside.jsonl");
        fs::write(&outside, "{}\n").unwrap();
        let link = project.join("linked.jsonl");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        link
    };

    let adapter = PiJsonlAdapter::<()>::default();
    let opening = discover(&adapter, &sessions).unwrap();
    assert_eq!(opening.accepted_len(), 1);
    #[cfg(unix)]
    assert_eq!(opening.quarantined_leaves().count(), 2);
    #[cfg(not(unix))]
    assert_eq!(opening.quarantined_leaves().count(), 1);
    assert_eq!(opening.pending_leaves().count(), 1);

    let unbound_paths =
        |obs: &JsonlFamilyMembershipObservation<CaptureError>| -> BTreeSet<PathBuf> {
            obs.unbound_routes()
                .map(|(path, _, _)| path.to_path_buf())
                .collect()
        };

    let mut expected_unbound = BTreeSet::from([
        fs::canonicalize(&corrupted).unwrap(),
        fs::canonicalize(&pending).unwrap(),
    ]);
    #[cfg(unix)]
    expected_unbound.insert(std::path::absolute(&linked).unwrap());

    let initial_membership = observe_terminal_membership(&sessions, &opening).unwrap();
    assert_eq!(unbound_paths(&initial_membership), expected_unbound);

    // Creating, modifying, and removing changes.jsonl must be invisible to terminal membership.
    let journal = artifacts_dir.join("changes.jsonl");
    fs::write(&journal, "{\"type\":\"call\",\"tool\":\"bash\"}\n").unwrap();
    let after_create = observe_terminal_membership(&sessions, &opening).unwrap();
    assert_eq!(unbound_paths(&after_create), expected_unbound);

    fs::write(
        &journal,
        "{\"type\":\"call\",\"tool\":\"bash\"}\n{\"type\":\"call\",\"tool\":\"edit\"}\n",
    )
    .unwrap();
    let after_modify = observe_terminal_membership(&sessions, &opening).unwrap();
    assert_eq!(unbound_paths(&after_modify), expected_unbound);

    fs::remove_file(&journal).unwrap();
    let after_delete = observe_terminal_membership(&sessions, &opening).unwrap();
    assert_eq!(unbound_paths(&after_delete), expected_unbound);

    // Adding a real new session JSONL file IS detected as an unbound route.
    let added = project.join("second.jsonl");
    fs::write(
        &added,
        "{\"type\":\"session\",\"version\":3,\"id\":\"second\",\"timestamp\":\"2026-11-16T00:01:00.000Z\"}\n",
    )
    .unwrap();
    expected_unbound.insert(fs::canonicalize(&added).unwrap());
    let after_add = observe_terminal_membership(&sessions, &opening).unwrap();
    assert_eq!(unbound_paths(&after_add), expected_unbound);
}
