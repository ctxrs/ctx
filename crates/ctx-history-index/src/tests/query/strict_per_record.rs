use super::*;

const RECORD_BYTES: usize = 64 * 1_024;

fn budgets(records: usize) -> (CoreEventPageBudget, CoreEventPageBudget) {
    (
        CoreEventPageBudget::new(RECORD_BYTES * records, RECORD_BYTES * records),
        CoreEventPageBudget::new(RECORD_BYTES, RECORD_BYTES),
    )
}

fn indexed_records(source: &SourceKey, records: &[CoreRecord]) -> (TempDir, VerifiedIndex) {
    let temp = tempdir().unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record.clone()).unwrap();
    }
    writer
        .certify_source(certificate(source, 1, records.len() as u64))
        .unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();
    (temp, index)
}

#[test]
fn mixed_small_and_content_oversized_record_declines_before_any_stored_read_or_decode() {
    let source = source("strict-per-record-content.jsonl");
    let small = document(&source, 1, "small preview evidence");
    let oversized = document(&source, 2, &"x".repeat(RECORD_BYTES + 1));
    let (_temp, index) = indexed_records(&source, &[small.clone(), oversized.clone()]);
    let (aggregate, per_record) = budgets(2);

    crate::query::reset_stored_core_event_record_materializations();
    crate::query::reset_core_record_decodes();
    assert!(index
        .core_events_by_ids_with_strict_per_record_budget(
            &[small.event_id.as_uuid(), oversized.event_id.as_uuid()],
            2,
            aggregate,
            per_record,
        )
        .unwrap()
        .is_none());
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);
    assert_eq!(crate::query::core_record_decodes(), 0);
}

#[test]
fn small_body_large_metadata_record_is_declined_before_materialization_or_decode() {
    let source = source("strict-per-record-encoded.jsonl");
    let mut encoded_heavy = document(&source, 1, "small");
    encoded_heavy.branch = Some("\u{0001}".repeat(16 * 1_024));
    encoded_heavy.validate_contract().unwrap();
    let ordinary = document(&source, 2, "small");
    let encoded_bytes = encoded_heavy.encode_stored().unwrap().len();
    let aggregate_encoded_bytes = encoded_bytes + ordinary.encode_stored().unwrap().len();
    assert!(encoded_bytes > RECORD_BYTES);
    assert!(aggregate_encoded_bytes < 2 * RECORD_BYTES);
    let (_temp, index) = indexed_records(&source, &[encoded_heavy.clone(), ordinary.clone()]);
    let (aggregate, per_record) = budgets(2);

    crate::query::reset_stored_core_event_record_materializations();
    crate::query::reset_core_record_decodes();
    assert!(index
        .core_events_by_ids_with_strict_per_record_budget(
            &[
                encoded_heavy.event_id.as_uuid(),
                ordinary.event_id.as_uuid(),
            ],
            2,
            aggregate,
            per_record,
        )
        .unwrap()
        .is_none());
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);
    assert_eq!(crate::query::core_record_decodes(), 0);
}

#[test]
fn three_ordinary_records_pass_per_record_and_aggregate_limits_in_exact_order() {
    let source = source("strict-per-record-order.jsonl");
    let first = document(&source, 1, "first preview evidence");
    let second = document(&source, 2, "second preview evidence");
    let third = document(&source, 3, "third preview evidence");
    let (_temp, index) = indexed_records(&source, &[second.clone(), third.clone(), first.clone()]);
    let requested = [
        third.event_id.as_uuid(),
        first.event_id.as_uuid(),
        second.event_id.as_uuid(),
    ];
    let (aggregate, per_record) = budgets(3);

    crate::query::reset_stored_core_event_record_materializations();
    crate::query::reset_core_record_decodes();
    let batch = index
        .core_events_by_ids_with_strict_per_record_budget(
            &requested,
            requested.len(),
            aggregate,
            per_record,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        batch
            .items
            .iter()
            .map(|record| record.event_id.as_uuid())
            .collect::<Vec<_>>(),
        requested
    );
    assert_eq!(
        batch
            .items
            .iter()
            .map(|record| &record.core_record)
            .collect::<Vec<_>>(),
        vec![&third, &first, &second]
    );
    assert_eq!(crate::query::stored_core_event_record_materializations(), 3);
    assert_eq!(crate::query::core_record_decodes(), 3);
}
