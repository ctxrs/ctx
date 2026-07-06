#[allow(unused_imports)]
use super::*;

pub(crate) fn msgpack_map_get<'a>(
    fields: &'a [(MsgpackValue, MsgpackValue)],
    key: &str,
) -> Option<&'a MsgpackValue> {
    fields.iter().find_map(|(field_key, field_value)| {
        (msgpack_string(field_key).as_deref() == Some(key)).then_some(field_value)
    })
}

pub(crate) fn msgpack_map_string(
    fields: &[(MsgpackValue, MsgpackValue)],
    key: &str,
) -> Option<String> {
    msgpack_map_get(fields, key).and_then(msgpack_string)
}

pub(crate) fn msgpack_string(value: &MsgpackValue) -> Option<String> {
    match value {
        MsgpackValue::String(text) => text.as_str().map(str::to_owned),
        _ => None,
    }
}
