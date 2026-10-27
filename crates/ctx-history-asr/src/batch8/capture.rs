use crate::Result;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek},
};

/// Preserve capture time on logical no-ops, including relocation and a WAL
/// checkpoint that changes physical stamps without changing the selected rows.
pub(crate) fn observation(
    header: &mut Value,
    body: &mut File,
    old: Option<&Value>,
) -> Result<Value> {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(header)?);
    body.rewind()?;
    let mut buffer = [0u8; 65536];
    loop {
        let n = body.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    let hash = format!("{:x}", digest.finalize());
    let unchanged = old.is_some_and(|v| v["_meta"]["rs.ctx/captureDigest"] == hash);
    let previous = old
        .and_then(|v| v["_meta"]["rs.ctx/observedAt"].as_str())
        .filter(|_| unchanged);
    let time = previous
        .map(str::to_owned)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true));
    header["observedAt"] = json!(time);
    Ok(json!({"rs.ctx/captureDigest":hash,"rs.ctx/observedAt":time}))
}
