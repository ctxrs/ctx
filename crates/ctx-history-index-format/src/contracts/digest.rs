use super::{IndexError, Result};

pub(super) fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn decode_sha256_hex(value: &str) -> Result<[u8; 32]> {
    if !is_sha256_hex(value) {
        return Err(IndexError::InvalidGenerationId);
    }
    let mut decoded = [0_u8; 32];
    for (output, pair) in decoded.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let high = hex_nibble(pair[0]).ok_or(IndexError::InvalidGenerationId)?;
        let low = hex_nibble(pair[1]).ok_or(IndexError::InvalidGenerationId)?;
        *output = (high << 4) | low;
    }
    Ok(decoded)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
