pub(super) fn goose_message_locator(rowid: i64) -> (&'static str, Vec<u8>) {
    let mut value = Vec::with_capacity(9);
    value.push(2);
    value.extend_from_slice(&goose_ordered_i64(rowid).to_be_bytes());
    ("goose-messages-native-id-v4", value)
}

fn goose_ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}
