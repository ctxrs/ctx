use super::super::position::{
    decode_goose_position, encode_goose_position, goose_locator, initial_goose_position,
    GooseCapturePhase, GooseKeyset,
};

#[test]
fn goose_positions_and_locators_keep_exact_keyset_bytes() {
    let initial = initial_goose_position().unwrap();
    assert_eq!(initial.kind(), "goose-logical-row-keyset-v3");
    assert_eq!(initial.value(), &[0]);

    let position = encode_goose_position(GooseKeyset {
        phase: GooseCapturePhase::Sessions,
        next_ordinal: 0x0102_0304_0506_0708,
        rowid: -2,
    })
    .unwrap();
    assert_eq!(position.kind(), "goose-logical-row-keyset-v3");
    assert_eq!(
        position.value(),
        &[1, 1, 2, 3, 4, 5, 6, 7, 8, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,]
    );
    let decoded = decode_goose_position(&position).unwrap().unwrap();
    assert_eq!(decoded.phase, GooseCapturePhase::Sessions);
    assert_eq!(decoded.next_ordinal, 0x0102_0304_0506_0708);
    assert_eq!(decoded.rowid, -2);

    let locator = goose_locator(GooseCapturePhase::Messages, 42).unwrap();
    assert_eq!(locator.kind(), "goose-logical-row-v3");
    assert_eq!(locator.value(), &[2, 0x80, 0, 0, 0, 0, 0, 0, 0x2a]);
}
