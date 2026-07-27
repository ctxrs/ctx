#![recursion_limit = "512"]

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use ctx_pro_host_protocol::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[path = "generate_protocol_inventory/blame.rs"]
mod blame;
#[path = "generate_protocol_inventory/fixtures.rs"]
mod fixtures;
#[path = "generate_protocol_inventory/messages.rs"]
mod messages;
#[path = "generate_protocol_inventory/schema.rs"]
mod schema;

use messages::golden_vectors;
use schema::inventory;

const FINGERPRINT_PLACEHOLDER: &str = "<sha256-of-this-canonical-inventory>";

fn fields(required: &[&str], optional: &[&str]) -> Value {
    json!({"required": required, "optional": optional})
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn frame_hex<T: serde::Serialize>(value: &T) -> String {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, value).unwrap_or_else(|error| panic!("encode golden frame: {error}"));
    hex(&bytes)
}

fn main() {
    let bytes =
        serde_json::to_vec(&inventory()).unwrap_or_else(|error| panic!("inventory: {error}"));
    let digest = hex(&Sha256::digest(&bytes));
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("--fingerprint-rust") => {
            assert!(
                arguments.next().is_none(),
                "--fingerprint-rust takes no value"
            );
            println!(
                "/// Lowercase SHA-256 of `testdata/v1/inventory.json`'s canonical inventory.\n\
                 pub const PROTOCOL_FINGERPRINT: &str =\n    \"{digest}\";"
            );
            return;
        }
        None => {}
        Some(argument) => panic!("unsupported inventory argument: {argument}"),
    }
    let output = json!({
        "canonical_inventory": serde_json::from_slice::<Value>(&bytes)
            .unwrap_or_else(|error| panic!("canonical inventory: {error}")),
        "canonical_sha256": digest,
        "golden_vectors": golden_vectors(&digest)
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output)
            .unwrap_or_else(|error| panic!("format inventory: {error}"))
    );
}
