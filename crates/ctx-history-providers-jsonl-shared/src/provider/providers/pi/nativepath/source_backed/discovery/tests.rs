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

const REPEATED_SESSION_ID: &str = "01a01e7d-0000-7000-8000-000000000001";

fn omp_title_slot(title: &str) -> String {
    let record = format!(
        r#"{{"type":"title","v":1,"title":"{title}","source":"auto","updatedAt":"2026-11-16T00:00:00.000Z","pad":""}}"#
    );
    let pad = " ".repeat(JsonlMutableFirstRecord::WIRE_BYTES - 1 - record.len());
    format!("{}{pad}\"}}\n", &record[..record.len() - 2])
}

fn subagent_transcript(title: &str, messages: &[&str]) -> String {
    let mut transcript = omp_title_slot(title);
    transcript.push_str(&format!(
        r#"{{"type":"session","version":3,"id":"{REPEATED_SESSION_ID}","timestamp":"2026-11-16T00:00:00.000Z","cwd":"/tmp/proj"}}"#
    ));
    transcript.push('\n');
    for (index, text) in messages.iter().enumerate() {
        transcript.push_str(&format!(
            r#"{{"type":"message","id":"{index:08x}","parentId":null,"timestamp":"2026-11-16T00:00:0{index}.000Z","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}],"timestamp":1794787201000}}}}"#
        ));
        transcript.push('\n');
    }
    transcript
}

fn write_session_dir_copy(project: &Path, session_dir: &str, contents: &str) -> PathBuf {
    let dir = project.join(session_dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Researcher.jsonl");
    fs::write(&path, contents).unwrap();
    path
}

#[test]
fn omp_title_slot_helper_writes_a_valid_fixed_width_slot() {
    let slot = omp_title_slot("Researcher");
    assert_eq!(slot.len(), JsonlMutableFirstRecord::WIRE_BYTES);
    let bytes: [u8; JsonlMutableFirstRecord::WIRE_BYTES] = slot.as_bytes().try_into().unwrap();
    assert!(is_omp_title_slot_wire(&bytes));
}

#[test]
fn copied_subagent_transcripts_admit_the_most_complete_copy() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let project = sessions.join("-tmp-proj");
    let original = subagent_transcript("Researcher", &["first", "second"]);
    let continued = subagent_transcript("Researcher", &["first", "second", "third"]);
    write_session_dir_copy(&project, "2026-11-16T00-00-00-000Z_parent", &original);
    write_session_dir_copy(&project, "2026-11-16T01-00-00-000Z_fork", &original);
    let longest =
        write_session_dir_copy(&project, "2026-11-16T02-00-00-000Z_continued", &continued);

    let adapter = PiJsonlAdapter::<()>::default();
    let inventory = discover(&adapter, &sessions).unwrap();
    assert_eq!(inventory.accepted_len(), 1);
    assert_eq!(inventory.quarantined_len(), 0);
    let admitted: Vec<&Path> = inventory
        .accepted_leaves()
        .map(|leaf| leaf.source_path())
        .collect();
    assert_eq!(
        admitted,
        vec![fs::canonicalize(&longest).unwrap().as_path()]
    );
    observe_terminal_membership(&sessions, &inventory).unwrap();
}

#[test]
fn copies_that_differ_only_in_the_omp_title_slot_are_one_session() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let project = sessions.join("-tmp-proj");
    write_session_dir_copy(
        &project,
        "2026-11-16T00-00-00-000Z_parent",
        &subagent_transcript("Researcher", &["first"]),
    );
    write_session_dir_copy(
        &project,
        "2026-11-16T01-00-00-000Z_fork",
        &subagent_transcript("Renamed researcher", &["first", "second"]),
    );

    let adapter = PiJsonlAdapter::<()>::default();
    let inventory = discover(&adapter, &sessions).unwrap();
    assert_eq!(inventory.accepted_len(), 1);
    assert_eq!(inventory.quarantined_len(), 0);
}

#[test]
fn divergent_repeated_identity_quarantines_its_members_and_keeps_peers() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let project = sessions.join("-tmp-proj");
    write_session_dir_copy(
        &project,
        "2026-11-16T00-00-00-000Z_parent",
        &subagent_transcript("Researcher", &["first", "left"]),
    );
    write_session_dir_copy(
        &project,
        "2026-11-16T01-00-00-000Z_fork",
        &subagent_transcript("Researcher", &["first", "right"]),
    );
    fs::write(
        project.join("2026-11-16T03-00-00-000Z_unrelated.jsonl"),
        concat!(
            r#"{"type":"session","version":3,"id":"01a01e7d-0000-7000-8000-000000000002","timestamp":"2026-11-16T03:00:00.000Z","cwd":"/tmp/proj"}"#,
            "\n",
        ),
    )
    .unwrap();

    let adapter = PiJsonlAdapter::<()>::default();
    let inventory = discover(&adapter, &sessions).unwrap();
    assert_eq!(inventory.accepted_len(), 1);
    assert_eq!(inventory.quarantined_len(), 2);
    let repeated = source_key_scoped(REPEATED_SESSION_ID, SourceAnchorScope::Unqualified).unwrap();
    for leaf in inventory.quarantined_leaves() {
        assert!(leaf
            .source()
            .is_some_and(|source| source.exact_descriptor_eq(&repeated)));
    }
}
