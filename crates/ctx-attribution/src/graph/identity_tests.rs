use super::GraphRecordId;

#[test]
fn record_identities_are_repeatable_and_length_delimited() {
    let first = GraphRecordId::from_parts("fact", [b"ab".as_slice(), b"c".as_slice()]);
    let again = GraphRecordId::from_parts("fact", [b"ab".as_slice(), b"c".as_slice()]);
    let differently_partitioned =
        GraphRecordId::from_parts("fact", [b"a".as_slice(), b"bc".as_slice()]);

    assert_eq!(first, again);
    assert_ne!(first, differently_partitioned);
    assert!(first.to_string().starts_with("fact_"));
}
