#[allow(unused_imports)]
use super::*;

pub fn stable_capture_uuid(dedupe_key: &str, role: &str) -> Uuid {
    let mut bytes = [0_u8; 16];
    let name = format!("ctx-ctx-history-capture:{dedupe_key}:{role}");
    let first = fnv1a64(name.as_bytes()).to_be_bytes();
    let second = fnv1a64(format!("{name}:uuid-v7").as_bytes()).to_be_bytes();

    bytes[..6].copy_from_slice(&first[..6]);
    bytes[6] = 0x70 | (first[6] & 0x0f);
    bytes[7] = first[7];
    bytes[8] = 0x80 | (second[0] & 0x3f);
    bytes[9..].copy_from_slice(&second[1..]);
    Uuid::from_bytes(bytes)
}
