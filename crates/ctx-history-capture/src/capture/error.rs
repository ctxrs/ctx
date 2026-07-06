#[allow(unused_imports)]
use super::*;

pub type Result<T> = std::result::Result<T, CaptureError>;

pub(crate) fn proto_skip(data: &[u8], pos: &mut usize, wire: u8) -> Result<()> {
    match wire {
        0 => {
            let _ = proto_varint(data, pos)?;
        }
        1 => {
            *pos = pos.checked_add(8).ok_or_else(|| {
                CaptureError::InvalidPayload("overflow while skipping fixed64".into())
            })?;
        }
        2 => {
            let _ = proto_len(data, pos)?;
        }
        5 => {
            *pos = pos.checked_add(4).ok_or_else(|| {
                CaptureError::InvalidPayload("overflow while skipping fixed32".into())
            })?;
        }
        other => {
            return Err(CaptureError::InvalidPayload(format!(
                "unsupported Warp protobuf wire type {other}"
            )));
        }
    }
    if *pos > data.len() {
        return Err(CaptureError::InvalidPayload(
            "truncated field while skipping Warp protobuf".into(),
        ));
    }
    Ok(())
}

pub(crate) fn uuid_field(value: &Value, field: &str) -> Result<Option<Uuid>> {
    match value.get(field) {
        Some(Value::String(raw)) => Ok(Some(Uuid::parse_str(raw)?)),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CaptureError::InvalidPayload(format!(
            "{field} must be a UUID string"
        ))),
    }
}
