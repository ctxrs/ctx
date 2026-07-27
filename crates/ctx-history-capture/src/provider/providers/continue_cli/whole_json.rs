use crate::captured_batch::whole_json::WholeJsonBatchError;
use crate::captured_batch::{CapturedBatchError, NativePosition};
use crate::{CaptureError, Result};

const WHOLE_JSON_POSITION_KIND: &str = "whole-json-item-v1";

pub(super) fn whole_json_position(ordinal: u64) -> Result<NativePosition> {
    NativePosition::new(WHOLE_JSON_POSITION_KIND, ordinal.to_be_bytes().to_vec())
        .map_err(continue_captured_batch_error)
}

pub(super) fn whole_json_position_ordinal(position: &NativePosition) -> Result<u64> {
    if position.kind() != WHOLE_JSON_POSITION_KIND || position.value().len() != 8 {
        return Err(CaptureError::InvalidPayload(
            "Continue cursor has an invalid whole-JSON position".to_owned(),
        ));
    }
    let bytes: [u8; 8] = position.value().try_into().map_err(|_| {
        CaptureError::InvalidPayload("Continue cursor has an invalid whole-JSON ordinal".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

pub(super) fn continue_whole_json_error(error: WholeJsonBatchError) -> CaptureError {
    match error {
        WholeJsonBatchError::Io(error) => CaptureError::Io(error),
        WholeJsonBatchError::SourceSizeChanged { .. }
        | WholeJsonBatchError::SourceMetadataChangedDuringRead => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

pub(super) fn continue_captured_batch_error(error: CapturedBatchError) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_codec_preserves_kind_and_big_endian_ordinal_bytes() {
        let position = whole_json_position(0x0102_0304_0506_0708).unwrap();

        assert_eq!(position.kind(), "whole-json-item-v1");
        assert_eq!(position.value(), &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            whole_json_position_ordinal(&position).unwrap(),
            0x0102_0304_0506_0708
        );
    }
}
