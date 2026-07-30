pub(super) fn goose_message_locator(rowid: i64) -> (&'static str, Vec<u8>) {
    let mut value = Vec::with_capacity(9);
    value.push(2);
    value.extend_from_slice(&goose_ordered_i64(rowid).to_be_bytes());
    ("goose-messages-native-id-v4", value)
}

pub(super) fn decode_goose_message_locator(value: &[u8]) -> Option<i64> {
    if value.len() != 9 || value.first().copied() != Some(2) {
        return None;
    }
    let ordered = u64::from_be_bytes(value.get(1..)?.try_into().ok()?);
    Some((ordered ^ (1_u64 << 63)) as i64)
}

fn goose_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}
