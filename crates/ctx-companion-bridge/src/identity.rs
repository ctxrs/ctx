use std::fmt;

use crate::BridgeError;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn from_hex(value: &str) -> Result<Self, BridgeError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(BridgeError::Verification(
                "SHA-256 identity is not lowercase hexadecimal".to_owned(),
            ));
        }
        let mut bytes = [0_u8; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = hex_nibble(chunk[0]);
            let low = hex_nibble(chunk[1]);
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        format!("{self}")
    }
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}
