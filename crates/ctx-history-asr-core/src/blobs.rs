//! Bound inline representations without interpreting or fetching attachments.
use crate::Result;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path};
const INLINE_LIMIT: usize = 64 * 1024;
pub fn externalize(out: &Path, record: &mut Value) -> Result<()> {
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
pub fn store(out: &Path, bytes: &[u8]) -> Result<Value> {
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

/// Move binary payloads by decoded size. Ordinary text is not binary.
pub fn binaries(out: &Path, value: &mut Value) -> Result<()> {
    use base64::Engine;
    let binary = match value["type"].as_str() {
        Some("image" | "audio") => Some((&mut *value, "data", "blob")),
        Some("resource") => value.get_mut("resource").map(|v| (v, "blob", "blobRef")),
        _ => None,
    };
    if let Some((part, inline, external)) = binary {
        if let Some(data) = part[inline].as_str().filter(|s| s.len() > INLINE_LIMIT) {
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                if bytes.len() > INLINE_LIMIT {
                    let mut blob = store(out, &bytes)?;
                    if let Some(mime) = part.get("mimeType") {
                        blob["mimeType"] = mime.clone();
                    } else {
                        blob["mimeType"] = json!("application/octet-stream");
                    }
                    part.as_object_mut().unwrap().remove(inline);
                    part[external] = blob;
                }
            }
        }
    }
    match value {
        Value::Array(a) => {
            for v in a {
                binaries(out, v)?;
            }
        }
        Value::Object(o) => {
            for v in o.values_mut() {
                binaries(out, v)?;
            }
        }
        _ => {}
    }
    Ok(())
}
/// Preserve whole affected values as JSON text or JSON blobs, including native facts.
pub fn numbers(out: &Path, record: &mut Value) -> Result<()> {
    let keys: Vec<String> = record
        .as_object()
        .into_iter()
        .flat_map(|o| o.keys().cloned())
        .collect();
    for key in keys {
        if crate::numbers::safe(&record[&key]) {
            continue;
        }
        let value = record.as_object_mut().unwrap().remove(&key).unwrap();
        let text = crate::numbers::text(&value);
        if key == "input" {
            record["inputText"] = json!(text);
        } else if matches!(
            key.as_str(),
            "content" | "displayContent" | "raw" | "summary"
        ) {
            let blob = store(out, text.as_bytes())?;
            record["capture"][&key] = json!({"state":"external","blob":blob});
            if key == "content" && record["type"] == "message" {
                record["content"] = json!([]);
            }
        } else if key == "_meta" {
            // Namespaced JSON text is an explicit carrier, not a changed native value.
            record["_meta"] = json!({"rs.ctx/metadataJsonText":text});
        } else {
            record["_meta"]["rs.ctx/unsafeJsonText"][&key] = json!(text);
        }
    }
    Ok(())
}
