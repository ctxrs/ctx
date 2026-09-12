use super::*;

fn publish(store: &FlatSegmentStore, sequence: u64) -> FlatResult<FlatPublishOutcome> {
    store.publish_replacement_event_chunks(
        &[replacement(
            Uuid::from_u128(901),
            sequence,
            1,
            vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
        )],
        &[],
    )
}

fn selected(root: &Path) -> FlatResult<SelectedManifest> {
    select_manifest(root, &contract())?
        .ok_or_else(|| FlatStoreError::Corrupt("expected test manifest".to_owned()))
}

fn write_inline_manifest(root: &Path, selected: &SelectedManifest) -> FlatResult<PathBuf> {
    let mut inline = selected.envelope.clone();
    inline.manifest.schema_version = INLINE_MANIFEST_SCHEMA_VERSION;
    inline.manifest.catalog_pages.clear();
    inline.manifest_sha256 =
        encode_hex(Sha256::digest(serde_json::to_vec(&inline.manifest)?).as_slice());
    let path = manifests_directory(root).join(manifest_name(
        inline.manifest.generation,
        &inline.manifest_sha256,
    ));
    fs::write(&path, serde_json::to_vec(&inline)?)
        .map_err(|source| io_error("write schema 4 fixture", &path, source))?;
    fs::remove_file(&selected.path)
        .map_err(|source| io_error("replace test root manifest", &selected.path, source))?;
    Ok(path)
}

#[test]
fn inline_schema_four_upgrades_without_rewriting_vectors() -> FlatResult<()> {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let store = FlatSegmentStore::open(root, contract())?;
    publish(&store, 1)?;
    let first = selected(root)?;
    let old_segments = first.envelope.manifest.segments.clone();
    let inline_path = write_inline_manifest(root, &first)?;
    drop(store);
    let legacy = selected(root)?;
    assert_eq!(legacy.envelope.manifest.schema_version, 4);
    assert_eq!(legacy.path, inline_path);
    let store = FlatSegmentStore::open(root, contract())?;
    let old_pin = store.pin_generation()?.unwrap();
    assert!(!store.model_contract_reset_pending()?);
    publish(&store, 2)?;
    let upgraded = selected(root)?;
    assert_eq!(upgraded.envelope.manifest.schema_version, 5);
    assert!(!upgraded.envelope.manifest.catalog_pages.is_empty());
    for descriptor in old_segments {
        assert!(upgraded.envelope.manifest.segments.contains(&descriptor));
    }
    assert_eq!(visible_chunks(&old_pin)[0].1, 1);
    assert_eq!(visible_chunks(&store.pin_generation()?.unwrap())[0].1, 2);
    Ok(())
}

#[test]
fn catalog_publication_survives_process_exit() -> FlatResult<()> {
    if let Some(root) = std::env::var_os("CTX_FLAT_CRASH_ROOT") {
        let point = std::env::var("CTX_FLAT_CRASH_POINT")
            .unwrap()
            .parse::<u8>()
            .unwrap();
        let store = FlatSegmentStore::open(Path::new(&root), contract())?;
        super::super::catalog_pages::TEST_PUBLICATION_CRASH.with(|value| value.set(point));
        publish(&store, 2)?;
        panic!("publication did not exit at the requested durable boundary");
    }
    for point in [1, 2] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let store = FlatSegmentStore::open(root, contract())?;
        publish(&store, 1)?;
        let retained = store.pin_generation()?.unwrap();
        drop(store);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .env("HOME", root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_RUNTIME_DIR", root)
            .env("TMPDIR", root)
            .env("CTX_DATA_ROOT", root)
            .env("CTX_ANALYTICS_ENABLED", "false")
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .env("CTX_FLAT_CRASH_ROOT", root)
            .env("CTX_FLAT_CRASH_POINT", point.to_string())
            .arg("--exact")
            .arg("vector_store::flat_segments::tests::catalog_pages::catalog_publication_survives_process_exit")
            .status().unwrap();
        assert_eq!(status.code(), Some(73));
        let recovered = FlatSegmentStore::open(root, contract())?;
        let active = recovered.pin_generation()?.unwrap();
        assert_eq!(visible_chunks(&active)[0].1, if point == 1 { 1 } else { 2 });
        assert_eq!(visible_chunks(&retained)[0].1, 1);
        publish(&recovered, 3)?;
        assert_eq!(
            visible_chunks(&recovered.pin_generation()?.unwrap())[0].1,
            3
        );
        assert_eq!(visible_chunks(&retained)[0].1, 1);
    }
    Ok(())
}

#[test]
fn catalog_pages_reject_missing_corrupt_and_oversized_bytes() -> FlatResult<()> {
    for damage in ["missing", "checksum", "oversized", "duplicate", "routing"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let store = FlatSegmentStore::open(root, contract())?;
        publish(&store, 1)?;
        let current = selected(root)?;
        let page_path =
            segments_directory(root).join(current.envelope.manifest.catalog_pages[0].file_name());
        match damage {
            "missing" => fs::remove_file(&page_path).unwrap(),
            "checksum" => {
                let mut bytes = fs::read(&page_path).unwrap();
                bytes[0] ^= 1;
                fs::write(&page_path, bytes).unwrap();
            }
            "oversized" => File::options()
                .write(true)
                .open(&page_path)
                .unwrap()
                .set_len(MAX_MANIFEST_BYTES + 1)
                .unwrap(),
            "duplicate" | "routing" => {
                let mut root_json: serde_json::Value =
                    serde_json::from_slice(&fs::read(&current.path).unwrap())?;
                if damage == "duplicate" {
                    let refs = root_json["manifest"]["catalog_pages"]
                        .as_array_mut()
                        .unwrap();
                    refs.push(refs[0].clone());
                } else {
                    let bucket = root_json["manifest"]["catalog_pages"][0]["bucket"]
                        .as_u64()
                        .unwrap();
                    root_json["manifest"]["catalog_pages"][0]["bucket"] =
                        serde_json::json!((bucket + 1) % 256);
                }
                let stored: Manifest = serde_json::from_value(root_json["manifest"].clone())?;
                let digest = encode_hex(Sha256::digest(serde_json::to_vec(&stored)?).as_slice());
                root_json["manifest_sha256"] = serde_json::json!(digest);
                let path =
                    manifests_directory(root).join(manifest_name(stored.generation, &digest));
                fs::write(&path, serde_json::to_vec(&root_json)?).unwrap();
                fs::remove_file(&current.path).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            select_manifest(root, &contract()).is_err(),
            "{damage} page was accepted"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn catalog_page_symlink_is_rejected() -> FlatResult<()> {
    let temp = tempfile::tempdir().unwrap();
    let store = FlatSegmentStore::open(temp.path(), contract())?;
    publish(&store, 1)?;
    let current = selected(temp.path())?;
    let page = segments_directory(temp.path())
        .join(current.envelope.manifest.catalog_pages[0].file_name());
    let target = temp.path().join("untrusted.json");
    fs::rename(&page, &target).unwrap();
    std::os::unix::fs::symlink(&target, &page).unwrap();
    assert!(matches!(
        select_manifest(temp.path(), &contract()),
        Err(FlatStoreError::Corrupt(_))
    ));
    Ok(())
}

#[test]
fn oversized_catalog_candidate_preserves_published_authority() -> FlatResult<()> {
    let temp = tempfile::tempdir().unwrap();
    let store = FlatSegmentStore::open(temp.path(), contract())?;
    publish(&store, 1)?;
    let before = selected(temp.path())?;
    FlatSegmentStore::with_test_manifest_byte_limit(1024, || {
        assert!(matches!(
            publish(&store, 2),
            Err(FlatStoreError::InvalidInput(_))
        ));
    });
    assert_eq!(
        selected(temp.path())?.generation_hash,
        before.generation_hash
    );
    drop(store);
    let recovered = FlatSegmentStore::open(temp.path(), contract())?;
    assert_eq!(
        visible_chunks(&recovered.pin_generation()?.unwrap())[0].1,
        1
    );
    publish(&recovered, 2)?;
    assert_eq!(
        visible_chunks(&recovered.pin_generation()?.unwrap())[0].1,
        2
    );
    Ok(())
}
