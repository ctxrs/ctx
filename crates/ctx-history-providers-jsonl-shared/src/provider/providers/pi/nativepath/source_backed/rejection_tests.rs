use super::*;
use ctx_history_capture_runtime::MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS;

fn rejections(path: &str) -> JsonlRecordRejections {
    JsonlRecordRejections::new(
        source_key("pi-rejection-session").unwrap(),
        CaptureProvider::Pi,
        path,
    )
}

#[test]
fn malformed_rows_report_locations_and_keep_valid_peers_projectable() {
    let mut rejections = rejections("/tmp/pi/session.jsonl");
    let rows: [&[u8]; 4] = [
        br#"{"type":"message","timestamp":"2026-09-02T00:00:00Z"}"#,
        br#"{"#,
        br#"{"type":"title","v":1,"title":"late"}"#,
        br#"{"type":"message","timestamp":"2026-09-02T00:00:01Z"}"#,
    ];
    let mut accepted = Vec::new();
    for (ordinal, row) in rows.into_iter().enumerate() {
        if let Some(value) = parse_pi_event_record(
            &mut rejections,
            JsonlRecordRef::for_test(row, ordinal as u64),
        ) {
            accepted.push(value);
        }
    }

    assert_eq!(accepted.len(), 2);
    assert_eq!(rejections.count(), 2);
    let (drafts, omitted) = rejections.take_drafts().into_parts();
    assert_eq!(omitted, 0);
    assert_eq!(
        drafts
            .iter()
            .map(|draft| draft.line_number)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    assert!(drafts[0].detail.contains("malformed Pi JSONL"));
    assert_eq!(
        drafts[1].detail,
        "Pi title record appears after the session header"
    );
}

#[test]
fn all_invalid_rows_keep_an_exact_count_with_bounded_details() {
    let mut rejections = rejections("/tmp/pi/all-invalid.jsonl");
    let rejected = MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS + 1;
    let accepted = (0..rejected)
        .filter_map(|ordinal| {
            parse_pi_event_record(
                &mut rejections,
                JsonlRecordRef::for_test(b"{", ordinal as u64),
            )
        })
        .count();

    assert_eq!(accepted, 0);
    assert_eq!(rejections.count(), rejected as u64);
    let (drafts, omitted) = rejections.take_drafts().into_parts();
    assert_eq!(drafts.len(), MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS);
    assert_eq!(omitted, 1);
    assert_eq!(drafts.len() + omitted, rejected);
}

#[test]
fn pre_header_rejections_retain_typed_diagnostics() {
    let source = source_key("pi-leading-rejection-session").unwrap();
    let path = Path::new("/tmp/pi/leading.jsonl");
    let details = vec![(
        1,
        leading_rejection_detail(JsonlRecordRef::for_test(b"{", 0)),
    )];
    let drafts = pi_leading_rejection_drafts(&source, path, details).unwrap();

    ensure_rejection_count(&drafts, 1, "test Pi rejection count mismatch").unwrap();
    let (recorded, omitted) = drafts.into_parts();
    assert_eq!(recorded.len(), 1);
    assert_eq!(omitted, 0);
    assert_eq!(recorded[0].line_number, 1);
    assert_eq!(recorded[0].source, source);
    assert_eq!(recorded[0].provider, CaptureProvider::Pi);
    assert_eq!(recorded[0].source_selector, path.display().to_string());
    assert_eq!(
        recorded[0].class,
        SourceBackedRecordRejectionClass::MalformedRecord
    );
    assert!(recorded[0]
        .detail
        .contains("malformed Pi JSONL before the session header"));
}
