use super::*;

// Authored filesystem and malformed-stream cases, not native producer captures.
enum Refusal {
    CorruptCompressed,
    CorruptRenamedCompressed,
    OversizedCompressed,
    #[cfg(unix)]
    Symlink,
    #[cfg(unix)]
    PermissionDenied,
}

fn exercise_leaf_failure(refusal: Refusal, warm: bool) {
    let compressed = matches!(
        refusal,
        Refusal::CorruptCompressed
            | Refusal::CorruptRenamedCompressed
            | Refusal::OversizedCompressed
    );
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let failed_id = "019fb000-0000-7000-8000-000000000081";
    let peer_id = "019fb000-0000-7000-8000-000000000082";
    let path = if matches!(refusal, Refusal::CorruptRenamedCompressed) {
        sessions.join("renamed.jsonl.zst")
    } else if compressed {
        sessions.join(format!("rollout-{failed_id}.jsonl.zst"))
    } else {
        session_path(&sessions, failed_id)
    };
    let original = jsonl_bytes([
        session_meta(failed_id, ProviderNativeSessionRelationship::Root, None),
        message("leaforiginalmarker"),
    ]);
    let write_valid = || {
        let bytes = if compressed {
            zstd::stream::encode_all(original.as_slice(), 1).unwrap()
        } else {
            original.clone()
        };
        fs::write(&path, bytes).unwrap();
    };
    write_valid();
    write_session(
        &sessions,
        peer_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("leafpeermarker")],
    );
    let registry = register_tree(&[&sessions]);
    if warm {
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    }
    match refusal {
        Refusal::CorruptCompressed | Refusal::CorruptRenamedCompressed => {
            fs::write(&path, b"invalid compressed stream").unwrap()
        }
        Refusal::OversizedCompressed => write_oversized_raw_zstd(&path),
        #[cfg(unix)]
        Refusal::Symlink => {
            let target = temp.path().join("refused-target.jsonl");
            fs::write(&target, &original).unwrap();
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
        }
        #[cfg(unix)]
        Refusal::PermissionDenied => {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            if fs::File::open(&path).is_ok() {
                return; // Privileged users bypass Unix mode-bit denial.
            }
        }
    }
    for ordinal in 0..2 {
        let marker = format!("leafpeeradvance{ordinal}");
        append_event(&session_path(&sessions, peer_id), message(&marker));
        let receipt = refresh_source_backed_generation(&index_root, &registry, writer_options())
            .expect("one refused leaf must not abort the route");
        assert!(receipt.failed_routes.is_empty());
        assert_eq!(receipt.logical_source_failures.total(), 1);
        let failure = &receipt.logical_source_failures.failures()[0];
        assert_eq!(
            failure.source_selector.as_deref(),
            Some(path.to_str().unwrap())
        );
        assert_eq!(failure.carried_forward, warm);
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert!(source_records_contain(&index, peer_id, &marker));
        assert_eq!(
            !search_event_candidates(&index, "leaforiginalmarker", 8).is_empty(),
            warm
        );
        if matches!(refusal, Refusal::OversizedCompressed) {
            assert!(
                failure.detail.contains("per-leaf temporary-volume limit"),
                "{}",
                failure.detail
            );
            assert!(failure.detail.contains("compressed_bytes="));
            assert!(failure.detail.contains("decoded_bytes_at_least="));
            assert!(failure.detail.contains("limit_bytes=268435456"));
        }
    }
    fs::remove_file(&path).unwrap();
    write_valid();
    let repaired =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(source_records_contain(
        &index,
        failed_id,
        "leaforiginalmarker"
    ));
}

#[test]
fn corrupt_compressed_leaf_isolated_cold_and_warm() {
    for warm in [false, true] {
        exercise_leaf_failure(Refusal::CorruptCompressed, warm);
    }
}

#[cfg(unix)]
#[test]
fn refused_symlink_leaf_isolated_cold_and_warm() {
    for warm in [false, true] {
        exercise_leaf_failure(Refusal::Symlink, warm);
    }
}

// A standard Zstandard frame with raw blocks and a 64 KiB window, deliberately
// larger than half the scratch cap. Compressed and decoded lengths each fit
// their individual caps; their sum must trip the production per-leaf cap.
fn write_oversized_raw_zstd(path: &Path) {
    const BLOCK: usize = 64 * 1024;
    const BLOCKS: usize = 129 * 1024 * 1024 / BLOCK;
    let mut file = fs::File::create(path).unwrap();
    file.write_all(&[0x28, 0xb5, 0x2f, 0xfd, 0x80, 0x30])
        .unwrap();
    file.write_all(&((BLOCK * BLOCKS) as u32).to_le_bytes())
        .unwrap();
    let block = [b' '; BLOCK];
    for ordinal in 0..BLOCKS {
        let header = ((BLOCK as u32) << 3) | u32::from(ordinal + 1 == BLOCKS);
        file.write_all(&header.to_le_bytes()[..3]).unwrap();
        file.write_all(&block).unwrap();
    }
}

#[test]
fn oversized_compressed_leaf_isolated_cold_and_warm() {
    for warm in [false, true] {
        exercise_leaf_failure(Refusal::OversizedCompressed, warm);
    }
}

#[cfg(unix)]
#[test]
fn unreadable_leaf_isolated_cold_and_warm() {
    for warm in [false, true] {
        exercise_leaf_failure(Refusal::PermissionDenied, warm);
    }
}

#[test]
fn unreadable_renamed_compressed_leaf_preserves_prior_path_ownership() {
    for warm in [false, true] {
        exercise_leaf_failure(Refusal::CorruptRenamedCompressed, warm);
    }
}
