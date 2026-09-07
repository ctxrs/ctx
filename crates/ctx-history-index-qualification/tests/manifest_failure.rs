use super::*;

#[test]
#[ignore = "requires scripts/source-backed-recovery/run-linux-fault-tests.sh"]
fn manifest_failure_reclaims_only_its_candidate_and_preserves_visible_publication() {
    let shim = required_fault_shim();
    let make_incompatible = |root: &Path| {
        let path = active_generation_path(root).join("meta.json");
        let mut meta: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        meta["index_settings"]["docstore_blocksize"] = (64 * 1024).into();
        fs::write(path, serde_json::to_vec(&meta).unwrap()).unwrap();
        assert!(matches!(
            VerifiedIndex::open_pinned(root),
            Err(IndexError::IndexSettingsMismatch(_))
        ));
    };
    let fixture = RecoveryFixture::new();
    make_incompatible(&fixture.root);
    let pointer_before = fs::read(fixture.root.join("active-generation.json")).unwrap();
    let predecessor = active_generation_path(&fixture.root);
    let payloads = fs::read_dir(&predecessor)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect::<Vec<_>>();
    let unrelated = fixture.root.join("index-generations").join("unrelated");
    fs::create_dir(&unrelated).unwrap();
    fs::write(unrelated.join("keep"), b"unrelated residue").unwrap();
    for _ in 0..2 {
        let output = fixture.run_fault_child(
            &shim,
            "commit_expect_error",
            FaultCase::fail("write", "manifest_temp", "ENOSPC", None),
        );
        assert!(output.status.success(), "{output:?}");
        assert!(
            fixture.marker.is_file(),
            "manifest write fault was not reached"
        );
        let error = fs::read_to_string(&fixture.result).unwrap();
        assert!(
            error.contains("StorageFull") || error.contains("No space left"),
            "{error}"
        );
        assert_eq!(
            fs::read(fixture.root.join("active-generation.json")).unwrap(),
            pointer_before
        );
        for (path, bytes) in &payloads {
            assert_eq!(&fs::read(path).unwrap(), bytes);
        }
        assert_eq!(
            inactive_generation_directories(&fixture.root),
            vec![unrelated.clone()]
        );
        assert_eq!(
            fs::read(unrelated.join("keep")).unwrap(),
            b"unrelated residue"
        );
    }
    let receipt = staged_replacement(&fixture.root).commit(|_| true).unwrap();
    assert_generation(
        &fixture.root,
        &receipt.generation_id,
        "candidate",
        "previous",
    );

    // The pointer is already visible when its parent-directory sync fails.
    // Both cold creation and incompatible rebuild must retain that generation.
    for cold in [true, false] {
        let fixture = RecoveryFixture::new();
        if cold {
            fs::remove_dir_all(&fixture.root).unwrap();
        } else {
            make_incompatible(&fixture.root);
        }
        let output = fixture.run_fault_child(
            &shim,
            "commit_expect_error",
            FaultCase::fail("sync", "root_dir", "EIO", Some("pointer_rename")),
        );
        assert!(output.status.success(), "{output:?}");
        assert!(
            fixture.marker.is_file(),
            "post-rename sync fault was not reached"
        );
        let error = fs::read_to_string(&fixture.result).unwrap();
        assert!(
            error.contains("CommittedGenerationNeedsRecovery"),
            "{error}"
        );
        let index = VerifiedIndex::open_pinned(&fixture.root).unwrap();
        assert_reader_terms(&index, "candidate", "previous");
        assert_eq!(index.manifest().indexed_documents, 1);
    }
}
