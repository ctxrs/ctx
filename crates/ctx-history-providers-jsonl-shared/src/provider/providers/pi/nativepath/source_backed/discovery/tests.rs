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

    // An explicitly targeted changes.jsonl file is still validated and rejected.
    let explicit = discover(&adapter, &journal).unwrap();
    assert_eq!(explicit.accepted_len(), 0);
    assert_eq!(explicit.quarantined_leaves().count(), 1);

    // Non-journal invalid JSONL files beside sessions still surface as quarantined.
    fs::write(project.join("corrupted.jsonl"), "{\"not\":\"a session\"}\n").unwrap();
    let with_corrupted = discover(&adapter, &sessions).unwrap();
    assert_eq!(with_corrupted.accepted_len(), 1);
    assert_eq!(with_corrupted.quarantined_leaves().count(), 1);
}
