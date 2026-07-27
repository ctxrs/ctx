use crate::provider::importer::provider_source_event_seq;
use uuid::Uuid;

#[test]
fn provider_source_event_seq_keeps_large_provider_indices_distinct() {
    let source_id = Uuid::parse_str("018fe2e4-2266-7000-8000-000000000001").unwrap();

    assert_ne!(
        provider_source_event_seq(source_id, 0),
        provider_source_event_seq(source_id, 1_048_576)
    );
    assert_eq!(
        provider_source_event_seq(source_id, 1_048_576) & 0xffff_ffff,
        1_048_576
    );
}
