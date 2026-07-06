use super::*;
use serde::Serialize;

pub(crate) fn new_link_id(target_id: Uuid) -> Uuid {
    let mut bytes = *target_id.as_bytes();
    bytes[15] = bytes[15].wrapping_add(80);
    Uuid::from_bytes(bytes)
}

pub(crate) fn deterministic_tie_record(id: &str) -> HistoryRecord {
    let mut record = HistoryRecord::new(
        "Stable tie title",
        "stabletie exact equal body for deterministic ranking",
        vec!["stabletie".into()],
        "task",
        None,
    );
    record.id = Uuid::parse_str(id).unwrap();
    record.created_at = fixed_time();
    record.updated_at = fixed_time();
    record
}

pub(crate) fn packet_without_generated_at<T: Serialize>(packet: &T) -> serde_json::Value {
    let mut value = serde_json::to_value(packet).unwrap();
    value.as_object_mut().unwrap().remove("generated_at");
    value
}

#[test]
fn search_packet_terms_merges_broad_queries_without_requiring_all_terms() {
    let (_temp, store) = test_store();
    for (id, title, body) in [
        (
            "018f45d0-0000-7000-8000-000000020001",
            "Signed metadata release",
            "signed metadata verification and trusted release manifests",
        ),
        (
            "018f45d0-0000-7000-8000-000000020002",
            "Buildkite worker setup",
            "buildkite pipeline worker provisioning and release queue setup",
        ),
    ] {
        let mut record = HistoryRecord::new(title, body, Vec::new(), "task", None);
        record.id = Uuid::parse_str(id).unwrap();
        record.created_at = fixed_time();
        record.updated_at = fixed_time();
        store.insert_record(&record).unwrap();
    }
    let options = PacketOptions {
        limit: 10,
        snippet_chars: 160,
        ..PacketOptions::default()
    };

    let exact = search_packet(&store, "signed metadata buildkite", &options).unwrap();
    assert_eq!(exact.results.len(), 0);

    let broad = search_packet_terms(
        &store,
        "signed metadata",
        &[String::from("buildkite")],
        &options,
    )
    .unwrap();
    let titles = broad
        .results
        .iter()
        .map(|result| result.title.as_str())
        .collect::<Vec<_>>();
    assert!(titles.contains(&"Signed metadata release"));
    assert!(titles.contains(&"Buildkite worker setup"));
    assert_eq!(broad.query, "signed metadata OR buildkite");
}
