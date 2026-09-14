use serde::{Deserialize, Serialize};
use sha2::{
    compress256,
    digest::{generic_array::GenericArray, typenum::U64},
};

const SHA256_INITIAL_STATE: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Stable wire representation of a resumable SHA-256 computation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonlSha256State {
    version: u32,
    state: [u32; 8],
    bytes_hashed: u64,
    buffer: Vec<u8>,
}

/// SHA-256 with an explicit, implementation-independent resumable state.
#[derive(Debug, Clone)]
pub struct JsonlResumableSha256 {
    state: [u32; 8],
    bytes_hashed: u64,
    buffer: [u8; 64],
    buffer_len: u8,
}

impl Default for JsonlResumableSha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonlResumableSha256 {
    const STATE_VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            state: SHA256_INITIAL_STATE,
            bytes_hashed: 0,
            buffer: [0; 64],
            buffer_len: 0,
        }
    }

    pub fn restore(snapshot: &JsonlSha256State) -> Option<Self> {
        let buffered = u64::try_from(snapshot.buffer.len()).ok()?;
        (snapshot.version == Self::STATE_VERSION
            && snapshot.buffer.len() < 64
            && snapshot.bytes_hashed % 64 == buffered
            && snapshot.bytes_hashed <= u64::MAX / 8)
            .then(|| {
                let mut buffer = [0; 64];
                buffer[..snapshot.buffer.len()].copy_from_slice(&snapshot.buffer);
                Self {
                    state: snapshot.state,
                    bytes_hashed: snapshot.bytes_hashed,
                    buffer,
                    buffer_len: snapshot.buffer.len() as u8,
                }
            })
    }

    pub fn snapshot(&self) -> JsonlSha256State {
        JsonlSha256State {
            version: Self::STATE_VERSION,
            state: self.state,
            bytes_hashed: self.bytes_hashed,
            buffer: self.buffer[..usize::from(self.buffer_len)].to_vec(),
        }
    }

    pub fn bytes_hashed(&self) -> u64 {
        self.bytes_hashed
    }

    pub fn update(&mut self, mut bytes: &[u8]) {
        self.bytes_hashed = self
            .bytes_hashed
            .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .expect("SHA-256 input length exceeds its 64-bit bit-length encoding");
        let buffered = usize::from(self.buffer_len);
        if buffered != 0 {
            let take = (64 - buffered).min(bytes.len());
            self.buffer[buffered..buffered + take].copy_from_slice(&bytes[..take]);
            self.buffer_len += take as u8;
            bytes = &bytes[take..];
            if self.buffer_len != 64 {
                return;
            }
            compress_blocks(&mut self.state, &self.buffer);
            self.buffer_len = 0;
        }
        let block_bytes = bytes.len() / 64 * 64;
        if block_bytes != 0 {
            compress_blocks(&mut self.state, &bytes[..block_bytes]);
            bytes = &bytes[block_bytes..];
        }
        self.buffer[..bytes.len()].copy_from_slice(bytes);
        self.buffer_len = bytes.len() as u8;
    }

    pub fn digest(&self) -> [u8; 32] {
        let mut state = self.state;
        let bit_length = self
            .bytes_hashed
            .checked_mul(8)
            .expect("SHA-256 input length exceeds its bit-length encoding");
        let buffered = usize::from(self.buffer_len);
        let padded_len = if buffered < 56 { 64 } else { 128 };
        let mut tail = [0; 128];
        tail[..buffered].copy_from_slice(&self.buffer[..buffered]);
        tail[buffered] = 0x80;
        tail[padded_len - 8..padded_len].copy_from_slice(&bit_length.to_be_bytes());
        compress_blocks(&mut state, &tail[..padded_len]);
        let mut digest = [0_u8; 32];
        for (encoded, word) in digest.chunks_exact_mut(4).zip(state) {
            encoded.copy_from_slice(&word.to_be_bytes());
        }
        digest
    }
}

fn compress_blocks(state: &mut [u32; 8], bytes: &[u8]) {
    debug_assert_eq!(bytes.len() % 64, 0);
    // SAFETY: `GenericArray<u8, U64>` is transparent over 64 bytes, has byte
    // alignment, and `bytes` contains an exact whole number of blocks.
    let blocks = unsafe {
        std::slice::from_raw_parts(
            bytes.as_ptr().cast::<GenericArray<u8, U64>>(),
            bytes.len() / 64,
        )
    };
    compress256(state, blocks);
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn resumable_sha256_matches_standard_vectors_and_every_resume_boundary() {
        for input in [
            &b""[..],
            &b"abc"[..],
            &b"a payload crossing the SHA-256 block boundary a payload crossing it again"[..],
        ] {
            let expected: [u8; 32] = Sha256::digest(input).into();
            for split in 0..=input.len() {
                let mut before = JsonlResumableSha256::new();
                before.update(&input[..split]);
                let mut resumed = JsonlResumableSha256::restore(&before.snapshot()).unwrap();
                resumed.update(&input[split..]);
                assert_eq!(resumed.digest(), expected, "split {split}");
            }
        }
    }
}
