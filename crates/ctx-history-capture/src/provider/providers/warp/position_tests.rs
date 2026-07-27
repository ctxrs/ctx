use super::*;

#[test]
fn v4_cursor_and_locator_bytes_are_stable() {
    let position = encode_warp_position(WarpKeyset {
        phase: WarpPhase::Tasks,
        next_ordinal: 65,
        rowid: 19,
        key_valid: false,
    })
    .unwrap();
    assert_eq!(position.kind(), WARP_POSITION_KIND);
    assert_eq!(
        position.value(),
        [4, 0, 0, 0, 0, 0, 0, 0, 65, 2, 0, 128, 0, 0, 0, 0, 0, 0, 19,]
    );
    let decoded = decode_warp_position(&position).unwrap().unwrap();
    assert_eq!(decoded.phase, WarpPhase::Tasks);
    assert_eq!(decoded.next_ordinal, 65);
    assert_eq!(decoded.rowid, 19);
    assert!(!decoded.key_valid);

    let locator = warp_locator(WarpPhase::Conversations, -1).unwrap();
    assert_eq!(locator.kind(), WARP_LOCATOR_KIND);
    assert_eq!(locator.value(), [1, 127, 255, 255, 255, 255, 255, 255, 255]);
}

#[test]
fn initial_and_legacy_cursor_contracts_are_stable() {
    let initial = initial_warp_position().unwrap();
    assert_eq!(initial.kind(), WARP_POSITION_KIND);
    assert_eq!(initial.value(), [0]);
    assert!(decode_warp_position(&initial).unwrap().is_none());

    let legacy = NativePosition::new("warp-conversation-task-keyset-v2", vec![0]).unwrap();
    let error = decode_warp_position(&legacy).unwrap_err();
    assert!(error
        .to_string()
        .contains("unexpected native-position kind"));
}
