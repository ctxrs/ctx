use super::*;

pub(super) fn enospc_preserves_current_and_allows_retry(shim: &Path) {
    // Two sources share one segment, so replacing one must expunge a live
    // segment with 50% deletes. Arm only after candidate meta publication:
    // the injected write is merge output, not clone or indexing input.
    let temporary = tempdir().unwrap();
    let root = temporary.path().join("index");
    let mut writer = GenerationWriter::open(&root, writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    let peer = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8("merge-peer.jsonl").unwrap())
            .unwrap(),
    )
    .unwrap();
    for (source, body) in [(source(), PREVIOUS_BODY), (peer, "retained peer content")] {
        writer.begin_source(source.clone()).unwrap();
        writer.add_core_record(document(&source, body)).unwrap();
        writer.certify_source(certificate(&source, 1)).unwrap();
    }
    let baseline = writer.commit(|_| true).unwrap();
    assert_eq!(
        Index::open_in_dir(active_generation_path(&root))
            .unwrap()
            .searchable_segment_metas()
            .unwrap()
            .len(),
        1
    );
    let fixture = RecoveryFixture {
        marker: temporary.path().join("child.marker"),
        result: temporary.path().join("child.result"),
        root,
        temp: temporary,
        baseline,
    };
    let pinned = VerifiedIndex::open_pinned(&fixture.root).unwrap();
    let pointer = fs::read(fixture.root.join("active-generation.json")).unwrap();
    for (operation, target) in [("write", "index_data"), ("rename", "generation_meta_final")] {
        let output = fixture.run_fault_child(
            shim,
            "commit_expect_storage_full",
            FaultCase::fail(operation, target, "ENOSPC", Some("generation_meta_rename")),
        );
        assert!(output.status.success(), "merge fault child: {output:?}");
        assert!(
            fixture.marker.is_file(),
            "merge ENOSPC injection was not reached"
        );
        let error = fs::read_to_string(&fixture.result).unwrap();
        assert!(
            error.contains("StorageFull") || error.contains("No space left"),
            "{error}"
        );
        assert_eq!(
            fs::read(fixture.root.join("active-generation.json")).unwrap(),
            pointer
        );
        assert_reader_terms(&pinned, "previous", "candidate");
    }
    let retry = staged_replacement(&fixture.root).commit(|_| true).unwrap();
    assert_ne!(retry.generation_id, fixture.baseline.generation_id);
    assert_generation(&fixture.root, &retry.generation_id, "candidate", "previous");
    assert_reader_terms(&pinned, "previous", "candidate");
}
