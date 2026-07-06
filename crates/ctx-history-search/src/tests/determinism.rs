use super::*;

#[test]
fn search_packet_is_deterministic_for_large_history_and_equal_ties_use_record_id() {
    let (_temp, store) = test_store();
    for id in [
        "018f45d0-0000-7000-8000-000000010004",
        "018f45d0-0000-7000-8000-000000010001",
        "018f45d0-0000-7000-8000-000000010003",
        "018f45d0-0000-7000-8000-000000010002",
    ] {
        store.insert_record(&deterministic_tie_record(id)).unwrap();
    }

    let expected_order = vec![
        Uuid::parse_str("018f45d0-0000-7000-8000-000000010001").unwrap(),
        Uuid::parse_str("018f45d0-0000-7000-8000-000000010002").unwrap(),
        Uuid::parse_str("018f45d0-0000-7000-8000-000000010003").unwrap(),
        Uuid::parse_str("018f45d0-0000-7000-8000-000000010004").unwrap(),
    ];
    let options = PacketOptions {
        limit: 10,
        snippet_chars: 160,
        ..PacketOptions::default()
    };

    let first_search = search_packet(&store, "stabletie", &options).unwrap();
    let second_search = search_packet(&store, "stabletie", &options).unwrap();
    assert_eq!(
        first_search
            .results
            .iter()
            .map(|result| result.record_id)
            .collect::<Vec<_>>(),
        expected_order
    );
    assert_eq!(
        packet_without_generated_at(&first_search),
        packet_without_generated_at(&second_search)
    );
}
