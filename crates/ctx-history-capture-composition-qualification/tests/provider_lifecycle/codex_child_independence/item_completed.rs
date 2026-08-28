use std::io::Cursor;

use super::*;

const NATIVE_SESSION_ID: &str = "019fb000-0000-7000-8000-0000000000a1";
const FIXTURE: &str = "tests/fixtures/provider-history/codex-paginated-item-completed.jsonl";
type RecordIdentity = (
    String,
    String,
    u64,
    Option<i64>,
    String,
    Option<String>,
    String,
    String,
);

fn fixture_bytes() -> Vec<u8> {
    fs::read(crate::test_support_paths::capture_repo_root().join(FIXTURE)).unwrap()
}

fn fixture_lines() -> Vec<Vec<u8>> {
    fixture_bytes()
        .split_inclusive(|byte| *byte == b'\n')
        .map(ToOwned::to_owned)
        .collect()
}

fn records_identity(index: &VerifiedIndex) -> Vec<RecordIdentity> {
    let mut records = records_for(index, NATIVE_SESSION_ID)
        .into_iter()
        .map(|record| {
            (
                record.event_id.to_string(),
                serde_json::to_string(&record.native_event_id).unwrap(),
                record.event_sequence,
                record.occurred_at_unix_ms,
                record.event_type,
                record.role,
                serde_json::to_string(&record.content).unwrap(),
                record.parser_revision,
            )
        })
        .collect::<Vec<_>>();
    records.sort();
    records
}

#[test]
fn paginated_item_completed_fixture_keeps_raw_messages_once_and_projects_turn_qualified_plans() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("paginated.jsonl");
    let index_root = temp.path().join("index");
    fs::write(&source, fixture_bytes()).unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut registry, &source);

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert!(cold.logical_source_failures.is_empty());
    assert_eq!(
        cold.record_rejections.total(),
        5,
        "known lifecycle-only, future unknown, malformed, and both duplicate-payload orders"
    );
    let classes = cold
        .record_rejections
        .rejections()
        .iter()
        .map(|rejection| rejection.class)
        .collect::<Vec<_>>();
    assert_eq!(
        classes
            .iter()
            .filter(|class| {
                **class
                    == ctx_history_capture_runtime::SourceBackedRecordRejectionClass::UnsupportedRecord
            })
            .count(),
        2
    );
    assert_eq!(
        classes
            .iter()
            .filter(|class| {
                **class
                    == ctx_history_capture_runtime::SourceBackedRecordRejectionClass::MalformedRecord
            })
            .count(),
        3
    );

    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let records = records_for(&index, NATIVE_SESSION_ID);
    assert_eq!(records.len(), 6, "four raw messages and two Plans");
    for marker in [
        "synthetic user one",
        "synthetic assistant one",
        "synthetic user two",
        "synthetic assistant two",
    ] {
        assert_eq!(
            records
                .iter()
                .filter(|record| record.content.normalized_body.as_deref() == Some(marker))
                .count(),
            1,
            "raw/completed overlap must retain one message for {marker}"
        );
    }
    let plans = records
        .iter()
        .filter(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.starts_with("synthetic plan"))
        })
        .collect::<Vec<_>>();
    assert_eq!(plans.len(), 2);
    assert_ne!(
        plans[0].event_id, plans[1].event_id,
        "Plan key includes turn_id"
    );
    assert!(plans.iter().all(|record| {
        record
            .content
            .structured_content
            .as_ref()
            .is_some_and(|content| {
                content.get("type").and_then(serde_json::Value::as_str) == Some("item_completed")
                    && content.get("item").is_some()
            })
    }));
    assert!(records
        .iter()
        .all(|record| record.parser_revision == CURRENT_PARSER_REVISION));
}

#[test]
fn paginated_item_completed_append_noop_and_cold_replay_are_equivalent() {
    let temp = tempdir().unwrap();
    let prefix_source = temp.path().join("prefix.jsonl");
    let cold_source = temp.path().join("cold.jsonl");
    let prefix_index = temp.path().join("prefix-index");
    let cold_index = temp.path().join("cold-index");
    let lines = fixture_lines();
    let split = 8;
    fs::write(&prefix_source, lines[..split].concat()).unwrap();
    fs::write(&cold_source, fixture_bytes()).unwrap();

    let mut prefix_registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut prefix_registry, &prefix_source);
    let prefix =
        refresh_source_backed_generation(&prefix_index, &prefix_registry, writer_options())
            .unwrap();
    assert!(prefix.failed_routes.is_empty());
    append_event_bytes(&prefix_source, &lines[split..].concat());
    let (appended, _) = incremental_refresh(&prefix_index, &prefix_registry, &prefix);
    assert!(appended.failed_routes.is_empty());
    let appended_rejections = appended
        .record_rejections
        .rejections()
        .iter()
        .map(|rejection| {
            (
                rejection.line_number,
                format!("{:?}", rejection.class),
                rejection.payload_type.clone(),
                rejection.detail.clone(),
            )
        })
        .collect::<Vec<_>>();
    let (noop, _) = incremental_refresh(&prefix_index, &prefix_registry, &appended);
    assert_eq!(noop.commit.generation_id, appended.commit.generation_id);

    let mut cold_registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut cold_registry, &cold_source);
    let cold =
        refresh_source_backed_generation(&cold_index, &cold_registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    let cold_rejections = cold
        .record_rejections
        .rejections()
        .iter()
        .map(|rejection| {
            (
                rejection.line_number,
                format!("{:?}", rejection.class),
                rejection.payload_type.clone(),
                rejection.detail.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(appended_rejections, cold_rejections);

    let appended_index = VerifiedIndex::open_pinned(&prefix_index).unwrap();
    let cold_index = VerifiedIndex::open_pinned(&cold_index).unwrap();
    assert_eq!(
        records_identity(&appended_index),
        records_identity(&cold_index)
    );
}

#[test]
fn paginated_item_completed_raw_and_zstd_rollouts_have_identical_semantics() {
    let temp = tempdir().unwrap();
    let raw_source = temp.path().join("raw.jsonl");
    let zstd_source = temp.path().join("compressed.jsonl.zst");
    let raw_index = temp.path().join("raw-index");
    let zstd_index = temp.path().join("compressed-index");
    let bytes = fixture_bytes();
    fs::write(&raw_source, &bytes).unwrap();
    fs::write(
        &zstd_source,
        zstd::stream::encode_all(Cursor::new(bytes), 1).unwrap(),
    )
    .unwrap();

    let mut raw_registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut raw_registry, &raw_source);
    let raw =
        refresh_source_backed_generation(&raw_index, &raw_registry, writer_options()).unwrap();
    assert!(raw.failed_routes.is_empty());
    let mut zstd_registry = SourceBackedProviderRegistry::new();
    add_explicit_route(&mut zstd_registry, &zstd_source);
    let zstd =
        refresh_source_backed_generation(&zstd_index, &zstd_registry, writer_options()).unwrap();
    assert!(zstd.failed_routes.is_empty());
    assert_eq!(
        raw.record_rejections.total(),
        zstd.record_rejections.total()
    );
    let raw_rejections = raw
        .record_rejections
        .rejections()
        .iter()
        .map(|rejection| {
            (
                rejection.line_number,
                format!("{:?}", rejection.class),
                rejection.payload_type.clone(),
                rejection.detail.clone(),
            )
        })
        .collect::<Vec<_>>();
    let zstd_rejections = zstd
        .record_rejections
        .rejections()
        .iter()
        .map(|rejection| {
            (
                rejection.line_number,
                format!("{:?}", rejection.class),
                rejection.payload_type.clone(),
                rejection.detail.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(raw_rejections, zstd_rejections);

    let raw_index = VerifiedIndex::open_pinned(&raw_index).unwrap();
    let zstd_index = VerifiedIndex::open_pinned(&zstd_index).unwrap();
    assert_eq!(records_identity(&raw_index), records_identity(&zstd_index));
}

fn append_event_bytes(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}
