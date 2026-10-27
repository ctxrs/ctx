use crate::{
    content,
    records::{extras, meta},
    Provider,
};
use serde_json::{json, Value};
const P: Provider = Provider::Pi;
pub(super) fn parts(value: &Value) -> Vec<Value> {
    if value.is_string() {
        return content::parts(value, P);
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let mut r = match p["type"].as_str() {
                Some("toolCall") => return None,
                Some("thinking") => {
                    let mut r = if p["redacted"] == true {
                        json!({"type":"reasoning","visibility":"redacted"})
                    } else {
                        json!({"type":"reasoning","text":p["thinking"]})
                    };
                    if let Some(s) = p["thinkingSignature"].as_str() {
                        if p["redacted"] == true {
                            r["opaque"] = json!({"format":"pi.thinkingSignature","data":s});
                        } else {
                            r["signature"] = json!(s);
                        }
                    }
                    extras(&mut r, P, p, &["thinking", "thinkingSignature", "redacted"]);
                    r
                }
                _ => content::parts(&json!([p]), P).into_iter().next()?,
            };
            extras(
                &mut r,
                P,
                p,
                &[
                    "text",
                    "thinking",
                    "thinkingSignature",
                    "redacted",
                    "data",
                    "mimeType",
                    "annotations",
                    "_meta",
                ],
            );
            if let Some(signature) = p["textSignature"].as_str() {
                meta(&mut r, P, "textSignature", &json!(signature));
                if let Ok(s) = serde_json::from_str::<Value>(signature) {
                    match s["phase"].as_str() {
                        Some("commentary") => r["phase"] = json!("commentary"),
                        Some("final_answer") => r["phase"] = json!("final"),
                        _ => {}
                    }
                }
            }
            Some(r)
        })
        .collect()
}
