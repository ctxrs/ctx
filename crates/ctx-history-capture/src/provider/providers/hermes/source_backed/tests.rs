use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{CaptureProvider, NativeRecordCoordinate, SourceAnchor, TypedKey};
use ctx_history_index::WriterOptions;
use rusqlite::Connection;

use super::*;
use crate::{
    provider::source_backed::{
        family::document::register_replacement_document_tree_route,
        refresh_source_backed_generation, SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

fn create_state_db(path: &Path, profile: &str, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "create table sessions (
             id text primary key,
             source text not null,
             parent_session_id text,
             started_at real not null,
             ended_at real,
             cwd text,
             git_branch text,
             git_repo_root text
         );
         create table messages (
             id integer not null,
             session_id text not null,
             role text not null,
             content text,
             timestamp real not null,
             active integer not null default 1,
             compacted integer not null default 0
         );",
    )
    .unwrap();
    let root = format!("{profile}-root");
    let child = format!("{profile}-child");
    conn.execute(
        "insert into sessions
             (id, source, parent_session_id, started_at, ended_at, cwd,
              git_branch, git_repo_root)
         values (?1, 'cli', null, 1.0, 4.0, ?2, ?3, ?4)",
        rusqlite::params![
            root,
            format!("/work/{profile}"),
            format!("branch-{profile}"),
            format!("/repo/{profile}")
        ],
    )
    .unwrap();
    conn.execute(
        "insert into sessions
             (id, source, parent_session_id, started_at, ended_at, cwd,
              git_branch, git_repo_root)
         values (?1, 'cli', ?2, 2.0, 3.0, ?3, ?4, ?5)",
        rusqlite::params![
            child,
            root,
            format!("/work/{profile}"),
            format!("branch-{profile}"),
            format!("/repo/{profile}")
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages
             (id, session_id, role, content, timestamp, active, compacted)
         values (7, ?1, 'user', ?2, 2.5, 1, 0)",
        rusqlite::params![child, body],
    )
    .unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn source_backed_open_does_not_follow_leaf_swap_after_authorization() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let attacker = temp.path().join("attacker.db");
    let original = temp.path().join("original.db");
    create_state_db(&path, "expected", "expected");
    create_state_db(&attacker, "attacker", "attacker");

    let result = open_root_authorized_snapshot_with_hook(
        crate::test_provider_sqlite_data_root(),
        &path,
        || {
            fs::rename(&path, &original).unwrap();
            fs::rename(&attacker, &path).unwrap();
        },
    );
    assert!(
        matches!(
            result,
            Err(HermesSourceBackedError::Capture(
                CaptureError::InvalidProviderTranscriptPath { .. }
            )) | Err(HermesSourceBackedError::SqliteSource(
                SqliteSourceAccessError::SourceChanged
            ))
        ),
        "{result:?}"
    );
}

#[test]
fn active_wal_scan_reads_latest_rows_without_persistent_source_writes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_state_db(&path, "wal", "before WAL");
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch("pragma wal_checkpoint(truncate)")
        .unwrap();
    writer
        .execute(
            "update messages set content = ?1 where id = 7",
            ["Hermes active WAL sentinel"],
        )
        .unwrap();
    let before = sqlite_persistent_bytes(&path);
    let candidate = hermes_source_backed_explicit(
        crate::test_provider_sqlite_data_root(),
        &path,
        SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8("wal").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let (_, records) = scan_candidate(&candidate);
    assert!(event(&records).body.contains("Hermes active WAL sentinel"));
    assert_eq!(sqlite_persistent_bytes(&path), before);
    drop(writer);
}

#[test]
fn idle_wal_writer_first_scan_succeeds_and_append_changes_revision() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_state_db(&path, "idle-wal", "before idle WAL");
    let writer = Connection::open(&path).unwrap();
    let mode: String = writer
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
    assert!(
        !path.with_file_name("state.db-wal").exists(),
        "the idle writer must not have materialized a WAL pathname"
    );
    let candidate = hermes_source_backed_explicit(
        crate::test_provider_sqlite_data_root(),
        &path,
        SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8("idle-wal").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();

    let (before_certificate, before_records) = scan_candidate(&candidate);
    assert!(before_records.iter().any(|record| matches!(
        record,
        HermesSourceBackedRecord::Event(event) if event.body == "before idle WAL"
    )));
    let (unchanged_certificate, _) = scan_candidate(&candidate);
    assert_eq!(
        before_certificate.observation().revision(),
        unchanged_certificate.observation().revision()
    );
    assert_eq!(
        before_certificate.content_digest(),
        unchanged_certificate.content_digest()
    );
    let database_before_append = fs::read(&path).unwrap();
    writer
        .execute(
            "insert into messages
                 (id, session_id, role, content, timestamp, active, compacted)
             values (8, ?1, 'user', ?2, 3.5, 1, 0)",
            rusqlite::params!["idle-wal-child", "Hermes committed WAL append sentinel"],
        )
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), database_before_append);

    let (after_certificate, after_records) = scan_candidate(&candidate);
    assert!(after_records.iter().any(|record| matches!(
        record,
        HermesSourceBackedRecord::Event(event)
            if event.body == "Hermes committed WAL append sentinel"
    )));
    assert_ne!(
        before_certificate.observation().revision(),
        after_certificate.observation().revision()
    );
    assert_ne!(
        before_certificate.content_digest(),
        after_certificate.content_digest()
    );

    drop(writer);
}

#[test]
fn terminal_family_treats_empty_wal_create_remove_and_sibling_churn_as_noops() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let path = temp.path().join("state.db");
    let wal = path.with_file_name("state.db-wal");
    let sibling = temp.path().join("unrelated-sibling");
    create_state_db(&path, "terminal-noop", "terminal noop");
    let writer = Connection::open(&path).unwrap();
    let mode: String = writer
        .query_row("pragma journal_mode = wal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer
        .execute_batch("pragma wal_autocheckpoint = 0")
        .unwrap();
    assert!(!wal.exists());

    let (_, absent_snapshot) = open_root_authorized_snapshot_with_hook(&data_root, &path, || {
        fs::write(&sibling, b"sibling churn during open").unwrap();
    })
    .unwrap();
    let absent_revision = *absent_snapshot.evidence().revision();
    let absent_terminal = absent_snapshot.terminal_revalidator();
    absent_snapshot.finish().unwrap();
    fs::write(&wal, b"").unwrap();
    absent_terminal().unwrap();

    let (_, empty_snapshot) = open_root_authorized_snapshot(&data_root, &path).unwrap();
    assert_eq!(empty_snapshot.evidence().revision(), &absent_revision);
    assert_eq!(empty_snapshot.evidence().wal_length(), None);
    let empty_terminal = empty_snapshot.terminal_revalidator();
    empty_snapshot.finish().unwrap();
    fs::remove_file(&wal).unwrap();
    empty_terminal().unwrap();

    fs::write(&sibling, b"sibling churn after seal").unwrap();
    empty_terminal().unwrap();
    drop(writer);
}

#[test]
fn terminal_family_rejects_nonempty_wal_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let path = temp.path().join("state.db");
    create_state_db(&path, "terminal-wal", "before terminal mutation");
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute("update messages set content = content where id = 7", [])
        .unwrap();

    let (_, snapshot) = open_root_authorized_snapshot(&data_root, &path).unwrap();
    let terminal = snapshot.terminal_revalidator();
    snapshot.finish().unwrap();
    writer
        .execute(
            "update messages set content = 'after terminal mutation' where id = 7",
            [],
        )
        .unwrap();
    assert!(matches!(
        terminal(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));
    drop(writer);
}

#[test]
fn concurrent_committed_wal_mutation_during_snapshot_open_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let path = temp.path().join("state.db");
    create_state_db(&path, "concurrent-wal", "before concurrent mutation");
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute("update messages set content = content where id = 7", [])
        .unwrap();

    let result = open_root_authorized_snapshot_with_hook(&data_root, &path, || {
        writer
            .execute(
                "update messages set content = 'during snapshot open' where id = 7",
                [],
            )
            .unwrap();
    });
    assert!(matches!(
        result,
        Err(HermesSourceBackedError::SqliteSource(
            SqliteSourceAccessError::SourceChanged
        ))
    ));
    drop(writer);
}

#[test]
fn hermes_source_backed_indexes_full_policy_body_and_hydrates_display_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let text = format!("hermes-head-{}-hermes-tail", "x".repeat(20_000));
    create_state_db(&path, "full", &text);
    let candidate = hermes_source_backed_explicit(
        crate::test_provider_sqlite_data_root(),
        &path,
        SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8("full").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let (_, records) = scan_candidate(&candidate);
    let document = event(&records);
    assert_eq!(document.body, text);
    assert!(document.body.ends_with("hermes-tail"));

    let resolver = HermesLocatorResolver::new(
        crate::test_provider_sqlite_data_root(),
        candidate.path(),
        candidate.source().clone(),
    );
    let hydrated = resolver
        .hydrate_locators(&[&document.locator])
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(hydrated.text, text);
    assert_eq!(hydrated.provider_bytes, text.as_bytes());
    assert_eq!(resolver.counters(), (1, 1, 1));
}

#[test]
fn oversized_indivisible_message_is_a_typed_rejection_not_a_truncated_document() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let oversized = "x".repeat(NATIVE_INGESTION_PAGE_MAX_BYTES + 1);
    create_state_db(&path, "oversized", &oversized);
    let candidate = hermes_source_backed_explicit(
        crate::test_provider_sqlite_data_root(),
        &path,
        SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8("oversized").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();

    let (certificate, records) = scan_candidate(&candidate);
    assert_eq!(certificate.counts().complete_records, 3);
    assert_eq!(certificate.counts().indexed_documents, 0);
    assert_eq!(certificate.counts().rejected_records, 1);
    assert!(!records
        .iter()
        .any(|record| matches!(record, HermesSourceBackedRecord::Event(_))));
    let rejection = records
        .iter()
        .find_map(|record| match record {
            HermesSourceBackedRecord::Rejected(rejection) => Some(rejection),
            HermesSourceBackedRecord::Session(_) | HermesSourceBackedRecord::Event(_) => None,
        })
        .expect("typed oversized Hermes rejection");
    assert!(rejection.reason.contains("indivisible"));
    assert!(rejection.reason.contains("NativePath page"));
}

#[test]
fn projection_and_hydration_use_bounded_native_row_sets() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_state_db(&path, "sets", "set message 7");
    let mut conn = Connection::open(&path).unwrap();
    let transaction = conn.transaction().unwrap();
    for message_id in 8_i64..=136 {
        transaction
            .execute(
                "insert into messages
                     (id, session_id, role, content, timestamp, active, compacted)
                 values (?1, 'sets-child', 'user', ?2, ?3, 1, 0)",
                rusqlite::params![
                    message_id,
                    format!("set message {message_id}"),
                    message_id as f64
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    drop(conn);

    let candidate = hermes_source_backed_explicit(
        crate::test_provider_sqlite_data_root(),
        &path,
        SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8("sets").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let mut records = Vec::new();
    let scan = scan_hermes_source_backed(&candidate, |page| {
        records.extend(page.records);
        Ok(())
    })
    .unwrap();
    assert_eq!(scan.decoded_rows, 132);
    assert_eq!(scan.emitted_pages, 3);
    assert_eq!(scan.peak_buffered_records, 64);
    assert_eq!(scan.native_candidate_query_batches, 6);
    assert_eq!(scan.native_hydration_query_batches, 4);
    assert_eq!(scan.max_native_rows_per_set, 64);

    let locators = records
        .iter()
        .filter_map(|record| match record {
            HermesSourceBackedRecord::Event(document) => Some(&document.locator),
            HermesSourceBackedRecord::Session(_) | HermesSourceBackedRecord::Rejected(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(locators.len(), 130);
    let resolver = HermesLocatorResolver::new(
        crate::test_provider_sqlite_data_root(),
        candidate.path(),
        candidate.source().clone(),
    );
    let hydrated = resolver.hydrate_locators(&locators).unwrap();
    assert_eq!(hydrated.len(), 130);
    assert_eq!(resolver.counters(), (1, 1, 130));
    assert_eq!(hydrated.first().unwrap().text, "set message 7");
    assert_eq!(hydrated.last().unwrap().text, "set message 136");
}

#[test]
fn replacement_route_uses_one_active_wal_snapshot_and_zero_replay_projection() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    let index = temp.path().join("index");
    create_state_db(&path, "route", "route baseline");
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute("update messages set content = content where id = 7", [])
        .unwrap();
    let candidate = hermes_source_backed_explicit(
        crate::test_provider_sqlite_data_root(),
        &path,
        SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8("route").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let source = ProviderSource {
        provider: CaptureProvider::Hermes,
        path: path.clone(),
        exists: true,
        source_format: HERMES_SQLITE_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();
    register_replacement_document_tree_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        candidate,
    )
    .unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };

    let before_cold = sqlite_persistent_bytes(&path);
    reset_route_work_counters();
    let cold = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    assert_eq!(sqlite_persistent_bytes(&path), before_cold);
    assert!(cold.sources[0].frontier().is_some());
    let cold_work = route_work_counters();
    assert_eq!(cold_work.logical_observation_passes, 1);
    assert_eq!(cold_work.projection_passes, 1);
    assert_eq!(cold_work.immutable_snapshot_opens, 0);
    assert_eq!(cold_work.copied_snapshot_opens, 1);
    assert!(cold_work.source_bytes_copied > 0);
    assert_eq!(cold_work.terminal_fences, 1);
    assert_eq!(cold_work.terminal_revalidations, 2);

    let before_replay = sqlite_persistent_bytes(&path);
    reset_route_work_counters();
    let replay = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    assert_eq!(sqlite_persistent_bytes(&path), before_replay);
    assert_eq!(replay.sources, cold.sources);
    assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
    assert_eq!(replay.commit.opstamp, cold.commit.opstamp);
    let replay_work = route_work_counters();
    assert_eq!(replay_work.logical_observation_passes, 1);
    assert_eq!(replay_work.projection_passes, 0);
    assert_eq!(replay_work.immutable_snapshot_opens, 0);
    assert_eq!(replay_work.copied_snapshot_opens, 1);
    assert!(replay_work.source_bytes_copied > 0);
    assert_eq!(replay_work.terminal_fences, 1);
    assert_eq!(replay_work.terminal_revalidations, 2);

    writer
        .execute(
            "update messages set content = 'route replacement' where id = 7",
            [],
        )
        .unwrap();
    let before_replacement = sqlite_persistent_bytes(&path);
    reset_route_work_counters();
    let replacement = refresh_source_backed_generation(&index, &registry, options).unwrap();
    assert_eq!(sqlite_persistent_bytes(&path), before_replacement);
    assert_ne!(replacement.sources, replay.sources);
    assert!(replacement.sources[0].frontier().is_some());
    let replacement_work = route_work_counters();
    assert_eq!(replacement_work.logical_observation_passes, 1);
    assert_eq!(replacement_work.projection_passes, 1);
    assert_eq!(replacement_work.immutable_snapshot_opens, 0);
    assert_eq!(replacement_work.copied_snapshot_opens, 1);
    assert!(replacement_work.source_bytes_copied > 0);
    assert_eq!(replacement_work.terminal_fences, 1);
    assert_eq!(replacement_work.terminal_revalidations, 2);
    drop(writer);
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

fn scan_candidate(
    candidate: &HermesSourceCandidate,
) -> (CertifiedSource, Vec<HermesSourceBackedRecord>) {
    let mut records = Vec::new();
    let scan = scan_hermes_source_backed(candidate, |page| {
        assert!(!page.records.is_empty());
        assert!(page.records.len() <= NATIVE_INGESTION_PAGE_MAX_UNITS);
        assert!(page.owned_bytes <= NATIVE_INGESTION_PAGE_MAX_BYTES);
        records.extend(page.records);
        Ok(())
    })
    .unwrap();
    assert_eq!(scan.row_decode_passes, 1);
    assert_eq!(
        scan.decoded_rows,
        scan.certificate.counts().complete_records
    );
    assert_eq!(scan.emitted_pages == 0, records.is_empty());
    assert!(scan.peak_buffered_records <= NATIVE_INGESTION_PAGE_MAX_UNITS as u64);
    assert!(scan.native_candidate_query_batches > 0);
    assert!(scan.native_hydration_query_batches <= scan.native_candidate_query_batches);
    assert!(scan.max_native_rows_per_set <= 64);
    (scan.certificate, records)
}

fn event(records: &[HermesSourceBackedRecord]) -> &LexicalDocument {
    records
        .iter()
        .find_map(|record| match record {
            HermesSourceBackedRecord::Event(event) => Some(event),
            HermesSourceBackedRecord::Session(_) | HermesSourceBackedRecord::Rejected(_) => None,
        })
        .unwrap()
}

fn child_session(records: &[HermesSourceBackedRecord]) -> &HermesSourceBackedSession {
    records
        .iter()
        .find_map(|record| match record {
            HermesSourceBackedRecord::Session(session)
                if session.provider_parent_session_id.is_some() =>
            {
                Some(session)
            }
            HermesSourceBackedRecord::Session(_)
            | HermesSourceBackedRecord::Event(_)
            | HermesSourceBackedRecord::Rejected(_) => None,
        })
        .unwrap()
}

fn root_session(records: &[HermesSourceBackedRecord]) -> &HermesSourceBackedSession {
    records
        .iter()
        .find_map(|record| match record {
            HermesSourceBackedRecord::Session(session)
                if session.provider_parent_session_id.is_none() =>
            {
                Some(session)
            }
            HermesSourceBackedRecord::Session(_)
            | HermesSourceBackedRecord::Event(_)
            | HermesSourceBackedRecord::Rejected(_) => None,
        })
        .unwrap()
}

#[test]
fn hermes_source_backed_gateway_inventory_scans_multiple_profiles_and_hydrates_exact_rows() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let root = home.join(".hermes");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("config.yaml"),
        "gateway:\n  multiplex_profiles: true\n",
    )
    .unwrap();
    create_state_db(
        &root.join("state.db"),
        "default",
        "default gateway exact sentinel",
    );
    create_state_db(
        &root.join("profiles/alpha/state.db"),
        "alpha",
        "alpha gateway exact sentinel",
    );
    create_state_db(
        &root.join("profiles/zeta/state.db"),
        "zeta",
        "zeta gateway exact sentinel",
    );
    create_state_db(
        &root.join("profiles/Bad.Name/state.db"),
        "invalid",
        "must not be inventoried",
    );

    let context = DiscoveryContext::new(
        &home,
        &cwd,
        crate::provider_sources::DiscoveryPlatform::Linux,
        crate::provider_sources::DiscoveryPlatformDirs::default(),
    );
    let inventory =
        discover_hermes_source_backed(crate::test_provider_sqlite_data_root(), &context).unwrap();
    assert!(inventory.issues.is_empty());
    assert_eq!(
        inventory
            .sources
            .iter()
            .map(|source| source.path().to_path_buf())
            .collect::<Vec<_>>(),
        vec![
            root.join("state.db"),
            root.join("profiles/alpha/state.db"),
            root.join("profiles/zeta/state.db"),
        ]
    );

    let mut source_ids = Vec::new();
    for candidate in &inventory.sources {
        assert_eq!(candidate.status(), ProviderSourceStatus::Available);
        let (certified, records) = scan_candidate(candidate);
        assert_eq!(
            certified.counts(),
            ScannedSourceCounts {
                complete_records: 3,
                retained_records: 3,
                rejected_records: 0,
                ignored_records: 0,
                indexed_documents: 1,
                certified_bytes: certified.counts().certified_bytes,
            }
        );
        source_ids.push(candidate.source().identity());
        let event = event(&records);
        assert!(event.body.contains("gateway exact sentinel"));
        assert_eq!(event.session_id, child_session(&records).session_id);
        assert_eq!(
            event.parent_session_id,
            child_session(&records).parent_session_id
        );
        assert_eq!(event.root_session_id, root_session(&records).session_id);
        assert_eq!(
            event.provider_session_id,
            Some(child_session(&records).provider_session_id.clone())
        );
        assert!(event.branch.as_deref().unwrap().starts_with("branch-"));
        assert_eq!(event.source_path.as_deref(), candidate.path().to_str());
        assert_eq!(event.agent_type, "subagent");
        assert!(!event.is_primary);
        assert!(event.workspace.as_deref().unwrap().starts_with("/repo/"));
        let hydrated = hydrate_hermes_source_backed_message(
            crate::test_provider_sqlite_data_root(),
            candidate.path(),
            &event.locator,
        )
        .unwrap();
        assert_eq!(
            hydrated.provider_session_id,
            event.provider_session_id.as_deref().unwrap()
        );
        assert_eq!(hydrated.provider_event_hash, "message:7");
        assert!(hydrated.text.contains("gateway exact sentinel"));
        assert_eq!(hydrated.provider_bytes, hydrated.text.as_bytes());
    }
    source_ids.sort_by_key(|identity| identity.as_uuid());
    source_ids.dedup();
    assert_eq!(source_ids.len(), 3);
}

#[test]
fn hermes_source_backed_inactive_profiles_remain_explicit_only() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let root = home.join(".hermes");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("active_profile"), "active\n").unwrap();
    create_state_db(
        &root.join("profiles/active/state.db"),
        "active",
        "active exact sentinel",
    );
    let inactive_path = root.join("profiles/inactive/state.db");
    create_state_db(&inactive_path, "inactive", "inactive explicit sentinel");

    let context = DiscoveryContext::new(
        &home,
        &cwd,
        crate::provider_sources::DiscoveryPlatform::Linux,
        crate::provider_sources::DiscoveryPlatformDirs::default(),
    );
    let inventory =
        discover_hermes_source_backed(crate::test_provider_sqlite_data_root(), &context).unwrap();
    assert_eq!(inventory.sources.len(), 1);
    assert_eq!(
        inventory.sources[0].selection(),
        &HermesSourceSelection::NamedProfile("active".to_owned())
    );
    assert_eq!(
        inventory.sources[0].path(),
        root.join("profiles/active/state.db")
    );

    let explicit = hermes_source_backed_explicit(
        crate::test_provider_sqlite_data_root(),
        &inactive_path,
        SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8("inactive").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(explicit.selection(), &HermesSourceSelection::Explicit);
    let (_, records) = scan_candidate(&explicit);
    let event = event(&records);
    assert_eq!(event.body, "inactive explicit sentinel");
    assert_eq!(
        hydrate_hermes_source_backed_message(
            crate::test_provider_sqlite_data_root(),
            explicit.path(),
            &event.locator
        )
        .unwrap()
        .text,
        "inactive explicit sentinel"
    );
}

#[test]
fn hermes_source_backed_replacement_preserves_ids_and_rejects_stale_exact_coordinates() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_state_db(&path, "replacement", "before replacement sentinel");
    let candidate = hermes_source_backed_explicit(
        crate::test_provider_sqlite_data_root(),
        &path,
        SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8("replacement").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let (before_certificate, before_records) = scan_candidate(&candidate);
    let before_event = event(&before_records).clone();
    let before_child = child_session(&before_records).clone();
    assert_eq!(
        hydrate_hermes_source_backed_message(
            crate::test_provider_sqlite_data_root(),
            &path,
            &before_event.locator
        )
        .unwrap()
        .text,
        "before replacement sentinel"
    );

    fs::remove_file(&path).unwrap();
    create_state_db(&path, "replacement", "after replacement exact sentinel");
    let (after_certificate, after_records) = scan_candidate(&candidate);
    let after_event = event(&after_records);
    let after_child = child_session(&after_records);

    assert_eq!(before_event.event_id, after_event.event_id);
    assert_eq!(before_event.session_id, after_event.session_id);
    assert_eq!(before_child.session_id, after_child.session_id);
    assert_eq!(
        before_child.parent_session_id,
        after_child.parent_session_id
    );
    assert!(
        before_certificate.observation().revision() != after_certificate.observation().revision()
            || before_certificate.content_digest() != after_certificate.content_digest()
    );
    assert!(matches!(
        hydrate_hermes_source_backed_message(
            crate::test_provider_sqlite_data_root(),
            &path,
            &before_event.locator
        )
        .unwrap_err(),
        HermesSourceBackedError::StaleSourceEvidence | HermesSourceBackedError::StaleRecordEvidence
    ));
    assert_eq!(
        hydrate_hermes_source_backed_message(
            crate::test_provider_sqlite_data_root(),
            &path,
            &after_event.locator
        )
        .unwrap()
        .text,
        "after replacement exact sentinel"
    );

    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = after_event.locator.coordinate()
    else {
        panic!("expected Hermes SQLite coordinate");
    };
    assert_eq!(logical_relation, HERMES_MESSAGE_RELATION);
    assert_eq!(
        primary_key,
        &TypedKey::Composite(vec![
            TypedKey::Utf8("replacement-child".to_owned()),
            TypedKey::I64(7),
        ])
    );
    assert!(matches!(
        row_version,
        Some(TypedKey::Bytes(digest)) if digest.as_slice() == after_event.locator.record_digest()
    ));
    assert_eq!(
        after_event.source.provider(),
        CaptureProvider::Hermes.as_str()
    );
}
