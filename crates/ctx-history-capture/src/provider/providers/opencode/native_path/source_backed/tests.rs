use std::{ffi::OsString, fs};

use ctx_history_core::{
    BatchHydrationRequest, ContentSourceResolver, EventHydrationRequest, HydrationFailureKind,
    LocatorRevisionPolicy, NativeRecordCoordinate, TypedKey,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use rusqlite::{params, Connection};
use serde_json::json;

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, SourceBackedCoordinatorError,
        SourceBackedProviderRegistry, SourceBackedRouteError, SourceBackedRouteErrorKind,
        SourceBackedRouteSelection,
    },
    provider_sources::{
        discover_provider_sources_for_provider_with_context, provider_source_for_path,
        DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs, ProviderSourceStatus,
    },
};

#[cfg(target_os = "linux")]
#[test]
fn source_backed_open_does_not_follow_leaf_swap_after_authorization() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("opencode.sqlite");
    let attacker = temp.path().join("attacker.sqlite");
    let original = temp.path().join("original.sqlite");
    create_fixture(&path, "expected", 1);
    create_fixture(&attacker, "attacker", 1);

    let result = open_root_authorized_snapshot_with_hook(&path, || {
        fs::rename(&path, &original).unwrap();
        fs::rename(&attacker, &path).unwrap();
    });
    assert!(matches!(
        result,
        Err(OpenCodeSourceBackedError::SqliteSource(
            SqliteSourceAccessError::SourceChanged,
        ))
    ));
}

#[test]
fn cold_scan_and_exact_row_hydration_cover_all_three_dialects() {
    for registration in opencode_family_source_backed_registrations() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp
            .path()
            .join(format!("{}.sqlite", registration.provider().as_str()));
        let expected = create_fixture(&path, registration.provider().as_str(), 2);
        let mut documents = Vec::new();
        let scan = registration
            .scan(&path, &mut |page| {
                documents.extend(page);
                Ok(())
            })
            .unwrap();

        assert_eq!(scan.certificate.counts().complete_records, 2);
        assert_eq!(scan.certificate.counts().retained_records, 2);
        assert_eq!(scan.certificate.counts().indexed_documents, 2);
        assert!(scan.certificate.frontier().is_none());
        assert_eq!(
            scan.source.schema_variant(),
            "opencode-family-session_message_seq-v1"
        );
        assert_eq!(documents.len(), 2);
        let first_row: serde_json::Value = serde_json::from_str(&expected[0]).unwrap();
        let expected_first_body = first_row["text"].as_str().unwrap();
        assert_eq!(documents[0].body, expected_first_body);
        assert!(documents[0].body.ends_with("opencode-tail"));
        assert_eq!(documents[0].provider_session_id.as_deref(), Some("child"));
        let root_session_id = session_id(&scan.source, "root").unwrap();
        assert_eq!(documents[0].parent_session_id, Some(root_session_id));
        assert_eq!(documents[0].root_session_id, root_session_id);
        assert_eq!(documents[0].branch.as_deref(), Some("feature"));
        assert_eq!(
            documents[0].source_path.as_deref(),
            Some(path.to_string_lossy().as_ref())
        );
        assert_eq!(documents[0].agent_type, "subagent");
        assert!(!documents[0].is_primary);
        assert_eq!(documents[0].event_sequence, 0);
        assert_eq!(documents[1].event_sequence, 1);

        let NativeRecordCoordinate::ProviderSqlite {
            logical_relation,
            primary_key,
            row_version,
        } = documents[0].locator.coordinate()
        else {
            panic!("expected provider SQLite locator")
        };
        assert_eq!(logical_relation, "session_message");
        assert_eq!(primary_key, &TypedKey::Utf8("message-0".to_owned()));
        assert!(matches!(row_version, Some(TypedKey::Composite(parts)) if parts.len() == 2));
        assert_eq!(
            documents[0].locator.revision_policy(),
            LocatorRevisionPolicy::StableRecordEvidence
        );
        assert!(documents[0]
            .locator
            .certified_source_revision_digest()
            .is_none());

        let mut replayed = Vec::new();
        let replay = registration
            .scan(&path, &mut |page| {
                replayed.extend(page);
                Ok(())
            })
            .unwrap();
        assert_eq!(replay.source.identity(), scan.source.identity());
        assert_eq!(replay.certificate, scan.certificate);
        assert_eq!(replayed[0].event_id, documents[0].event_id);
        assert_eq!(replayed[0].session_id, documents[0].session_id);

        let request =
            EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone())
                .unwrap();
        let resolver = registration.exact_resolver(&path);
        let hydrated = resolver.hydrate_event(&request).unwrap();
        assert_eq!(hydrated.provider_bytes, documents[0].body.as_bytes());
        let session_requests = documents
            .iter()
            .map(|document| {
                EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
            })
            .collect();
        let session_request =
            SessionHydrationRequest::new(documents[0].session_id, session_requests).unwrap();
        let hydrated_session = resolver.hydrate_session(&session_request).unwrap();
        assert_eq!(hydrated_session.len(), documents.len());
        for (hydrated, document) in hydrated_session.iter().zip(&documents) {
            assert_eq!(hydrated.event_id, document.event_id);
            assert_eq!(hydrated.provider_bytes, document.body.as_bytes());
        }

        let batch_events = documents
            .iter()
            .rev()
            .map(|document| {
                EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
            })
            .collect();
        let batch_request = BatchHydrationRequest::new(batch_events).unwrap();
        let batch_resolver = registration.exact_resolver(&path);
        let hydrated_batch = batch_resolver.hydrate_batch(&batch_request).unwrap();
        assert_eq!(batch_resolver.hydration_counters(), (1, 1));
        for (hydrated, document) in hydrated_batch.records().iter().zip(documents.iter().rev()) {
            assert_eq!(hydrated.event_id, document.event_id);
            assert_eq!(hydrated.provider_bytes, document.body.as_bytes());
        }

        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "update session_message
             set data = ?1, time_updated = time_updated + 1
             where id = 'message-1'",
            [r#"{"role":"assistant","text":"unrelated changed provider row"}"#],
        )
        .unwrap();
        drop(conn);
        let unrelated_replacement = registration.scan(&path, &mut |_| Ok(())).unwrap();
        assert_ne!(unrelated_replacement.certificate, scan.certificate);
        assert_eq!(
            resolver.hydrate_event(&request).unwrap().provider_bytes,
            documents[0].body.as_bytes()
        );

        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "update session_message
             set data = ?1, time_updated = time_updated + 1
             where id = 'message-0'",
            [r#"{"role":"user","text":"changed provider row"}"#],
        )
        .unwrap();
        drop(conn);
        let replacement = registration.scan(&path, &mut |_| Ok(())).unwrap();
        assert_ne!(replacement.certificate, unrelated_replacement.certificate);
        assert!(replacement.certificate.frontier().is_none());
        let stale = resolver.hydrate_event(&request).unwrap_err();
        assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);
        let stale_batch = batch_resolver.hydrate_batch(&batch_request).unwrap_err();
        assert_eq!(stale_batch.kind, HydrationFailureKind::StaleRecordEvidence);
    }
}

#[test]
fn exact_hydration_does_not_scan_unrelated_provider_rows() {
    for registration in opencode_family_source_backed_registrations() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp
            .path()
            .join(format!("{}.sqlite", registration.provider().as_str()));
        create_fixture(&path, registration.provider().as_str(), 2);
        let (_, documents) = collect_scan(registration, &path);
        let request =
            EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone())
                .unwrap();

        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "update session_message
                 set time_updated = 'invalid-unrelated-order'
                 where id = 'message-1'",
                [],
            )
            .unwrap();
        assert!(OpenCodeNativeSchema::probe(&connection, registration.dialect).is_err());
        drop(connection);

        let hydrated = registration
            .exact_resolver(&path)
            .hydrate_event(&request)
            .unwrap();
        assert_eq!(hydrated.provider_bytes, documents[0].body.as_bytes());
    }
}

#[test]
fn grouped_batch_hydration_uses_one_snapshot_and_bounded_native_key_queries() {
    let registration = opencode_source_backed_registration();
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("opencode.sqlite");
    create_fixture(&path, "opencode", hydration::HYDRATION_NATIVE_KEY_BATCH + 1);
    let (_, documents) = collect_scan(registration, &path);
    let request = BatchHydrationRequest::new(
        documents
            .iter()
            .rev()
            .map(|document| {
                EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
            })
            .collect(),
    )
    .unwrap();
    let resolver = registration.exact_resolver(&path);

    let hydrated = resolver.hydrate_batch(&request).unwrap();

    assert_eq!(resolver.hydration_counters(), (1, 2));
    assert_eq!(hydrated.records().len(), documents.len());
    for (record, document) in hydrated.records().iter().zip(documents.iter().rev()) {
        assert_eq!(record.event_id, document.event_id);
        assert_eq!(record.provider_bytes, document.body.as_bytes());
    }
}

#[test]
fn active_wal_scan_reads_latest_rows_without_persistent_source_writes() {
    let registration = opencode_source_backed_registration();
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("opencode.sqlite");
    create_fixture(&path, "opencode", 65);

    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch("pragma wal_checkpoint(truncate)")
        .unwrap();
    let wal_body = r#"{"role":"user","text":"OpenCode active WAL sentinel"}"#;
    writer
        .execute(
            "update session_message set data = ?1, time_updated = time_updated + 1
             where id = 'message-0'",
            [wal_body],
        )
        .unwrap();
    let before = sqlite_directory_state(temp.path());

    let mut documents = Vec::new();
    registration
        .scan(&path, &mut |page| {
            documents.extend(page);
            Ok(())
        })
        .unwrap();
    assert!(documents
        .iter()
        .any(|document| document.body.contains("OpenCode active WAL sentinel")));
    assert_eq!(sqlite_directory_state(temp.path()), before);
    drop(writer);
}

#[test]
fn checkpoint_sidecar_removal_and_vacuum_preserve_all_family_logical_snapshots() {
    for registration in opencode_family_source_backed_registrations() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp
            .path()
            .join(format!("{}.sqlite", registration.provider().as_str()));
        create_fixture(&path, registration.provider().as_str(), 2);
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "wal").unwrap();
        writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        writer
            .execute_batch("pragma wal_checkpoint(truncate)")
            .unwrap();
        writer
            .execute(
                "update session_message
                 set data = ?1, time_updated = time_updated + 1
                 where id = 'message-0'",
                [r#"{"role":"user","text":"stable logical WAL row"}"#],
            )
            .unwrap();

        let (baseline, baseline_documents) = collect_scan(registration, &path);
        let baseline_certificate = baseline.certificate.clone();
        let request = EventHydrationRequest::new(
            baseline_documents[0].event_id,
            baseline_documents[0].locator.clone(),
        )
        .unwrap();
        let resolver = registration.exact_resolver(&path);
        assert_eq!(
            resolver.hydrate_event(&request).unwrap().provider_bytes,
            baseline_documents[0].body.as_bytes()
        );

        writer
            .execute_batch("pragma wal_checkpoint(truncate)")
            .unwrap();
        assert_logical_replay(
            registration,
            &path,
            &baseline_certificate,
            &baseline_documents,
        );

        let journal_mode: String = writer
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
        writer
            .pragma_update(None, "journal_mode", "delete")
            .unwrap();
        drop(writer);
        assert!(!sqlite_sidecar_path(&path, "-wal").exists());
        assert_logical_replay(
            registration,
            &path,
            &baseline_certificate,
            &baseline_documents,
        );

        let vacuum = Connection::open(&path).unwrap();
        vacuum.execute_batch("vacuum").unwrap();
        drop(vacuum);
        assert_logical_replay(
            registration,
            &path,
            &baseline_certificate,
            &baseline_documents,
        );
        assert_eq!(
            resolver.hydrate_event(&request).unwrap().provider_bytes,
            baseline_documents[0].body.as_bytes()
        );
    }
}

#[test]
fn schema_policy_and_projection_classification_changes_replace() {
    for registration in opencode_family_source_backed_registrations() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp
            .path()
            .join(format!("{}.sqlite", registration.provider().as_str()));
        create_fixture(&path, registration.provider().as_str(), 2);
        let (baseline, baseline_documents) = collect_scan(registration, &path);
        let request = EventHydrationRequest::new(
            baseline_documents[0].event_id,
            baseline_documents[0].locator.clone(),
        )
        .unwrap();
        let resolver = registration.exact_resolver(&path);

        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 7).unwrap();
        drop(connection);
        let (schema_replacement, schema_documents) = collect_scan(registration, &path);
        assert_ne!(schema_replacement.certificate, baseline.certificate);
        assert_eq!(schema_documents.len(), baseline_documents.len());
        assert!(schema_replacement.certificate.frontier().is_none());
        assert_eq!(
            resolver.hydrate_event(&request).unwrap().provider_bytes,
            baseline_documents[0].body.as_bytes()
        );

        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "update session_message
                 set data = 'not-json', time_updated = time_updated + 1
                 where id = 'message-0'",
                [],
            )
            .unwrap();
        drop(connection);
        let (classification_replacement, classified_documents) = collect_scan(registration, &path);
        assert_ne!(
            classification_replacement.certificate,
            schema_replacement.certificate
        );
        assert_eq!(
            classification_replacement
                .certificate
                .counts()
                .rejected_records,
            1
        );
        assert_eq!(classified_documents.len(), 1);
        assert!(classification_replacement.certificate.frontier().is_none());
        assert_eq!(
            resolver.hydrate_event(&request).unwrap_err().kind,
            HydrationFailureKind::StaleRecordEvidence
        );
    }
}

#[test]
fn one_decode_pass_streams_logical_rows_directly_without_a_page_bridge() {
    for registration in opencode_family_source_backed_registrations() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp
            .path()
            .join(format!("{}.sqlite", registration.provider().as_str()));
        let row_count = 129;
        create_fixture(&path, registration.provider().as_str(), row_count);

        let mut documents = Vec::new();
        let baseline = registration
            .scan(&path, &mut |page| {
                assert_eq!(page.len(), 1);
                documents.extend(page);
                Ok(())
            })
            .unwrap();
        assert_eq!(documents.len(), row_count);
        assert_eq!(
            baseline.certificate.counts().complete_records,
            row_count as u64
        );
        assert!(baseline.certificate.frontier().is_none());

        let request =
            EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone())
                .unwrap();
        let resolver = registration.exact_resolver(&path);
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("vacuum").unwrap();
        drop(connection);

        let replay = registration.scan(&path, &mut |_| Ok(())).unwrap();
        assert_eq!(replay.certificate, baseline.certificate);
        assert_eq!(
            replay.certificate.counts().complete_records,
            row_count as u64
        );
        assert_eq!(
            resolver.hydrate_event(&request).unwrap().provider_bytes,
            documents[0].body.as_bytes()
        );
    }
}

#[cfg(unix)]
#[test]
fn active_wal_read_only_provider_directory_is_byte_and_name_unchanged() {
    use std::os::unix::fs::PermissionsExt as _;

    let registration = opencode_source_backed_registration();
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("opencode.sqlite");
    create_fixture(&path, "opencode", 2);
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute(
            "update session_message
             set data = ?1, time_updated = time_updated + 1
             where id = 'message-0'",
            [r#"{"role":"user","text":"read-only active WAL row"}"#],
        )
        .unwrap();

    for entry in fs::read_dir(temp.path()).unwrap() {
        let entry = entry.unwrap();
        let mut permissions = entry.metadata().unwrap().permissions();
        permissions.set_mode(0o444);
        fs::set_permissions(entry.path(), permissions).unwrap();
    }
    let mut directory_permissions = fs::metadata(temp.path()).unwrap().permissions();
    directory_permissions.set_mode(0o555);
    fs::set_permissions(temp.path(), directory_permissions).unwrap();
    let before = sqlite_directory_state(temp.path());

    let scan = registration.scan(&path, &mut |_| Ok(()));
    let after = sqlite_directory_state(temp.path());

    let mut directory_permissions = fs::metadata(temp.path()).unwrap().permissions();
    directory_permissions.set_mode(0o755);
    fs::set_permissions(temp.path(), directory_permissions).unwrap();
    for entry in fs::read_dir(temp.path()).unwrap() {
        let entry = entry.unwrap();
        let mut permissions = entry.metadata().unwrap().permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(entry.path(), permissions).unwrap();
    }
    drop(writer);

    scan.unwrap();
    assert_eq!(after, before);
}

#[test]
fn logical_same_route_replacement_preserves_the_certificate_without_append() {
    for registration in opencode_family_source_backed_registrations() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp
            .path()
            .join(format!("{}.sqlite", registration.provider().as_str()));
        let index = temp.path().join("index");
        create_fixture(&path, registration.provider().as_str(), 2);
        let source = provider_source_for_path(registration.provider(), path.clone());
        assert_eq!(source.status, ProviderSourceStatus::Available);
        let mut registry = SourceBackedProviderRegistry::new();
        register_source_backed_route(
            &mut registry,
            source,
            SourceBackedRouteSelection::ExplicitManual,
        )
        .unwrap();
        let options = WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        };
        let cold = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("vacuum").unwrap();
        drop(connection);
        let replay = refresh_source_backed_generation(&index, &registry, options).unwrap();

        assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
        assert_eq!(replay.commit.opstamp, cold.commit.opstamp);
        assert_eq!(replay.commit.indexed_documents, 2);
        assert_eq!(replay.sources, cold.sources);
        assert!(cold.removals.is_empty());
        assert!(replay.removals.is_empty());
        assert!(replay
            .sources
            .iter()
            .all(|certificate| certificate.frontier().is_none()));
    }
}

#[test]
fn route_deletes_a_missing_database_and_preserves_on_acquisition_unavailable() {
    let registration = opencode_source_backed_registration();
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("opencode.sqlite");
    let moved = temp.path().join("opencode.moved.sqlite");
    let index = temp.path().join("index");
    create_fixture(&path, "opencode", 2);
    let source = provider_source_for_path(registration.provider(), path.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    register_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let cold = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    let old_source = cold.sources[0].observation().source().clone();

    fs::rename(&path, &moved).unwrap();
    let deleted = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    assert!(deleted.sources.is_empty());
    assert!(deleted
        .removals
        .iter()
        .any(|removal| removal.deletion.source().exact_descriptor_eq(&old_source)));

    fs::rename(&moved, &path).unwrap();
    let restored = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    assert_eq!(restored.sources.len(), 1);
    let retained_generation = restored.commit.generation_id.clone();
    let writer = Connection::open(&path).unwrap();
    writer.execute_batch("begin immediate").unwrap();
    writer
        .execute(
            "update session_message set time_updated = time_updated + 1 where id = 'message-0'",
            [],
        )
        .unwrap();
    let error = refresh_source_backed_generation(&index, &registry, options).unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::RouteScan {
            source: SourceBackedRouteError {
                kind: SourceBackedRouteErrorKind::Unavailable,
                ..
            },
            ..
        }
    ));
    assert_eq!(
        VerifiedIndex::open(&index).unwrap().generation_id(),
        retained_generation
    );
    writer.execute_batch("rollback").unwrap();
}

#[test]
fn registration_discovery_preserves_winners_and_inactive_exclusions() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(xdg.join("kilo")).unwrap();
    fs::create_dir_all(xdg.join("mimocode/preview")).unwrap();
    fs::write(xdg.join("kilo/kilo.db"), b"current").unwrap();
    fs::write(xdg.join("kilo/opencode.db"), b"legacy").unwrap();
    fs::write(xdg.join("mimocode/preview/mimocode.db"), b"inactive").unwrap();

    let dirs = DiscoveryPlatformDirs {
        data: Some(xdg.clone()),
        config: None,
        state: None,
        local_data: Some(xdg.clone()),
    };
    let context = DiscoveryContext::new(&home, &cwd, DiscoveryPlatform::Linux, dirs.clone())
        .with_env("XDG_DATA_HOME", &xdg);

    let kilo = discover_provider_sources_for_provider_with_context(&context, CaptureProvider::Kilo);
    assert_eq!(kilo.sources.len(), 1);
    assert_eq!(kilo.sources[0].path, xdg.join("kilo/kilo.db"));
    assert_eq!(kilo.sources[0].status, ProviderSourceStatus::Available);

    let mimo =
        discover_provider_sources_for_provider_with_context(&context, CaptureProvider::MiMoCode);
    assert_eq!(mimo.sources.len(), 1);
    assert_eq!(mimo.sources[0].path, xdg.join("mimocode/mimocode.db"));
    assert_eq!(mimo.sources[0].status, ProviderSourceStatus::Missing);
    assert!(mimo
        .sources
        .iter()
        .all(|source| !source.path.to_string_lossy().contains("preview")));

    for (provider, env_name) in [
        (CaptureProvider::OpenCode, "OPENCODE_DB"),
        (CaptureProvider::Kilo, "KILO_DB"),
        (CaptureProvider::MiMoCode, "MIMOCODE_DB"),
    ] {
        let memory = DiscoveryContext::new(&home, &cwd, DiscoveryPlatform::Linux, dirs.clone())
            .with_env("XDG_DATA_HOME", &xdg)
            .with_env(env_name, ":memory:");
        let report = discover_provider_sources_for_provider_with_context(&memory, provider);
        assert!(report.sources.is_empty());
        assert_eq!(report.issues.len(), 1);
    }
}

fn create_fixture(path: &Path, provider: &str, rows: usize) -> Vec<String> {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "create table session (
             id text primary key,
             parent_id text,
             title text,
             directory text,
             branch text,
             agent text,
             time_created integer not null,
             time_updated integer not null
         );
         create table session_message (
             id text primary key,
             session_id text not null,
             type text not null,
             seq integer not null,
             time_created integer not null,
             time_updated integer not null,
             data text not null
         );",
    )
    .unwrap();
    conn.execute(
        "insert into session
         (id, parent_id, title, directory, branch, agent, time_created, time_updated)
         values ('root', null, 'Root', '/workspace/root', 'main', 'primary', 1, 2)",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into session
         (id, parent_id, title, directory, branch, agent, time_created, time_updated)
         values ('child', 'root', 'Child', '/workspace/child', 'feature',
                 'subagent', 2, 3)",
        [],
    )
    .unwrap();
    let mut expected = Vec::new();
    for sequence in 0..rows {
        let text = if sequence == 0 {
            format!(
                "{} opencode-tail",
                format!("{provider} retained ").repeat(400)
            )
        } else {
            format!("{provider} retained message {sequence}")
        };
        let data = json!({
            "role": if sequence % 2 == 0 { "user" } else { "assistant" },
            "text": text
        })
        .to_string();
        conn.execute(
            "insert into session_message
             (id, session_id, type, seq, time_created, time_updated, data)
             values (?1, 'child', 'message', ?2, ?3, ?3, ?4)",
            params![
                format!("message-{sequence}"),
                i64::try_from(sequence).unwrap(),
                1_800_000_000_000_i64 + i64::try_from(sequence).unwrap(),
                data,
            ],
        )
        .unwrap();
        expected.push(data);
    }
    expected
}

fn collect_scan(
    registration: OpenCodeSourceBackedRegistration,
    path: &Path,
) -> (OpenCodeSourceBackedScan, Vec<LexicalDocument>) {
    let mut documents = Vec::new();
    let scan = registration
        .scan(path, &mut |page| {
            documents.extend(page);
            Ok(())
        })
        .unwrap();
    (scan, documents)
}

fn assert_logical_replay(
    registration: OpenCodeSourceBackedRegistration,
    path: &Path,
    expected_certificate: &CertifiedSource,
    expected_documents: &[LexicalDocument],
) {
    let (scan, documents) = collect_scan(registration, path);
    assert_eq!(&scan.certificate, expected_certificate);
    assert_eq!(documents.len(), expected_documents.len());
    for (document, expected) in documents.iter().zip(expected_documents) {
        assert_eq!(document.event_id, expected.event_id);
        assert_eq!(document.session_id, expected.session_id);
        assert_eq!(document.source, expected.source);
        assert_eq!(document.locator, expected.locator);
        assert_eq!(document.event_sequence, expected.event_sequence);
        assert_eq!(document.body, expected.body);
    }
    assert!(scan.certificate.frontier().is_none());
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut component = path.as_os_str().to_os_string();
    component.push(suffix);
    component.into()
}

fn sqlite_directory_state(path: &Path) -> Vec<(OsString, Vec<u8>)> {
    let mut state = fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect::<Vec<_>>();
    state.sort_by(|left, right| left.0.cmp(&right.0));
    state
}
