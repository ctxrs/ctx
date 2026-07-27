use std::io::{self, Read, Write};

use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

use crate::{FRAME_HEADER_BYTES, FRAME_MAGIC, MAX_FRAME_PAYLOAD_BYTES, PROTOCOL_VERSION};

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid frame magic")]
    InvalidMagic,
    #[error("unsupported frame protocol version {received}; supported version is {supported}")]
    UnsupportedVersion { received: u16, supported: u16 },
    #[error("frame declares {declared} payload bytes; maximum is {maximum}")]
    Oversized { declared: usize, maximum: usize },
    #[error("truncated frame header: expected {expected} bytes, received {received}")]
    TruncatedHeader { expected: usize, received: usize },
    #[error("truncated frame payload: expected {expected} bytes, received {received}")]
    TruncatedPayload { expected: usize, received: usize },
    #[error("invalid strict JSON payload: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

/// Writes one complete `CTXPRO` frame. No partial payload is written after a
/// bounds error.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), FrameError> {
    let payload = serde_json::to_vec(value)?;
    if payload.len() > MAX_FRAME_PAYLOAD_BYTES {
        return Err(FrameError::Oversized {
            declared: payload.len(),
            maximum: MAX_FRAME_PAYLOAD_BYTES,
        });
    }
    writer.write_all(FRAME_MAGIC)?;
    writer.write_all(&PROTOCOL_VERSION.to_be_bytes())?;
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

/// Reads one strict JSON frame with fixed global bounds.
pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, FrameError> {
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    let header_read = read_at_most(reader, &mut header)?;
    if header_read != FRAME_HEADER_BYTES {
        return Err(FrameError::TruncatedHeader {
            expected: FRAME_HEADER_BYTES,
            received: header_read,
        });
    }
    if &header[..FRAME_MAGIC.len()] != FRAME_MAGIC {
        return Err(FrameError::InvalidMagic);
    }
    let version_offset = FRAME_MAGIC.len();
    let version = u16::from_be_bytes([header[version_offset], header[version_offset + 1]]);
    if version != PROTOCOL_VERSION {
        return Err(FrameError::UnsupportedVersion {
            received: version,
            supported: PROTOCOL_VERSION,
        });
    }
    let length_offset = version_offset + 2;
    let declared = u32::from_be_bytes([
        header[length_offset],
        header[length_offset + 1],
        header[length_offset + 2],
        header[length_offset + 3],
    ]) as usize;
    if declared > MAX_FRAME_PAYLOAD_BYTES {
        return Err(FrameError::Oversized {
            declared,
            maximum: MAX_FRAME_PAYLOAD_BYTES,
        });
    }
    let mut payload = vec![0; declared];
    let payload_read = read_at_most(reader, &mut payload)?;
    if payload_read != declared {
        return Err(FrameError::TruncatedPayload {
            expected: declared,
            received: payload_read,
        });
    }
    serde_json::from_slice(&payload).map_err(FrameError::InvalidJson)
}

fn read_at_most(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buffer.len() {
        match reader.read(&mut buffer[total..]) {
            Ok(0) => break,
            Ok(read) => total += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(total)
}
