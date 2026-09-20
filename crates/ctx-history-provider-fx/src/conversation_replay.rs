//! Narrow FXRPLY01 decoder matching command_replay_store.zig at fx 0.0.10.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ctx_history_provider_runtime::CaptureError;
use serde_json::{json, Value};

pub(crate) struct CommandReplay {
    /// Native callback order, with byte ranges relative to each complete stream.
    pub frames: Vec<(&'static str, usize, usize)>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub(crate) fn decode(bytes: &[u8]) -> Result<CommandReplay, CaptureError> {
    let mut rest = bytes
        .strip_prefix(b"FXRPLY01")
        .ok_or_else(|| invalid("invalid fx command replay header"))?;
    let mut replay = CommandReplay {
        frames: Vec::new(),
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    while !rest.is_empty() {
        if rest.len() < 9 {
            return Err(invalid("truncated fx command replay frame"));
        }
        let stream = match rest[0] {
            0 => "stdout",
            1 => "stderr",
            _ => return Err(invalid("invalid fx command replay stream")),
        };
        let length = u64::from_le_bytes(
            rest[1..9]
                .try_into()
                .map_err(|_| invalid("invalid fx replay length"))?,
        );
        // Native decodeFrameHeader rejects empty and >1 MiB frames.
        if length == 0 || length > 1024 * 1024 || length > (rest.len() - 9) as u64 {
            return Err(invalid("invalid fx command replay frame length"));
        }
        // Bound metadata expansion from one-byte callbacks independently of body
        // bytes; the caller records an explicit omission, not a partial replay.
        if replay.frames.len() == 65_536 {
            return Err(invalid("fx replay exceeds bounded frame inventory"));
        }
        let end = 9 + length as usize;
        let output = if stream == "stdout" {
            &mut replay.stdout
        } else {
            &mut replay.stderr
        };
        replay.frames.push((stream, output.len(), length as usize));
        output.extend_from_slice(&rest[9..end]);
        rest = &rest[end..];
    }
    Ok(replay)
}

pub(crate) fn capture(bytes: &[u8], body: &mut String) -> Result<Value, CaptureError> {
    let replay = decode(bytes)?;
    let mut streams = serde_json::Map::new();
    for (name, bytes) in [("stdout", replay.stdout), ("stderr", replay.stderr)] {
        let capture = match std::str::from_utf8(&bytes) {
            Ok(text) => {
                let start = body.len() + usize::from(!body.is_empty() && !text.is_empty());
                crate::conversation::append(body, text)?;
                json!({"capture_status":"normalized_body", "byte_start":start,"byte_length":bytes.len()})
            }
            Err(_) => json!({"capture_status":"present_base64", "bytes":STANDARD.encode(bytes)}),
        };
        streams.insert(name.into(), capture);
    }
    Ok(
        json!({"capture_status":"present", "frame_fields":["stream","byte_start","byte_length"],"frames":replay.frames,"streams":streams}),
    )
}

fn invalid(message: &str) -> CaptureError {
    CaptureError::InvalidPayload(message.into())
}
