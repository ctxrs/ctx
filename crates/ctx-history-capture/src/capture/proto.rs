#[allow(unused_imports)]
use super::*;

pub(crate) fn proto_nested_string_field_for_oneof(
    data: &[u8],
    outer_field: u32,
    inner_field: u32,
) -> Result<Option<String>> {
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (field, 2) if field == outer_field => {
                return proto_nested_string_field(proto_len(data, &mut pos)?, inner_field);
            }
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(None)
}

pub(crate) fn proto_nested_string_field(data: &[u8], desired_field: u32) -> Result<Option<String>> {
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (field, 2) if field == desired_field => return Ok(Some(proto_string(data, &mut pos)?)),
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(None)
}

pub(crate) fn proto_first_len_field(data: &[u8]) -> Result<Option<u32>> {
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        if wire == 2 {
            return Ok(Some(field));
        }
        proto_skip(data, &mut pos, wire)?;
    }
    Ok(None)
}

pub(crate) fn proto_key(data: &[u8], pos: &mut usize) -> Result<(u32, u8)> {
    let key = proto_varint(data, pos)?;
    Ok(((key >> 3) as u32, (key & 0x07) as u8))
}
