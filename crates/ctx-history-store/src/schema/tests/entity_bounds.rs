use super::fixtures::{fixed_time, imported_session, provider_archive_source, tempdir};
use crate::Store;

#[test]
fn session_upsert_preserves_complete_temporal_bounds() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut session = imported_session("temporal-merge");
    let session_id = session.id;
    session.started_at = fixed_time() + chrono::Duration::minutes(10);
    session.ended_at = Some(fixed_time() + chrono::Duration::minutes(20));
    store.upsert_session(&session).unwrap();

    session.started_at = fixed_time();
    session.ended_at = Some(fixed_time() + chrono::Duration::minutes(5));
    store.upsert_session(&session).unwrap();
    session.started_at = fixed_time() + chrono::Duration::minutes(30);
    session.ended_at = Some(fixed_time() + chrono::Duration::minutes(40));
    store.upsert_session(&session).unwrap();

    let stored = store.get_session(session_id).unwrap();
    assert_eq!(stored.started_at, fixed_time());
    assert_eq!(
        stored.ended_at,
        Some(fixed_time() + chrono::Duration::minutes(40))
    );
}

#[test]
fn capture_source_upsert_extends_bounds_for_later_replay() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut source = provider_archive_source(
        "6f3e2871-6c37-4e48-b80d-a1933b6e7551",
        "later-replay",
        "/repo/later.jsonl",
    );
    let source_id = source.id;
    source.ended_at = Some(fixed_time() + chrono::Duration::minutes(10));
    store.upsert_capture_source(&source).unwrap();

    source.started_at = fixed_time() + chrono::Duration::minutes(5);
    source.ended_at = Some(fixed_time() + chrono::Duration::minutes(20));
    source.descriptor.machine_id = "replayed-machine".to_owned();
    source.sync.metadata = serde_json::json!({"replay": "later"});
    store.upsert_capture_source(&source).unwrap();

    let stored = store.get_capture_source(source_id).unwrap();
    assert_eq!(stored.started_at, fixed_time());
    assert_eq!(stored.ended_at, source.ended_at);
    assert_eq!(stored.descriptor, source.descriptor);
    assert_eq!(stored.sync, source.sync);
}

#[test]
fn capture_source_upsert_preserves_latest_end_for_earlier_replay() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut source = provider_archive_source(
        "08074caf-5124-4dc3-b940-4403bc684c94",
        "earlier-replay",
        "/repo/earlier.jsonl",
    );
    let source_id = source.id;
    source.started_at = fixed_time() + chrono::Duration::minutes(10);
    source.ended_at = Some(fixed_time() + chrono::Duration::minutes(30));
    store.upsert_capture_source(&source).unwrap();

    source.started_at = fixed_time();
    source.ended_at = Some(fixed_time() + chrono::Duration::minutes(20));
    store.upsert_capture_source(&source).unwrap();

    let stored = store.get_capture_source(source_id).unwrap();
    assert_eq!(stored.started_at, fixed_time());
    assert_eq!(
        stored.ended_at,
        Some(fixed_time() + chrono::Duration::minutes(30))
    );
}

#[test]
fn capture_source_upsert_keeps_non_null_end_across_null_replays() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut source = provider_archive_source(
        "5ef05895-fede-4999-b5cf-338f6442c4af",
        "null-replay",
        "/repo/null.jsonl",
    );
    let source_id = source.id;
    source.started_at = fixed_time() + chrono::Duration::minutes(10);
    store.upsert_capture_source(&source).unwrap();

    source.started_at = fixed_time() + chrono::Duration::minutes(5);
    source.ended_at = Some(fixed_time() + chrono::Duration::minutes(20));
    store.upsert_capture_source(&source).unwrap();

    source.started_at = fixed_time() + chrono::Duration::minutes(15);
    source.ended_at = None;
    store.upsert_capture_source(&source).unwrap();

    let stored = store.get_capture_source(source_id).unwrap();
    assert_eq!(
        stored.started_at,
        fixed_time() + chrono::Duration::minutes(5)
    );
    assert_eq!(
        stored.ended_at,
        Some(fixed_time() + chrono::Duration::minutes(20))
    );
}
