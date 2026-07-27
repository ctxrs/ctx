use std::{fs, path::Path};

use ctx_history_core::CaptureProvider;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::fixtures::tempdir;
use crate::{
    ProviderSourceLocatorObservation, Store, StoreError, FINAL_SCHEMA_IDENTITY, SCHEMA_VERSION,
};

const SOURCE_ID: &str = "11111111-1111-7111-8111-111111111111";
const SESSION_ID: &str = "22222222-2222-7222-8222-222222222222";
const MESSAGE_ID: &str = "33333333-3333-7333-8333-333333333333";
const RESULT_ID: &str = "44444444-4444-7444-8444-444444444444";
const MESSAGE_CANARY: &str = "historical-release-message-canary";
const RESULT_CANARY: &str = "historical-release-raw-result-canary";
const SOURCE_PATH: &str = "/public-fixture/ctx/released-store/session.jsonl";
const SOURCE_IDENTITY: &str = "historical-release-source-identity";
const SOURCE_FORMAT: &str = "codex_session_jsonl";
const MACHINE_ID: &str = "historical-release-machine";
const CHECKSUMS: &str = include_str!("../../../testdata/released-stores/SHA256SUMS");

struct ReleasedFixture {
    release: &'static str,
    compressed_name: &'static str,
    uncompressed_name: &'static str,
    compressed: &'static [u8],
}

const RELEASED_FIXTURES: &[ReleasedFixture] = &[
    ReleasedFixture {
        release: "v0.24.0",
        compressed_name: "v0.24.0-work.sqlite.zst",
        uncompressed_name: "v0.24.0-work.sqlite",
        compressed: include_bytes!("../../../testdata/released-stores/v0.24.0-work.sqlite.zst"),
    },
    ReleasedFixture {
        release: "v0.25.0",
        compressed_name: "v0.25.0-work.sqlite.zst",
        uncompressed_name: "v0.25.0-work.sqlite",
        compressed: include_bytes!("../../../testdata/released-stores/v0.25.0-work.sqlite.zst"),
    },
];

fn id(value: &str) -> Uuid {
    Uuid::parse_str(value).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn expected_checksum(name: &str) -> &str {
    CHECKSUMS
        .lines()
        .find_map(|line| {
            let (digest, filename) = line.split_once("  ")?;
            (filename == name).then_some(digest)
        })
        .unwrap_or_else(|| panic!("missing checksum for {name}"))
}

fn assert_valid_sqlite(conn: &Connection, release: &str) {
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok", "{release} integrity check");
    let mut foreign_keys = conn.prepare("PRAGMA foreign_key_check").unwrap();
    assert!(
        foreign_keys.query([]).unwrap().next().unwrap().is_none(),
        "{release} foreign key check"
    );
}

fn table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
         )",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        != 0
}

fn assert_released_v46_evidence(path: &Path, release: &str) {
    let conn = Connection::open(path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 46, "{release} schema");
    assert!(!table_exists(&conn, "ctx_store_schema_identity"));
    assert!(!table_exists(&conn, "provider_source_locators"));
    assert!(!table_exists(&conn, "capture_source_provider_routes"));
    assert_valid_sqlite(&conn, release);

    let canonical_counts: (i64, i64, i64) = conn
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM capture_sources),
                 (SELECT COUNT(*) FROM sessions),
                 (SELECT COUNT(*) FROM events)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(canonical_counts, (1, 1, 2), "{release} canonical rows");

    let raw_result: String = conn
        .query_row(
            "SELECT payload_json FROM events WHERE id = ?1",
            [RESULT_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert!(raw_result.contains(RESULT_CANARY), "{release} raw result");
    let raw_search: String = conn
        .query_row(
            "SELECT preview_text FROM event_search WHERE event_id = ?1",
            [RESULT_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert!(raw_search.contains(RESULT_CANARY), "{release} raw search");
    let verified_locators: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events
             WHERE json_type(metadata_json, '$.verified_content_locators_v1')
                   IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(verified_locators, 0, "{release} predates locator metadata");
}

fn assert_current_upgrade(path: &Path, release: &str) {
    let store = Store::open(path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION, "{release} upgraded schema");
    let identity: String = store
        .conn
        .query_row(
            "SELECT schema_identity FROM ctx_store_schema_identity
             WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(identity, FINAL_SCHEMA_IDENTITY, "{release} final identity");
    let foreign_keys: i64 = store
        .conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 1, "{release} foreign keys enabled");
    assert_valid_sqlite(&store.conn, release);

    let source_id = id(SOURCE_ID);
    let source = store.get_capture_source(source_id).unwrap();
    assert_eq!(source.descriptor.provider, CaptureProvider::Codex);
    assert_eq!(source.descriptor.machine_id, MACHINE_ID);
    assert_eq!(
        source.descriptor.source_format.as_deref(),
        Some(SOURCE_FORMAT)
    );
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        Some(SOURCE_PATH)
    );
    assert_eq!(
        source.descriptor.source_identity.as_deref(),
        Some(SOURCE_IDENTITY)
    );

    let session = store.get_session(id(SESSION_ID)).unwrap();
    assert_eq!(session.capture_source_id, Some(source_id));
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("historical-release-session")
    );
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 2, "{release} event count");
    let message = events
        .iter()
        .find(|event| event.id == id(MESSAGE_ID))
        .unwrap();
    assert_eq!(
        message
            .payload
            .get("text")
            .and_then(serde_json::Value::as_str),
        Some(MESSAGE_CANARY)
    );
    let result = events
        .iter()
        .find(|event| event.id == id(RESULT_ID))
        .unwrap();
    let result_json = serde_json::to_string(&result.payload).unwrap();
    assert!(
        !result_json.contains(RESULT_CANARY),
        "{release} result body"
    );
    assert_eq!(
        result
            .payload
            .pointer("/body/result_outcome")
            .and_then(serde_json::Value::as_str),
        Some("failure")
    );
    assert_eq!(
        result
            .payload
            .pointer("/body/exit_code")
            .and_then(serde_json::Value::as_i64),
        Some(7)
    );

    let message_hits = store.search_event_hits(MESSAGE_CANARY, 10).unwrap();
    assert_eq!(message_hits.len(), 1, "{release} message search");
    assert_eq!(message_hits[0].event_id, message.id);
    assert!(
        store
            .search_event_hits(RESULT_CANARY, 10)
            .unwrap()
            .is_empty(),
        "{release} stale result search"
    );
    let archive = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
    assert!(archive.contains(MESSAGE_CANARY));
    assert!(!archive.contains(RESULT_CANARY));

    // v0.24/v0.25 did not persist v0.26 locator authority. The migration must
    // fail closed rather than inventing it from the historical raw path.
    assert!(matches!(
        store.authorized_source_route_for_event(message.id),
        Err(StoreError::AuthorizedSourceRouteUnavailable { .. })
    ));
    let observation = ProviderSourceLocatorObservation {
        provider: CaptureProvider::Codex,
        source_format: SOURCE_FORMAT.to_owned(),
        machine_id: MACHINE_ID.to_owned(),
        locator_identity: format!("{release}-historical-fixture-locator"),
        cursor_stream: format!("{release}-historical-fixture-cursor"),
        proposed_source_identity: SOURCE_IDENTITY.to_owned(),
        raw_source_path: Some(SOURCE_PATH.to_owned()),
        source_revision: format!("{release}-historical-fixture-revision"),
        observed_at_ms: 1_788_290_400_000,
    };
    let resolution = store
        .reconcile_provider_source_locator(&observation)
        .unwrap();
    assert_eq!(resolution.canonical_source_identity, SOURCE_IDENTITY);
    store
        .bind_capture_source_provider_route(source_id, &resolution.route_binding())
        .unwrap();
    let route = store.authorized_source_route_for_event(message.id).unwrap();
    assert_eq!(route.event_id(), message.id);
    assert_eq!(route.capture_source_id(), source_id);
    assert_eq!(route.provider(), CaptureProvider::Codex);
    assert_eq!(route.source_format(), SOURCE_FORMAT);
    assert_eq!(route.machine_id(), MACHINE_ID);
    assert_eq!(route.canonical_source_identity(), SOURCE_IDENTITY);
    assert_eq!(route.path(), Path::new(SOURCE_PATH));
    assert_eq!(route.source_revision(), observation.source_revision);
    drop(store);

    let reopened = Store::open(path).unwrap();
    assert_eq!(
        reopened
            .authorized_source_route_for_event(id(MESSAGE_ID))
            .unwrap()
            .path(),
        Path::new(SOURCE_PATH)
    );
    assert_valid_sqlite(&reopened.conn, release);
}

#[test]
fn immutable_v024_and_v025_stores_upgrade_to_current_v47() {
    for fixture in RELEASED_FIXTURES {
        assert_eq!(
            sha256(fixture.compressed),
            expected_checksum(fixture.compressed_name),
            "{} compressed checksum",
            fixture.release
        );
        let sqlite = zstd::stream::decode_all(fixture.compressed).unwrap();
        assert_eq!(
            sha256(&sqlite),
            expected_checksum(fixture.uncompressed_name),
            "{} uncompressed checksum",
            fixture.release
        );
        let temp = tempdir();
        let path = temp.path().join(fixture.uncompressed_name);
        fs::write(&path, sqlite).unwrap();
        assert_released_v46_evidence(&path, fixture.release);
        assert_current_upgrade(&path, fixture.release);
    }
}
