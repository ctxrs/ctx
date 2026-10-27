//! Bound inline representations without interpreting or fetching attachments.
use crate::Result;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path};
const INLINE_LIMIT: usize = 64 * 1024;
pub(crate) fn externalize(out: &Path, record: &mut Value) -> Result<()> {
    for field in [
        "content",
        "displayContent",
        "raw",
        "input",
        "inputText",
        "summary",
    ] {
        if let Some(value) = record.get(field) {
            let bytes = serde_json::to_vec(value)?;
            if bytes.len() > INLINE_LIMIT {
                let blob = store(out, &bytes)?;
                record.as_object_mut().map(|o| o.remove(field));
                if field == "content" && record["type"] == "message" {
                    record["content"] = json!([]);
                }
                record["capture"][field] = json!({"state":"external","blob":blob});
            }
        }
    }
    if let Some(meta) = record.get_mut("_meta").and_then(Value::as_object_mut) {
        for value in meta.values_mut() {
            let bytes = serde_json::to_vec(value)?;
            if bytes.len() > INLINE_LIMIT {
                *value = store(out, &bytes)?;
            }
        }
    }
    Ok(())
}
fn store(out: &Path, bytes: &[u8]) -> Result<Value> {
    let hash = format!("{:x}", Sha256::digest(bytes));
    let dir = out.join("blobs/sha256");
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(&hash);
    if !target.exists() {
        let mut file = tempfile::NamedTempFile::new_in(&dir)?;
        file.write_all(bytes)?;
        file.persist(target).map_err(|e| e.error)?;
    }
    Ok(json!({"blob":format!("sha256:{hash}"),"size":bytes.len(),"mimeType":"application/json"}))
}
