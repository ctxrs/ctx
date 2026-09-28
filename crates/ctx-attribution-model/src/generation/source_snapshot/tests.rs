use super::*;
use ctx_history_core::{SourceAnchor, SourceKey, TypedKey};

fn source(index: u64) -> CoreSourceState {
    CoreSourceState {
        source: SourceKey::derive(
            "inventory",
            "fixture",
            "v1",
            1,
            SourceAnchor::ProviderNative {
                namespace: "source".into(),
                key: TypedKey::U64(index),
            },
        )
        .unwrap(),
        core_record_accumulator: "0".repeat(64),
        event_count: index % 31,
    }
}

#[test]
fn streamed_inventory_matches_legacy_json_above_old_source_limit() {
    let mut sources = (0..20_003).map(source).collect::<Vec<_>>();
    sources.sort_by_key(|state| state.source.identity().digest());
    let expected = hex_sha256(Sha256::digest(serde_json::to_vec(&sources).unwrap()));
    let mut builder = CoreSourceSnapshotBuilder::default();
    for page in sources.chunks(137) {
        for source in page {
            builder.push(source).unwrap();
        }
    }
    let snapshot = builder.finish();
    assert_eq!(snapshot.sha256, expected);
    assert_eq!(snapshot.source_count, 20_003);
    assert_eq!(
        snapshot.event_count,
        (0..20_003_u64).map(|n| n % 31).sum::<u64>()
    );
    assert_eq!(
        CoreSourceSnapshotBuilder::default().finish().sha256,
        hex_sha256(Sha256::digest(b"[]"))
    );
}

#[test]
fn snapshot_rejects_duplicate_order_and_event_overflow() {
    let state = source(1);
    let mut builder = CoreSourceSnapshotBuilder::default();
    builder.push(&state).unwrap();
    assert_eq!(
        builder.push(&state).unwrap_err().class,
        ErrorClass::Sequence
    );
    let mut sources = [source(1), source(2)];
    sources.sort_by_key(|state| state.source.identity().digest());
    sources[0].event_count = u64::MAX;
    let mut builder = CoreSourceSnapshotBuilder::default();
    builder.push(&sources[0]).unwrap();
    assert_eq!(
        builder.push(&sources[1]).unwrap_err().class,
        ErrorClass::Bounds
    );
}
