use std::{fs, path::Path};

use ctx_history_core::CaptureProvider;
use rusqlite::Connection;

use super::*;
use crate::provider_sources::provider_source_for_path;

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
