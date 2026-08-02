use std::{collections::BTreeSet, fs, path::Path};

use ctx_history_core::{CaptureProvider, SourceAnchor};
use ctx_history_index::{GenerationWriter, SourceRouteSnapshot, VerifiedIndex, WriterOptions};
use rusqlite::Connection;

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, refresh_source_backed_generation_with_progress,
        SourceBackedProviderRegistry,
    },
    provider_sources::provider_source_for_path,
    register_hermes_explicit_source_backed_route,
};

#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [
        include_str!("../source_backed.rs"),
        include_str!("replacement.rs"),
    ];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("HERMES_SOURCE_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("native.complete_text"));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
}

fn provider_family_bytes(path: &Path) -> Vec<(String, Vec<u8>)> {
    [path.to_path_buf(), path.with_extension("db-wal")]
        .into_iter()
        .filter(|member| member.exists())
        .map(|member| {
            (
                member.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read(member).unwrap(),
            )
        })
        .collect()
}

fn provider_directory_names(path: &Path) -> Vec<String> {
    let mut names = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn projected_event_bodies(
    candidate: &HermesSourceCandidate,
    snapshot: &SqliteSourceReadSnapshot,
) -> Vec<String> {
    let mut bodies = Vec::new();
    project_hermes_snapshot(candidate, snapshot.connection().unwrap(), &mut |page| {
        for record in page.records {
            if let HermesSourceBackedRecord::Event(event) = record {
                bodies.push(event.content.normalized_body.unwrap_or_default());
            }
        }
        Ok(())
    })
    .unwrap();
    bodies
}

fn create_context_fixture(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    Connection::open(path)
        .unwrap()
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 parent_session_id text,
                 model_config text,
                 started_at real not null,
                 cwd text,
                 git_branch text,
                 git_repo_root text,
                 title text
             );
             create table messages (
                 id integer primary key autoincrement,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null
             );",
        )
        .unwrap();
}

#[test]
fn ordinary_parent_and_project_context_matches_direct_projection() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let path = temp.path().join("profile/state.db");
    create_context_fixture(&path);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "insert into sessions
             (id, source, parent_session_id, started_at, cwd, git_branch, git_repo_root)
             values ('root', 'acp', null, 1782259200.0, '/repo/root', 'main', '/repo')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "insert into sessions
             (id, source, parent_session_id, started_at, cwd, git_branch, git_repo_root)
             values ('child', 'acp', 'root', 1782259201.0, '/repo/child', 'feature', '/repo')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "insert into messages (session_id, role, content, timestamp)
             values ('root', 'assistant', 'root message', 1782259202.0),
                    ('child', 'assistant', 'child message', 1782259203.0)",
            [],
        )
        .unwrap();
    drop(connection);
    let before = fs::read(&path).unwrap();
    let candidate = HermesSourceCandidate::automatic(
        &data_root,
        provider_source_for_path(CaptureProvider::Hermes, path.clone()),
    )
    .unwrap();
    let (_authority, snapshot) = open_root_authorized_snapshot(&data_root, &path).unwrap();
    let mut events = Vec::new();
    let scan = project_hermes_snapshot(&candidate, snapshot.connection().unwrap(), &mut |page| {
        events.extend(page.records.into_iter().filter_map(|record| match record {
            HermesSourceBackedRecord::Event(event) => Some(event),
            HermesSourceBackedRecord::Session(_) | HermesSourceBackedRecord::Rejected(_) => None,
        }));
        Ok(())
    })
    .unwrap();
    snapshot.finish().unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(events.len(), 2);
    let root = events
        .iter()
        .find(|event| event.provider_session_id.as_deref() == Some("root"))
        .unwrap();
    let child = events
        .iter()
        .find(|event| event.provider_session_id.as_deref() == Some("child"))
        .unwrap();
    assert_eq!(root.root_session_id, root.session_id);
    assert_eq!(root.parent_session_id, None);
    assert!(root.is_primary);
    assert_eq!(root.branch.as_deref(), Some("main"));
    assert_eq!(root.workspace.as_deref(), Some("/repo"));
    assert_eq!(root.cwd.as_deref(), Some("/repo/root"));
    assert_eq!(child.root_session_id, root.session_id);
    assert_eq!(child.parent_session_id, Some(root.session_id));
    assert!(!child.is_primary);
    assert_eq!(child.branch.as_deref(), Some("feature"));
    assert_eq!(child.workspace.as_deref(), Some("/repo"));
    assert_eq!(child.cwd.as_deref(), Some("/repo/child"));
    assert!(scan.max_context_query_batches_per_page <= 2);
    assert!(scan.max_ancestry_rows_per_query <= ancestry::HERMES_ANCESTRY_QUERY_MAX_ROWS as u64);
    assert!(scan.peak_context_cache_rows <= ancestry::HERMES_CONTEXT_CACHE_MAX_ROWS as u64);
    assert!(scan.peak_context_cache_bytes <= ancestry::HERMES_CONTEXT_CACHE_MAX_BYTES as u64);
}

fn insert_context_session(
    statement: &mut rusqlite::Statement<'_>,
    id: &str,
    parent: Option<&str>,
    direct_metadata: Option<&str>,
    opaque_metadata: &str,
) {
    statement
        .execute(rusqlite::params![
            id,
            parent,
            opaque_metadata,
            direct_metadata,
            direct_metadata,
            direct_metadata,
            opaque_metadata,
        ])
        .unwrap();
}

#[test]
fn disjoint_and_deep_ancestry_streams_with_bounded_memory_and_queries() {
    const DISJOINT_CHAINS: usize = 128;
    const DISJOINT_DEPTH: usize = 5;
    const CACHE_ROOTS: usize = 300;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("profile/state.db");
    create_context_fixture(&path);
    let mut connection = Connection::open(&path).unwrap();
    let transaction = connection.transaction().unwrap();
    let large_opaque_metadata = "opaque".repeat(2_731);
    let large_direct_metadata = "context".repeat(2_341);
    let large_ancestry_key_tail = "k".repeat(ancestry::HERMES_SESSION_KEY_MAX_BYTES / 2);
    {
        let mut insert = transaction
            .prepare(
                "insert into sessions
                 (id, source, parent_session_id, model_config, started_at, cwd,
                  git_branch, git_repo_root, title)
                 values (?1, 'acp', ?2, ?3, 1782259200.0, ?4, ?5, ?6, ?7)",
            )
            .unwrap();
        let mut parent = None;
        for depth in 0..ancestry::HERMES_PARENT_CHAIN_MAX_DEPTH {
            let id = format!("deep-{depth:03}");
            insert_context_session(
                &mut insert,
                &id,
                parent.as_deref(),
                None,
                &large_opaque_metadata,
            );
            parent = Some(id);
        }
        insert_context_session(
            &mut insert,
            "deep-leaf",
            parent.as_deref(),
            Some(&large_direct_metadata),
            &large_opaque_metadata,
        );
        let mut parent = None;
        for depth in 0..=ancestry::HERMES_PARENT_CHAIN_MAX_DEPTH {
            let id = format!("overdeep-{depth:03}");
            insert_context_session(
                &mut insert,
                &id,
                parent.as_deref(),
                None,
                &large_opaque_metadata,
            );
            parent = Some(id);
        }
        insert_context_session(
            &mut insert,
            "overdeep-leaf",
            parent.as_deref(),
            Some(&large_direct_metadata),
            &large_opaque_metadata,
        );
        let mut parent = None;
        for depth in 0..140 {
            let id = format!("byte-{depth:03}-{large_ancestry_key_tail}");
            insert_context_session(&mut insert, &id, parent.as_deref(), None, "");
            parent = Some(id);
            if depth == 126 {
                insert_context_session(
                    &mut insert,
                    "near-byte-leaf",
                    parent.as_deref(),
                    Some(&large_direct_metadata),
                    "",
                );
            }
        }
        insert_context_session(
            &mut insert,
            "byte-leaf",
            parent.as_deref(),
            Some(&large_direct_metadata),
            &large_opaque_metadata,
        );
        for chain in 0..DISJOINT_CHAINS {
            let mut parent = None;
            for depth in 0..DISJOINT_DEPTH {
                let id = format!("wide-{chain:03}-{depth}");
                insert_context_session(
                    &mut insert,
                    &id,
                    parent.as_deref(),
                    None,
                    &large_opaque_metadata,
                );
                parent = Some(id);
            }
            insert_context_session(
                &mut insert,
                &format!("wide-leaf-{chain:03}"),
                parent.as_deref(),
                Some(&large_direct_metadata),
                &large_opaque_metadata,
            );
        }
        for root in 0..CACHE_ROOTS {
            insert_context_session(
                &mut insert,
                &format!("cache-root-{root:03}"),
                None,
                None,
                "",
            );
        }
    }
    transaction
        .execute_batch(
            "insert into sessions
             (id, source, parent_session_id, started_at, cwd)
             values ('projection-bad-parent', 'acp', null, 1782259200.0, x'ff');
             insert into sessions
             (id, source, parent_session_id, started_at, cwd)
             values ('projection-child', 'acp', 'projection-bad-parent', 1782259200.0, '/repo');
             insert into sessions
             (id, source, parent_session_id, started_at)
             values ('cycle-a', 'acp', 'cycle-b', 1782259200.0),
                    ('cycle-b', 'acp', 'cycle-a', 1782259200.0),
                    ('cycle-leaf', 'acp', 'cycle-a', 1782259200.0);",
        )
        .unwrap();
    transaction.commit().unwrap();
    drop(connection);
    let source_bytes = fs::read(&path).unwrap();
    let connection = crate::provider::sqlite::open_provider_sqlite_readonly(
        crate::test_provider_sqlite_data_root(),
        &path,
    )
    .unwrap();
    let schema = HermesSchema::detect(&connection).unwrap();
    let source = provider_source_for_path(CaptureProvider::Hermes, path.clone());
    let candidate =
        HermesSourceCandidate::automatic(crate::test_provider_sqlite_data_root(), source).unwrap();
    let mut near_limit_memo =
        HermesSessionContextMemo::new(&connection, &schema, &candidate.source);
    let near_limit = near_limit_memo
        .resolve_page(&BTreeSet::from(["near-byte-leaf".to_owned()]))
        .unwrap();
    assert!(matches!(
        near_limit.get("near-byte-leaf"),
        Some(HermesSessionResolution::Context(_))
    ));
    let near_limit_counters = near_limit_memo.counters();
    assert!(
        near_limit_counters.max_ancestry_bytes_per_query
            > (ancestry::HERMES_ANCESTRY_WORKSET_MAX_BYTES
                - ancestry::HERMES_ANCESTRY_ROW_MAX_BYTES) as u64
    );
    assert!(
        near_limit_counters.max_ancestry_bytes_per_query
            <= ancestry::HERMES_ANCESTRY_WORKSET_MAX_BYTES as u64
    );
    assert_eq!(ancestry::HERMES_ANCESTRY_TRAVERSAL_MAX_OWNED_BYTES, 0);
    drop(near_limit_memo);
    let mut memo = HermesSessionContextMemo::new(&connection, &schema, &candidate.source);

    let projection_page = BTreeSet::from([
        "projection-bad-parent".to_owned(),
        "projection-child".to_owned(),
    ]);
    let projection_resolutions = memo.resolve_page(&projection_page).unwrap();
    assert!(matches!(
        projection_resolutions.get("projection-bad-parent"),
        Some(HermesSessionResolution::Rejected(reason)) if reason.contains("cwd")
    ));
    let projection_root = match projection_resolutions.get("projection-child") {
        Some(HermesSessionResolution::Context(context)) => context.root_session_id,
        resolution => panic!("child lost independent ancestry evidence: {resolution:?}"),
    };

    let bounded_failures = memo
        .resolve_page(&BTreeSet::from([
            "byte-leaf".to_owned(),
            "cycle-leaf".to_owned(),
        ]))
        .unwrap();
    assert!(matches!(
        bounded_failures.get("byte-leaf"),
        Some(HermesSessionResolution::Rejected(reason)) if reason.contains("byte per-session")
    ));
    assert!(matches!(
        bounded_failures.get("cycle-leaf"),
        Some(HermesSessionResolution::Rejected(reason)) if reason.contains("cyclic parent chain")
    ));

    let deep = BTreeSet::from(["deep-leaf".to_owned()]);
    let deep_resolution = memo.resolve_page(&deep).unwrap();
    assert!(
        matches!(
            deep_resolution.get("deep-leaf"),
            Some(HermesSessionResolution::Context(_))
        ),
        "{deep_resolution:?}"
    );

    let mixed_page = BTreeSet::from(["cache-root-000".to_owned(), "overdeep-leaf".to_owned()]);
    let mixed_resolutions = memo.resolve_page(&mixed_page).unwrap();
    assert!(matches!(
        mixed_resolutions.get("cache-root-000"),
        Some(HermesSessionResolution::Context(_))
    ));
    assert!(matches!(
        mixed_resolutions.get("overdeep-leaf"),
        Some(HermesSessionResolution::Rejected(reason)) if reason.contains("256-row")
    ));

    for page in 0..2 {
        let leaves = (page * 64..(page + 1) * 64)
            .map(|chain| format!("wide-leaf-{chain:03}"))
            .collect::<BTreeSet<_>>();
        let resolutions = memo.resolve_page(&leaves).unwrap();
        assert_eq!(resolutions.len(), 64);
        assert!(resolutions
            .values()
            .all(|resolution| matches!(resolution, HermesSessionResolution::Context(_))));
    }

    for page in 0..5 {
        let roots = (page * 60..(page + 1) * 60)
            .map(|root| format!("cache-root-{root:03}"))
            .collect::<BTreeSet<_>>();
        let resolutions = memo.resolve_page(&roots).unwrap();
        assert!(resolutions
            .values()
            .all(|resolution| matches!(resolution, HermesSessionResolution::Context(_))));
    }
    let queries_before_revisit = memo.counters();
    let projection_revisit = memo
        .resolve_page(&BTreeSet::from(["projection-child".to_owned()]))
        .unwrap();
    assert!(matches!(
        projection_revisit.get("projection-child"),
        Some(HermesSessionResolution::Context(context))
            if context.root_session_id == projection_root
    ));
    let counters = memo.counters();
    assert_eq!(
        counters.direct_query_batches,
        queries_before_revisit.direct_query_batches + 1
    );
    assert_eq!(
        counters.ancestry_query_batches,
        queries_before_revisit.ancestry_query_batches + 1
    );
    assert_eq!(counters.direct_query_batches, 12);
    assert_eq!(counters.ancestry_query_batches, 134);
    assert_eq!(counters.max_query_batches_per_page, 65);
    assert_eq!(
        counters.max_ancestry_rows_per_query,
        ancestry::HERMES_ANCESTRY_QUERY_MAX_ROWS as u64
    );
    assert!(counters.max_direct_rows_per_query <= 64);
    assert!(
        counters.max_direct_bytes_per_query
            <= ancestry::HERMES_DIRECT_CONTEXT_RESIDENT_MAX_BYTES as u64
    );
    assert!(
        counters.max_ancestry_bytes_per_query > ancestry::HERMES_ANCESTRY_WORKSET_MAX_BYTES as u64
    );
    assert!(
        counters.max_ancestry_bytes_per_query
            <= ancestry::HERMES_ANCESTRY_RESIDENT_MAX_BYTES as u64
    );
    assert_eq!(
        counters.peak_cache_rows,
        ancestry::HERMES_CONTEXT_CACHE_MAX_ROWS as u64
    );
    assert!(counters.peak_cache_bytes <= ancestry::HERMES_CONTEXT_CACHE_MAX_BYTES as u64);
    drop(memo);
    drop(connection);
    assert_eq!(fs::read(path).unwrap(), source_bytes);
}

#[test]
fn online_backup_stays_stable_across_later_wal_commit_and_next_open_sees_it() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let path = temp.path().join("profile/state.db");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 parent_session_id text,
                 started_at real not null
             );
             create table messages (
                 id integer primary key autoincrement,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null
             );
             insert into sessions values ('session-1', 'acp', null, 1782259200.0);
             insert into messages (session_id, role, content, timestamp)
                 values ('session-1', 'assistant', 'admitted message', 1782259201.0);",
        )
        .unwrap();
    let candidate = HermesSourceCandidate::automatic(
        &data_root,
        provider_source_for_path(CaptureProvider::Hermes, path.clone()),
    )
    .unwrap();

    let names_before = provider_directory_names(&path);
    let bytes_before = provider_family_bytes(&path);
    let (authority, snapshot) = open_root_authorized_snapshot(&data_root, &path).unwrap();
    assert_eq!(
        authority.snapshot_counters().logical_online_backup_opens(),
        1
    );
    assert_eq!(
        projected_event_bodies(&candidate, &snapshot),
        vec!["admitted message"]
    );
    let terminal = snapshot.terminal_revalidator();
    snapshot.finish().unwrap();
    terminal().unwrap();
    assert_eq!(provider_directory_names(&path), names_before);
    assert_eq!(provider_family_bytes(&path), bytes_before);

    let (_authority, snapshot) = open_root_authorized_snapshot_with_hook(&data_root, &path, || {
        writer
            .execute(
                "insert into messages (session_id, role, content, timestamp)
                     values ('session-1', 'assistant', 'later message', 1782259202.0)",
                [],
            )
            .unwrap();
    })
    .unwrap();
    assert_eq!(
        projected_event_bodies(&candidate, &snapshot),
        vec!["admitted message"]
    );
    let terminal = snapshot.terminal_revalidator();
    snapshot.finish().unwrap();
    terminal().unwrap();

    let (_authority, snapshot) = open_root_authorized_snapshot(&data_root, &path).unwrap();
    assert_eq!(
        projected_event_bodies(&candidate, &snapshot),
        vec!["admitted message", "later message"]
    );
    snapshot.finish().unwrap();
}

fn create_refresh_fixture(path: &Path, message_count: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 parent_session_id text,
                 started_at real not null
             );
             create table messages (
                 id integer primary key autoincrement,
                 session_id text not null,
                 role text not null,
                 content text,
                 timestamp real not null
             );
             insert into sessions values ('session-1', 'acp', null, 1782259200.0);",
        )
        .unwrap();
    let mut insert = connection
        .prepare(
            "insert into messages (session_id, role, content, timestamp)
             values ('session-1', 'assistant', ?1, ?2)",
        )
        .unwrap();
    for ordinal in 0..message_count {
        insert
            .execute((
                format!("fixture message {ordinal:05}"),
                1_782_259_201_f64 + ordinal as f64,
            ))
            .unwrap();
    }
}

fn fixture_registry(data_root: &Path, database: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_hermes_explicit_source_backed_route(
        &mut registry,
        provider_source_for_path(CaptureProvider::Hermes, database.to_path_buf()),
        data_root,
        SourceAnchor::CatalogLineage([0x48; 32]),
    )
    .unwrap();
    registry
}

fn fixture_writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

#[test]
fn unchanged_legacy_v1_observation_is_rescanned_and_replaced_by_v2() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    create_refresh_fixture(&database, 1);
    let source_bytes = provider_family_bytes(&database);
    let source_names = provider_directory_names(&database);
    let anchor = SourceAnchor::CatalogLineage([0x48; 32]);
    let candidate = hermes_source_backed_explicit(&data_root, &database, anchor).unwrap();
    let registry = fixture_registry(&data_root, &database);
    let route_identity = registry
        .routes()
        .next()
        .and_then(|route| route.route_identity.clone())
        .unwrap();
    let (_authority, snapshot) = open_root_authorized_snapshot(&data_root, &database).unwrap();
    let connection = snapshot.connection().unwrap();
    let sqlite_user_version = connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .unwrap();
    let schema_fingerprint = sqlite_schema_fingerprint(connection).unwrap();
    let schema_evidence = hermes_schema_evidence(sqlite_user_version, &schema_fingerprint);
    let mut legacy_records = Vec::new();
    let projection = project_hermes_snapshot(&candidate, connection, &mut |page| {
        legacy_records.extend(page.records.into_iter().filter_map(|record| match record {
            HermesSourceBackedRecord::Event(mut event) => {
                event.parser_revision = HERMES_LEGACY_SOURCE_PARSER_REVISION.to_owned();
                Some(event)
            }
            HermesSourceBackedRecord::Session(_) | HermesSourceBackedRecord::Rejected(_) => None,
        }));
        Ok(())
    })
    .unwrap();
    snapshot.finish().unwrap();
    assert_eq!(legacy_records.len(), 1);
    let stable_event_id = legacy_records[0].event_id;
    let projected = projection.certificate;
    assert_eq!(projected.parser_revision(), HERMES_SOURCE_PARSER_REVISION);
    let legacy_certificate = SqliteLogicalSnapshot::new(
        HERMES_LEGACY_SOURCE_PARSER_REVISION,
        &schema_evidence,
        *projected.content_digest(),
        projected.counts(),
    )
    .certify(candidate.source.clone())
    .unwrap();
    assert_ne!(legacy_certificate.observation(), projected.observation());

    let mut legacy_writer = GenerationWriter::open(&index_root, fixture_writer_options()).unwrap();
    legacy_writer
        .set_source_route_plan(BTreeSet::from([route_identity.clone()]), BTreeSet::new())
        .unwrap();
    legacy_writer
        .begin_source_route_stage(route_identity.clone())
        .unwrap();
    legacy_writer
        .begin_source(candidate.source.clone())
        .unwrap();
    for record in legacy_records {
        legacy_writer.add_core_record(record).unwrap();
    }
    legacy_writer
        .certify_source(legacy_certificate.clone())
        .unwrap();
    legacy_writer
        .finish_source_route_stage(&route_identity)
        .unwrap();
    legacy_writer
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route_identity.clone(),
            vec![candidate.source.clone()],
        )
        .unwrap()])
        .unwrap();
    let legacy_commit = legacy_writer.commit(|_| true).unwrap();
    assert_eq!(
        legacy_commit.manifest().sources[0].parser_revision(),
        HERMES_LEGACY_SOURCE_PARSER_REVISION
    );

    reset_logical_row_traversals();
    let upgraded =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(logical_row_traversals(), 1);
    assert_ne!(upgraded.commit.generation_id, legacy_commit.generation_id);
    assert!(upgraded.removals.is_empty());
    assert_eq!(upgraded.sources.len(), 1);
    assert_eq!(
        upgraded.sources[0].parser_revision(),
        HERMES_SOURCE_PARSER_REVISION
    );
    assert_eq!(upgraded.sources[0].observation(), projected.observation());
    assert_ne!(
        upgraded.sources[0].observation(),
        legacy_certificate.observation()
    );
    assert_eq!(
        upgraded.sources[0].content_digest(),
        legacy_certificate.content_digest()
    );
    assert_eq!(upgraded.sources[0].counts(), legacy_certificate.counts());
    let upgraded_record = VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(stable_event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(upgraded_record.event_id, stable_event_id);
    assert_eq!(
        upgraded_record.parser_revision,
        HERMES_SOURCE_PARSER_REVISION
    );
    assert_eq!(provider_family_bytes(&database), source_bytes);
    assert_eq!(provider_directory_names(&database), source_names);
}

#[test]
fn cold_and_noop_refresh_use_one_visible_logical_row_traversal() {
    const MESSAGES: u64 = 512;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let database = temp.path().join("source/state.db");
    create_refresh_fixture(&database, MESSAGES);
    let registry = fixture_registry(&data_root, &database);
    let source_names = provider_directory_names(&database);
    let source_bytes = provider_family_bytes(&database);

    reset_logical_row_traversals();
    let mut progress = Vec::new();
    let cold = refresh_source_backed_generation_with_progress(
        &index_root,
        &registry,
        fixture_writer_options(),
        |update| {
            progress.push(update);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(cold.sources[0].counts().complete_records, MESSAGES + 1);
    assert!(cold.sources[0].frontier().is_none());
    assert!(progress.iter().any(|update| {
        update.phase == "refreshing"
            && update.current_source.as_deref() == database.to_str()
            && update.completed_records == Some(0)
            && update.completed_bytes.is_some_and(|bytes| bytes > 0)
    }));
    let route_terminal = progress
        .iter()
        .rev()
        .find(|update| update.current_source.as_deref() == database.to_str())
        .expect("terminal Hermes route progress");
    assert_eq!(route_terminal.completed_records, Some(MESSAGES));
    assert_eq!(
        route_terminal.completed_bytes,
        Some(cold.sources[0].counts().certified_bytes)
    );
    let terminal = progress.last().expect("terminal Hermes progress");
    assert_eq!(terminal.phase, "committed");
    assert!(terminal.current_source.is_none());
    assert!(terminal.completed_records.is_none());
    assert!(terminal.completed_bytes.is_none());
    assert_eq!(provider_directory_names(&database), source_names);
    assert_eq!(provider_family_bytes(&database), source_bytes);

    reset_logical_row_traversals();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(provider_directory_names(&database), source_names);
    assert_eq!(provider_family_bytes(&database), source_bytes);

    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("vacuum").unwrap();
    drop(connection);
    let vacuumed_names = provider_directory_names(&database);
    let vacuumed_bytes = provider_family_bytes(&database);
    reset_logical_row_traversals();
    let physically_changed =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(
        physically_changed.commit.generation_id,
        cold.commit.generation_id
    );
    assert_eq!(physically_changed.sources, cold.sources);
    assert_eq!(provider_directory_names(&database), vacuumed_names);
    assert_eq!(provider_family_bytes(&database), vacuumed_bytes);

    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "insert into messages (session_id, role, content, timestamp)
             values ('session-1', 'assistant', 'changed message', 1782264200.0)",
            [],
        )
        .unwrap();
    drop(connection);
    reset_logical_row_traversals();
    let changed =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(logical_row_traversals(), 1);
    assert_ne!(changed.commit.generation_id, cold.commit.generation_id);
    assert_ne!(changed.sources, cold.sources);
    assert_eq!(changed.sources[0].counts().complete_records, MESSAGES + 2);

    reset_logical_row_traversals();
    let changed_noop =
        refresh_source_backed_generation(&index_root, &registry, fixture_writer_options()).unwrap();
    assert_eq!(logical_row_traversals(), 1);
    assert_eq!(
        changed_noop.commit.generation_id,
        changed.commit.generation_id
    );
    assert_eq!(changed_noop.sources, changed.sources);
}
