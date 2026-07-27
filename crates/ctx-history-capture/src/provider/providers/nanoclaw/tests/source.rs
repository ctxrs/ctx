use super::super::position::initial_nanoclaw_position;
use super::super::rows::decode_nanoclaw_message_record;
use super::super::{NANOCLAW_MESSAGE_RECORD_KIND, NANOCLAW_SESSION_RECORD_KIND};
use super::*;
use crate::captured_batch::StructuralRejectionKind;

#[test]
fn phase_transitions_preserve_empty_sessions_and_direction_order() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "phase", 2);
    let (inbound, outbound) = create_message_stores(&root, "session-0001");
    insert_outbound(&outbound, "out-same", 7, 1_000, "assistant same");
    insert_inbound(&inbound, "in-same", 7, 1_000, "user same");
    insert_outbound(&outbound, "out-later", 8, 2_000, "assistant later");

    let batches = capture_batches(&root, initial_nanoclaw_position().unwrap());
    assert_eq!(
        record_kinds(&batches),
        vec![
            NANOCLAW_SESSION_RECORD_KIND,
            NANOCLAW_SESSION_RECORD_KIND,
            NANOCLAW_MESSAGE_RECORD_KIND,
            NANOCLAW_MESSAGE_RECORD_KIND,
            NANOCLAW_MESSAGE_RECORD_KIND,
        ]
    );
    let messages = batches
        .iter()
        .flat_map(|batch| batch.records())
        .filter(|record| record.record_kind().as_str() == NANOCLAW_MESSAGE_RECORD_KIND)
        .map(|record| {
            let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
                panic!("message record must contain SQLite values");
            };
            decode_nanoclaw_message_record(values).unwrap().0
        })
        .collect::<Vec<_>>();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.source, message.id.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("inbound", "in-same"),
            ("outbound", "out-same"),
            ("outbound", "out-later"),
        ]
    );
}

#[test]
fn sixty_five_logical_records_split_at_the_exact_boundary() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "boundary", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    for index in 0..64 {
        insert_inbound(
            &inbound,
            &format!("in-{index:04}"),
            index,
            10_000 + index,
            "bounded",
        );
    }

    let batches = capture_batches(&root, initial_nanoclaw_position().unwrap());
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(batches[1].records().len(), 1);
    assert_eq!(batches[0].range_end(), batches[1].range_before());
}

#[test]
fn replay_from_exact_positions_is_identical() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "replay", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    for index in 0..70 {
        insert_inbound(
            &inbound,
            &format!("in-{index:04}"),
            index,
            20_000 + index,
            "replay",
        );
    }

    let first_run = capture_batches(&root, initial_nanoclaw_position().unwrap());
    let replay = capture_batches(&root, initial_nanoclaw_position().unwrap());
    assert_eq!(first_run, replay);
    let resumed = capture_batches(&root, first_run[0].range_end().clone());
    assert_eq!(resumed, first_run[1..]);
}

#[test]
fn oversized_message_is_rejected_before_hydration() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "oversize", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(
        &inbound,
        "huge",
        1,
        1_000,
        &"x".repeat(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES + 1),
    );

    let batches = capture_batches(&root, initial_nanoclaw_position().unwrap());
    let oversized = batches
        .iter()
        .flat_map(|batch| batch.records())
        .find(|record| {
            matches!(
                record.payload(),
                CapturedRecordPayload::StructuralRejection {
                    kind: StructuralRejectionKind::OversizeRecord,
                    ..
                }
            )
        })
        .expect("oversized record should be represented as a structural rejection");
    assert_eq!(
        oversized.record_kind().as_str(),
        NANOCLAW_MESSAGE_RECORD_KIND
    );
}
