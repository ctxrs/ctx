#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_sources::explicit_path_source;
    use ctx_history_capture::provider_source_specs;
    use ctx_history_core::AgentType;

    #[test]
    fn every_importable_provider_uses_incremental_event_search() {
        for spec in provider_source_specs() {
            let source = explicit_path_source(
                spec.provider,
                PathBuf::from(format!("{}-history", spec.provider.as_str())),
            );

            assert_eq!(source.import_support, spec.import_support);
            assert!(
                source_uses_incremental_event_search(&source),
                "{} import must maintain event search incrementally",
                spec.provider
            );
        }
    }

    #[test]
    fn unsupported_source_does_not_claim_incremental_event_search() {
        let source = explicit_path_source(CaptureProvider::Shell, PathBuf::from("shell-history"));

        assert!(!source.import_support.is_importable());
        assert!(!source_uses_incremental_event_search(&source));
    }

    #[test]
    fn codex_result_rejects_a_generation_superseded_after_normalization() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let source_path = temp.path().join("session.jsonl");
        fs::write(&source_path, b"{}\n").unwrap();
        let source_root = temp.path().join("sessions").display().to_string();
        let source_path = source_path.display().to_string();
        let session = CatalogSession {
            provider: CaptureProvider::Codex,
            source_format: "codex_session_jsonl".to_owned(),
            source_root: source_root.clone(),
            source_path,
            external_session_id: Some("superseded-result".to_owned()),
            parent_external_session_id: None,
            agent_type: AgentType::Primary,
            role_hint: None,
            external_agent_id: None,
            cwd: None,
            session_started_at_ms: Some(1),
            file_size_bytes: 3,
            file_modified_at_ms: 1,
            import_revision: 1,
            cataloged_at_ms: 1,
            metadata: serde_json::json!({
                "file_observation_token_v1": "superseded-result-token"
            }),
        };
        let store = Store::open(db_path).unwrap();
        let superseded = store
            .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)
            .unwrap();
        store
            .upsert_catalog_sessions(superseded, std::slice::from_ref(&session))
            .unwrap();
        store
            .complete_catalog_inventory_generation(CaptureProvider::Codex, &source_root, superseded)
            .unwrap();
        store
            .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)
            .unwrap();

        let error = mark_catalog_session_result(
            &store,
            &session,
            Some(1),
            2,
            CatalogIndexedStatus::Indexed,
            None,
            superseded,
        )
        .unwrap_err();

        assert!(error.chain().any(|cause| matches!(
            cause.downcast_ref::<CaptureError>(),
            Some(CaptureError::InventorySuperseded)
        )));
    }

    #[test]
    fn sqlite_source_stats_observe_durable_sidecars_but_ignore_shm() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("state.db");
        fs::write(&db, b"main").unwrap();
        let initial = source_stats(&db).unwrap().change_token.unwrap();

        fs::write(sqlite_sidecar(&db, "-shm"), b"volatile coordination state").unwrap();
        assert_eq!(source_stats(&db).unwrap().change_token.unwrap(), initial);

        fs::write(sqlite_sidecar(&db, "-wal"), b"committed wal frame").unwrap();
        assert_ne!(source_stats(&db).unwrap().change_token.unwrap(), initial);

        let root = temp.path().join("project");
        fs::create_dir(&root).unwrap();
        let nested_db = root.join("session.db");
        fs::write(&nested_db, b"main").unwrap();
        let root_initial = source_stats(&root).unwrap().change_token.unwrap();
        fs::write(sqlite_sidecar(&nested_db, "-shm"), b"volatile").unwrap();
        assert_eq!(
            source_stats(&root).unwrap().change_token.unwrap(),
            root_initial
        );
        fs::write(sqlite_sidecar(&nested_db, "-journal"), b"committed journal").unwrap();
        assert_ne!(
            source_stats(&root).unwrap().change_token.unwrap(),
            root_initial
        );
    }

    #[test]
    fn sqlite_source_stats_ignore_orphan_sidecar_contents_and_churn() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("session.jsonl");
        let sidecar = temp.path().join("orphan.db-wal");
        fs::write(&source, b"stable\n").unwrap();
        let initial = source_stats(temp.path()).unwrap();

        fs::write(&sidecar, b"first generation").unwrap();
        let present = source_stats(temp.path()).unwrap();
        assert_eq!(present.change_token, initial.change_token);
        assert_eq!(present.files, initial.files + 1);
        assert_eq!(present.bytes, initial.bytes + 16);

        fs::remove_file(&sidecar).unwrap();
        let disappeared = source_stats(temp.path()).unwrap();
        assert_eq!(disappeared.change_token, initial.change_token);
        assert_eq!(disappeared.files, initial.files);
        assert_eq!(disappeared.bytes, initial.bytes);
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_source_stats_do_not_reopen_orphan_sidecars() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let sidecar = temp.path().join("orphan.db-journal");
        fs::write(&sidecar, b"transient journal").unwrap();
        fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o000)).unwrap();

        let stats = source_stats(temp.path()).unwrap();

        assert_eq!(stats.files, 1);
        assert_eq!(stats.bytes, 17);
        assert_eq!(stats.change_token, Some(source_change_token(Vec::new())));
    }

    #[test]
    fn sqlite_source_stats_detect_same_stat_wal_generation_and_disappearance() {
        use std::fs::FileTimes;

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("state.db");
        let first_fixture = real_wal_generation(temp.path(), "first", "omega");
        let second_fixture = real_wal_generation(temp.path(), "second", "sigma");
        assert_eq!(first_fixture.0, second_fixture.0);
        assert_eq!(first_fixture.1.len(), second_fixture.1.len());
        fs::write(&db, first_fixture.0).unwrap();
        let wal = sqlite_sidecar(&db, "-wal");
        fs::write(&wal, first_fixture.1).unwrap();
        let original_metadata = fs::metadata(&wal).unwrap();
        let original_modified = original_metadata.modified().unwrap();
        let first = source_stats(&db).unwrap().change_token.unwrap();

        fs::write(&wal, second_fixture.1).unwrap();
        fs::File::options()
            .write(true)
            .open(&wal)
            .unwrap()
            .set_times(FileTimes::new().set_modified(original_modified))
            .unwrap();
        let replacement_metadata = fs::metadata(&wal).unwrap();
        assert_eq!(replacement_metadata.len(), original_metadata.len());
        assert_eq!(replacement_metadata.modified().unwrap(), original_modified);
        let replaced = source_stats(&db).unwrap().change_token.unwrap();
        assert_ne!(replaced, first);

        fs::remove_file(&wal).unwrap();
        let disappeared = source_stats(&db).unwrap().change_token.unwrap();
        assert_ne!(disappeared, replaced);
    }

    #[test]
    fn source_change_tokens_include_the_ordinary_file_observation() {
        let path = PathBuf::from("session.jsonl");
        let entry = SourceChangeEntry {
            path: path.clone(),
            len: 42,
            modified_secs: 123,
            modified_nanos: 456,
            sentinel: b"ordinary-token".to_vec(),
        };
        let mut expected = Sha256::new();
        let path = path.as_os_str().as_encoded_bytes();
        expected.update((path.len() as u64).to_le_bytes());
        expected.update(path);
        expected.update(42_u64.to_le_bytes());
        expected.update(123_u64.to_le_bytes());
        expected.update(456_u32.to_le_bytes());
        expected.update(14_u64.to_le_bytes());
        expected.update(b"ordinary-token");
        let expected: [u8; 32] = expected.finalize().into();

        assert_eq!(source_change_token(vec![entry]), expected);
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_source_stats_detect_same_size_rewrite_with_restored_mtime() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        fs::write(&path, b"alpha\n").unwrap();
        let original_modified = fs::metadata(&path).unwrap().modified().unwrap();
        let first = source_stats(&path).unwrap();

        fs::write(&path, b"omega\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_modified))
            .unwrap();
        let second = source_stats(&path).unwrap();

        assert_eq!(first.bytes, second.bytes);
        assert_ne!(first.change_token, second.change_token);
    }

    #[cfg(unix)]
    #[test]
    fn source_change_tokens_distinguish_lossy_non_utf8_path_labels() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path_a = PathBuf::from(OsString::from_vec(b"session-\x80.jsonl".to_vec()));
        let path_b = PathBuf::from(OsString::from_vec(b"session-\x81.jsonl".to_vec()));
        assert_eq!(path_a.display().to_string(), path_b.display().to_string());

        let entry = |path| SourceChangeEntry {
            path,
            len: 42,
            modified_secs: 123,
            modified_nanos: 456,
            sentinel: Vec::new(),
        };
        assert_ne!(
            source_change_token(vec![entry(path_a)]),
            source_change_token(vec![entry(path_b)])
        );
    }

    fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        PathBuf::from(sidecar)
    }

    fn real_wal_generation(root: &Path, name: &str, value: &str) -> (Vec<u8>, Vec<u8>) {
        let path = root.join(format!("{name}.db"));
        let writer = rusqlite::Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "PRAGMA page_size = 512;
                 VACUUM;
                 CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
                 INSERT INTO entries VALUES (1, 'alpha');
                 PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .unwrap();
        writer
            .execute("UPDATE entries SET value = ?1 WHERE id = 1", [value])
            .unwrap();
        (
            fs::read(&path).unwrap(),
            fs::read(sqlite_sidecar(&path, "-wal")).unwrap(),
        )
    }
}
