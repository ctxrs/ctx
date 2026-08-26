use std::io::Cursor;

use super::*;

fn compressed_session_path(root: &Path, native_session_id: &str) -> PathBuf {
    root.join(format!("rollout-{native_session_id}.jsonl.zst"))
}

fn session_bytes(native_session_id: &str, marker: &str) -> Vec<u8> {
    jsonl_bytes([
        session_meta(
            native_session_id,
            ProviderNativeSessionRelationship::Root,
            None,
        ),
        message(marker),
    ])
}

fn write_compressed(path: &Path, plaintext: &[u8]) {
    let compressed = zstd::stream::encode_all(Cursor::new(plaintext), 1).unwrap();
    fs::write(path, compressed).unwrap();
}

fn write_compressed_frames(path: &Path, plaintext_frames: &[Vec<u8>]) {
    let mut compressed = Vec::new();
    for plaintext in plaintext_frames {
        compressed.extend(zstd::stream::encode_all(Cursor::new(plaintext), 1).unwrap());
    }
    fs::write(path, compressed).unwrap();
}

fn logical_identity(records: &[CoreRecord]) -> Vec<(String, String, String)> {
    records
        .iter()
        .map(|record| {
            (
                record.session_id.to_string(),
                record.event_id.to_string(),
                serde_json::to_string(&record.native_event_id).unwrap(),
            )
        })
        .collect()
}

fn revert_session_path(
    root: &Path,
    native_session_id: &str,
    rollout_id: &str,
    compressed: bool,
) -> PathBuf {
    root.join(format!(
        "rollout-2026-08-19T12-00-00-{native_session_id}_{rollout_id}.jsonl{}",
        if compressed { ".zst" } else { "" }
    ))
}

#[test]
fn revert_rollouts_use_embedded_owner_for_raw_and_compressed_tree_and_explicit_routes() {
    let temp = tempdir().unwrap();
    let tree_root = temp.path().join("revert-tree");
    let tree_index = temp.path().join("revert-tree-index");
    let explicit_root = temp.path().join("revert-explicit");
    let explicit_index = temp.path().join("revert-explicit-index");
    fs::create_dir_all(&tree_root).unwrap();
    fs::create_dir_all(&explicit_root).unwrap();

    let cases = [
        (
            "019fb000-0000-7000-8000-000000000076",
            "019fb000-0000-7000-8000-000000000077",
            false,
            "rawtreerevertowner",
        ),
        (
            "019fb000-0000-7000-8000-000000000078",
            "019fb000-0000-7000-8000-000000000079",
            true,
            "compressedtreerevertowner",
        ),
    ];
    for (native_session_id, rollout_id, compressed, marker) in cases {
        let path = revert_session_path(&tree_root, native_session_id, rollout_id, compressed);
        let bytes = session_bytes(native_session_id, marker);
        if compressed {
            write_compressed(&path, &bytes);
        } else {
            fs::write(path, bytes).unwrap();
        }
    }
    let registry = register_tree(&[&tree_root]);
    let receipt =
        refresh_source_backed_generation(&tree_index, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    assert_eq!(receipt.sources.len(), 2);
    let index = VerifiedIndex::open(&tree_index).unwrap();
    for (native_session_id, _, _, marker) in cases {
        assert_eq!(records_for(&index, native_session_id).len(), 1);
        assert_eq!(search_event_candidates(&index, marker, 8).len(), 1);
    }

    let explicit_cases = [
        (
            "019fb000-0000-7000-8000-00000000007a",
            "019fb000-0000-7000-8000-00000000007b",
            false,
            "rawexplicitrevertowner",
        ),
        (
            "019fb000-0000-7000-8000-00000000007c",
            "019fb000-0000-7000-8000-00000000007d",
            true,
            "compressedexplicitrevertowner",
        ),
    ];
    let mut registry = SourceBackedProviderRegistry::new();
    for (native_session_id, rollout_id, compressed, marker) in explicit_cases {
        let path = revert_session_path(&explicit_root, native_session_id, rollout_id, compressed);
        let bytes = session_bytes(native_session_id, marker);
        if compressed {
            write_compressed(&path, &bytes);
        } else {
            fs::write(&path, bytes).unwrap();
        }
        add_explicit_route(&mut registry, &path);
    }
    let receipt =
        refresh_source_backed_generation(&explicit_index, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    assert_eq!(receipt.sources.len(), 2);
    let index = VerifiedIndex::open(&explicit_index).unwrap();
    for (native_session_id, _, _, marker) in explicit_cases {
        assert_eq!(records_for(&index, native_session_id).len(), 1);
        assert_eq!(search_event_candidates(&index, marker, 8).len(), 1);
    }
}

#[test]
fn exact_compressed_and_raw_rollouts_have_identical_logical_identity() {
    let temp = tempdir().unwrap();
    let raw_root = temp.path().join("raw");
    let compressed_root = temp.path().join("compressed");
    let raw_index = temp.path().join("raw-index");
    let compressed_index = temp.path().join("compressed-index");
    fs::create_dir_all(&raw_root).unwrap();
    fs::create_dir_all(&compressed_root).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000061";
    let marker = "compressedrepresentationidentitymarker";
    let plaintext = session_bytes(native_session_id, marker);
    let raw_path = session_path(&raw_root, native_session_id);
    let compressed_path = compressed_session_path(&compressed_root, native_session_id);
    fs::write(&raw_path, &plaintext).unwrap();
    write_compressed(&compressed_path, &plaintext);

    let mut raw_registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut raw_registry, &raw_path);
    let raw_receipt =
        refresh_source_backed_generation(&raw_index, &raw_registry, writer_options()).unwrap();
    assert!(raw_receipt.failed_routes.is_empty());

    let mut compressed_registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut compressed_registry, &compressed_path);
    let compressed_receipt =
        refresh_source_backed_generation(&compressed_index, &compressed_registry, writer_options())
            .unwrap();
    assert!(compressed_receipt.failed_routes.is_empty());

    let raw = VerifiedIndex::open(&raw_index).unwrap();
    let compressed = VerifiedIndex::open(&compressed_index).unwrap();
    let raw_records = records_for(&raw, native_session_id);
    let compressed_records = records_for(&compressed, native_session_id);
    assert_eq!(raw_records.len(), 1);
    assert_eq!(compressed_records.len(), 1);
    assert_eq!(
        logical_identity(&raw_records),
        logical_identity(&compressed_records)
    );
    assert!(raw_records[0]
        .source
        .exact_descriptor_eq(&compressed_records[0].source));
    assert_eq!(
        search_event_candidates(&raw, marker, 8)[0].event.event_id,
        search_event_candidates(&compressed, marker, 8)[0]
            .event
            .event_id
    );
}

#[test]
fn exact_noncanonical_compressed_rollout_uses_embedded_session_identity() {
    let temp = tempdir().unwrap();
    let source_root = temp.path().join("renamed");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&source_root).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000067";
    let marker = "noncanonicalcompressedidentitymarker";
    let source = source_root.join("copied-rollout.jsonl.zst");
    write_compressed(&source, &session_bytes(native_session_id, marker));

    let mut registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut registry, &source);
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    assert_eq!(receipt.sources.len(), 1);
    let index = VerifiedIndex::open(&index_root).unwrap();
    let records = records_for(&index, native_session_id);
    assert_eq!(records.len(), 1);
    assert_eq!(search_event_candidates(&index, marker, 8).len(), 1);
    assert_eq!(
        records[0].provider_session_id.as_deref(),
        Some(native_session_id)
    );
}

#[test]
fn mixed_raw_and_compressed_tree_imports_each_native_session() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let raw_id = "019fb000-0000-7000-8000-000000000062";
    let compressed_id = "019fb000-0000-7000-8000-000000000063";
    fs::write(
        session_path(&sessions, raw_id),
        session_bytes(raw_id, "mixedrawmarker"),
    )
    .unwrap();
    write_compressed(
        &compressed_session_path(&sessions, compressed_id),
        &session_bytes(compressed_id, "mixedcompressedmarker"),
    );

    let registry = register_tree(&[&sessions]);
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    assert_eq!(receipt.sources.len(), 2);
    let index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(records_for(&index, raw_id).len(), 1);
    assert_eq!(records_for(&index, compressed_id).len(), 1);
    assert_eq!(
        search_event_candidates(&index, "mixedcompressedmarker", 8).len(),
        1
    );
}

#[test]
fn raw_to_compressed_representation_transition_replaces_physical_state_only() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000064";
    let marker = "representationtransitionmarker";
    let plaintext = session_bytes(native_session_id, marker);
    let raw_path = session_path(&sessions, native_session_id);
    let compressed_path = compressed_session_path(&sessions, native_session_id);
    fs::write(&raw_path, &plaintext).unwrap();
    let registry = register_tree(&[&sessions]);

    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let before = VerifiedIndex::open(&index_root).unwrap();
    let before_records = records_for(&before, native_session_id);
    let before_identity = logical_identity(&before_records);
    drop(before);

    write_compressed(&compressed_path, &plaintext);
    fs::remove_file(&raw_path).unwrap();
    let transitioned =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(transitioned.failed_routes.is_empty());
    assert!(transitioned.logical_source_failures.is_empty());
    assert_eq!(transitioned.sources.len(), 1);

    let after = VerifiedIndex::open(&index_root).unwrap();
    let after_records = records_for(&after, native_session_id);
    assert_eq!(logical_identity(&after_records), before_identity);
    assert_eq!(after_records.len(), 1);
    assert_eq!(search_event_candidates(&after, marker, 8).len(), 1);
    assert_eq!(
        certificate_for(&after, native_session_id)
            .frontier()
            .unwrap()
            .certified_prefix_bytes(),
        fs::metadata(&compressed_path).unwrap().len()
    );
}

#[test]
fn overlapping_raw_and_compressed_representations_coalesce_raw_first_and_keep_ids() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000068";
    let marker = "overlappingrepresentationmarker";
    let plaintext = session_bytes(native_session_id, marker);
    let raw_path = session_path(&sessions, native_session_id);
    let compressed_path = compressed_session_path(&sessions, native_session_id);
    fs::write(&raw_path, &plaintext).unwrap();
    write_compressed(&compressed_path, &plaintext);
    let registry = register_tree(&[&sessions]);

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert!(cold.logical_source_failures.is_empty());
    assert_eq!(cold.sources.len(), 1);
    let cold_index = VerifiedIndex::open(&index_root).unwrap();
    let identity = logical_identity(&records_for(&cold_index, native_session_id));
    assert_eq!(
        certificate_for(&cold_index, native_session_id)
            .frontier()
            .unwrap()
            .certified_prefix_bytes(),
        plaintext.len() as u64,
        "raw JSONL must deterministically win while both representations exist"
    );
    drop(cold_index);

    fs::remove_file(&raw_path).unwrap();
    let compressed_only =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(compressed_only.failed_routes.is_empty());
    assert_eq!(compressed_only.sources.len(), 1);
    let compressed_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        logical_identity(&records_for(&compressed_index, native_session_id)),
        identity
    );
    assert_eq!(
        certificate_for(&compressed_index, native_session_id)
            .frontier()
            .unwrap()
            .certified_prefix_bytes(),
        fs::metadata(&compressed_path).unwrap().len()
    );
    drop(compressed_index);

    fs::write(&raw_path, &plaintext).unwrap();
    let overlapping_again =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(overlapping_again.failed_routes.is_empty());
    assert_eq!(overlapping_again.sources.len(), 1);
    let overlapping_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        logical_identity(&records_for(&overlapping_index, native_session_id)),
        identity
    );
    assert_eq!(
        certificate_for(&overlapping_index, native_session_id)
            .frontier()
            .unwrap()
            .certified_prefix_bytes(),
        plaintext.len() as u64
    );
    drop(overlapping_index);

    fs::remove_file(&compressed_path).unwrap();
    let raw_only =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(raw_only.failed_routes.is_empty());
    let raw_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        logical_identity(&records_for(&raw_index, native_session_id)),
        identity
    );
    assert_eq!(search_event_candidates(&raw_index, marker, 8).len(), 1);
}

#[test]
fn exact_compressed_route_rejects_conflicting_concatenated_frame_owners() {
    let temp = tempdir().unwrap();
    let source_root = temp.path().join("explicit");
    fs::create_dir_all(&source_root).unwrap();
    let first_owner = "019fb000-0000-7000-8000-000000000074";
    let second_owner = "019fb000-0000-7000-8000-000000000075";
    let source = source_root.join("ambiguous-rollout.jsonl.zst");
    write_compressed_frames(
        &source,
        &[
            jsonl_bytes([session_meta(
                first_owner,
                ProviderNativeSessionRelationship::Root,
                None,
            )]),
            jsonl_bytes([
                session_meta(second_owner, ProviderNativeSessionRelationship::Root, None),
                message("explicitambiguousownermarker"),
            ]),
        ],
    );

    let mut registry = SourceBackedProviderRegistry::new();
    let error = register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            &source,
        ),
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::InvalidRoute {
            provider: CaptureProvider::Codex,
            detail,
        } if detail.contains("conflicting session_meta owners")
    ));
    assert!(registry.routes().next().is_none());
}

#[test]
fn automatic_compressed_route_fails_without_a_usable_frame_owner() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let admitted_owner = "019fb000-0000-7000-8000-000000000076";
    let conflicting_owner = "019fb000-0000-7000-8000-000000000077";
    let source = compressed_session_path(&sessions, admitted_owner);
    let mut bounded_catalog_prefix = vec![session_meta(
        admitted_owner,
        ProviderNativeSessionRelationship::Root,
        None,
    )];
    bounded_catalog_prefix.extend(
        (0..31).map(|sequence| serde_json::json!({"type": "world_state", "sequence": sequence})),
    );
    write_compressed_frames(
        &source,
        &[
            jsonl_bytes(bounded_catalog_prefix),
            jsonl_bytes([
                session_meta(
                    conflicting_owner,
                    ProviderNativeSessionRelationship::Root,
                    None,
                ),
                message("automaticambiguousownermarker"),
            ]),
        ],
    );

    let registry = register_tree(&[&sessions]);
    let error = refresh_source_backed_generation(&index_root, &registry, writer_options())
        .expect_err("ambiguous ownership leaves no usable logical source");
    let SourceBackedCoordinatorError::NoUsableLogicalSources { failed_sources } = error else {
        panic!("unexpected compressed ownership error: {error:?}");
    };
    assert_eq!(failed_sources.total(), 1);
    assert!(VerifiedIndex::open(&index_root).is_err());
}

#[test]
fn repeated_consistent_compressed_metadata_across_frames_remains_admissible() {
    let temp = tempdir().unwrap();
    let source_root = temp.path().join("explicit");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&source_root).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000078";
    let marker = "consistentcompressedownermarker";
    let source = source_root.join("repeated-rollout.jsonl.zst");
    let metadata = session_meta(
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
    );
    write_compressed_frames(
        &source,
        &[
            jsonl_bytes([metadata.clone()]),
            jsonl_bytes([metadata, message(marker)]),
        ],
    );

    let mut registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut registry, &source);
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    let index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(records_for(&index, native_session_id).len(), 1);
    assert_eq!(search_event_candidates(&index, marker, 8).len(), 1);
}

#[test]
fn compressed_same_inode_overwrite_after_snapshot_fails_terminal_fence() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000069";
    let path = compressed_session_path(&sessions, native_session_id);
    write_compressed(
        &path,
        &session_bytes(native_session_id, "snapshotterminalfencemarker"),
    );
    let mutate = path.clone();
    set_after_standard_zstd_snapshot_hook(move || {
        let mut bytes = fs::read(&mutate).unwrap();
        bytes[0] ^= 0xff;
        OpenOptions::new()
            .write(true)
            .open(&mutate)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
    });
    let registry = register_tree(&[&sessions]);
    match refresh_source_backed_generation(&index_root, &registry, writer_options()) {
        Ok(receipt) => {
            assert_eq!(receipt.failed_routes.len(), 1);
            assert!(receipt.sources.is_empty());
        }
        Err(SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes }) => {
            assert_eq!(failed_routes.len(), 1);
        }
        Err(SourceBackedCoordinatorError::RouteScan { source, .. }) => {
            assert_eq!(source.kind, SourceBackedRouteErrorKind::InvalidSource);
        }
        Err(error) => panic!("unexpected compressed overwrite failure: {error:?}"),
    }
}

fn assert_compressed_registry_rejected(registry: &SourceBackedProviderRegistry, index_root: &Path) {
    match refresh_source_backed_generation(index_root, registry, writer_options()) {
        Ok(receipt) => {
            assert_eq!(receipt.failed_routes.len(), 1);
            assert!(receipt.sources.is_empty());
        }
        Err(SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes }) => {
            assert_eq!(failed_routes.len(), 1);
        }
        Err(SourceBackedCoordinatorError::RouteScan { source, .. }) => {
            assert_eq!(source.kind, SourceBackedRouteErrorKind::InvalidSource);
        }
        Err(error) => panic!("unexpected compressed-source failure: {error:?}"),
    }
}

fn assert_compressed_source_rejected(sessions: &Path, index_root: &Path) {
    let registry = register_tree(&[sessions]);
    assert_compressed_registry_rejected(&registry, index_root);
}

#[test]
fn corrupt_and_oversize_compressed_rollouts_fail_the_capture_lifecycle() {
    let temp = tempdir().unwrap();
    let corrupt_sessions = temp.path().join("corrupt-sessions");
    fs::create_dir_all(&corrupt_sessions).unwrap();
    let corrupt_id = "019fb000-0000-7000-8000-000000000065";
    let corrupt_path = compressed_session_path(&corrupt_sessions, corrupt_id);
    let mut corrupt = zstd::stream::encode_all(
        Cursor::new(session_bytes(corrupt_id, "corruptcompressedmarker")),
        1,
    )
    .unwrap();
    corrupt[0] ^= 0xff;
    fs::write(corrupt_path, corrupt).unwrap();
    assert_compressed_source_rejected(&corrupt_sessions, &temp.path().join("corrupt-index"));

    let oversize_sessions = temp.path().join("oversize-sessions");
    fs::create_dir_all(&oversize_sessions).unwrap();
    let oversize_id = "019fb000-0000-7000-8000-000000000066";
    let oversize_path = compressed_session_path(&oversize_sessions, oversize_id);
    let bomb_like_plaintext = vec![b' '; 17 * 1024 * 1024];
    write_compressed(&oversize_path, &bomb_like_plaintext);
    assert_compressed_source_rejected(&oversize_sessions, &temp.path().join("oversize-index"));
}
