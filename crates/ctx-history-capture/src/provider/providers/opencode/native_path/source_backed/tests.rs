use std::fs;

use ctx_history_core::{
    ContentSourceResolver, EventHydrationRequest, HydrationFailureKind, NativeRecordCoordinate,
    TypedKey,
};
use rusqlite::{params, Connection};
use serde_json::json;

use super::*;
use crate::provider_sources::{DiscoveryPlatform, DiscoveryPlatformDirs, ProviderSourceStatus};

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

        assert_eq!(
            registration.mutation_policy(),
            OpenCodeSourceMutationPolicy::Replace
        );
        assert_eq!(scan.certificate.counts().complete_records, 2);
        assert_eq!(scan.certificate.counts().retained_records, 2);
        assert_eq!(scan.certificate.counts().indexed_documents, 2);
        assert!(scan.certificate.frontier().is_none());
        assert_eq!(scan.emitted_pages, 1);
        assert_eq!(scan.schema_family, "session_message_seq");
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

        let mut replayed = Vec::new();
        let replay = registration
            .scan(&path, &mut |page| {
                replayed.extend(page);
                Ok(())
            })
            .unwrap();
        assert_eq!(replay.source.identity(), scan.source.identity());
        assert_eq!(replayed[0].event_id, documents[0].event_id);
        assert_eq!(replayed[0].session_id, documents[0].session_id);

        let request =
            EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone())
                .unwrap();
        let resolver = registration.exact_resolver(&path);
        let hydrated = resolver.hydrate_event(&request).unwrap();
        assert_eq!(hydrated.provider_bytes, documents[0].body.as_bytes());

        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "update session_message
             set data = ?1, time_updated = time_updated + 1
             where id = 'message-0'",
            [r#"{"role":"user","text":"changed provider row"}"#],
        )
        .unwrap();
        drop(conn);
        let stale = resolver.hydrate_event(&request).unwrap_err();
        assert_eq!(stale.kind, HydrationFailureKind::StaleSourceEvidence);
    }
}

#[test]
fn active_wal_scan_reads_latest_rows_without_persistent_source_writes() {
    let registration = opencode_source_backed_registration();
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("opencode.sqlite");
    create_fixture(&path, "opencode", SOURCE_BACKED_PAGE_ROWS + 1);

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
    let before = sqlite_persistent_bytes(&path);

    let mut documents = Vec::new();
    registration
        .scan(&path, &mut |page| {
            assert!(page.len() <= SOURCE_BACKED_PAGE_ROWS);
            documents.extend(page);
            Ok(())
        })
        .unwrap();
    assert!(documents
        .iter()
        .any(|document| document.body.contains("OpenCode active WAL sentinel")));
    assert_eq!(sqlite_persistent_bytes(&path), before);
    drop(writer);
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

    let kilo = kilo_source_backed_registration().discover(&context);
    assert_eq!(kilo.sources.len(), 1);
    assert_eq!(kilo.sources[0].path, xdg.join("kilo/kilo.db"));
    assert_eq!(kilo.sources[0].status, ProviderSourceStatus::Available);

    let mimo = mimocode_source_backed_registration().discover(&context);
    assert_eq!(mimo.sources.len(), 1);
    assert_eq!(mimo.sources[0].path, xdg.join("mimocode/mimocode.db"));
    assert_eq!(mimo.sources[0].status, ProviderSourceStatus::Missing);
    assert!(mimo
        .sources
        .iter()
        .all(|source| !source.path.to_string_lossy().contains("preview")));

    for (registration, env_name) in [
        (opencode_source_backed_registration(), "OPENCODE_DB"),
        (kilo_source_backed_registration(), "KILO_DB"),
        (mimocode_source_backed_registration(), "MIMOCODE_DB"),
    ] {
        let memory = DiscoveryContext::new(&home, &cwd, DiscoveryPlatform::Linux, dirs.clone())
            .with_env("XDG_DATA_HOME", &xdg)
            .with_env(env_name, ":memory:");
        let report = registration.discover(&memory);
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

fn sqlite_persistent_bytes(path: &Path) -> Vec<Vec<u8>> {
    // Stock WAL readers may update volatile SHM reader marks.
    ["", "-wal"]
        .into_iter()
        .map(|suffix| {
            let mut component = path.as_os_str().to_os_string();
            component.push(suffix);
            fs::read(PathBuf::from(component)).unwrap()
        })
        .collect()
}
