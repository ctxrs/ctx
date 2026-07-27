use super::super::position::{
    decode_nanoclaw_message_locator, decode_nanoclaw_position, encode_nanoclaw_position,
    nanoclaw_locator, nanoclaw_message_locator, NanoClawKeyset, NanoClawMessageSource,
    NanoClawPositionPhase,
};
use super::super::{NANOCLAW_LOCATOR_KIND, NANOCLAW_MESSAGE_LOCATOR_KIND, NANOCLAW_POSITION_KIND};

#[test]
fn position_and_locator_wire_bytes_remain_stable() {
    let position = encode_nanoclaw_position(NanoClawKeyset {
        next_ordinal: 7,
        phase: NanoClawPositionPhase::Messages,
        session_rowid: 42,
        message_source: Some(NanoClawMessageSource::Outbound),
        message_rowid: 9,
    })
    .unwrap();
    assert_eq!(position.kind(), NANOCLAW_POSITION_KIND);
    assert_eq!(
        position.value(),
        [1, 0, 0, 0, 0, 0, 0, 0, 7, 2, 128, 0, 0, 0, 0, 0, 0, 42, 2, 128, 0, 0, 0, 0, 0, 0, 9,]
    );
    let decoded = decode_nanoclaw_position(&position).unwrap().unwrap();
    assert_eq!(decoded.next_ordinal, 7);
    assert_eq!(decoded.phase, NanoClawPositionPhase::Messages);
    assert_eq!(decoded.session_rowid, 42);
    assert_eq!(
        decoded.message_source,
        Some(NanoClawMessageSource::Outbound)
    );
    assert_eq!(decoded.message_rowid, 9);

    let locator = nanoclaw_locator(Some(NanoClawMessageSource::Outbound), 9).unwrap();
    assert_eq!(locator.kind(), NANOCLAW_LOCATOR_KIND);
    assert_eq!(locator.value(), [2, 128, 0, 0, 0, 0, 0, 0, 9]);

    let message_locator = nanoclaw_message_locator(42, NanoClawMessageSource::Outbound, 9).unwrap();
    assert_eq!(message_locator.kind(), NANOCLAW_MESSAGE_LOCATOR_KIND);
    assert_eq!(
        message_locator.value(),
        [128, 0, 0, 0, 0, 0, 0, 42, 2, 128, 0, 0, 0, 0, 0, 0, 9,]
    );
    assert_eq!(
        decode_nanoclaw_message_locator(&message_locator).unwrap(),
        super::super::position::NanoClawMessageLocator {
            session_rowid: 42,
            source: NanoClawMessageSource::Outbound,
            message_rowid: 9,
        }
    );
}
