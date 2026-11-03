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
