//! Codex PathUri uses file URIs on every host (codex c587d6e).
//! Decode URI escaping once; retain opaque/non-UTF-8 spellings as native metadata.
use serde_json::{json, Value};
pub(super) fn decode(raw: &str) -> Option<String> {
    let Some(uri) = raw.strip_prefix("file://") else {
        return Some(raw.to_owned());
    };
    let (host, path) = uri.split_once('/')?;
    let raw_path = format!("/{path}");
    let mut decoded = Vec::with_capacity(raw_path.len());
    let mut bytes = raw_path.as_bytes().iter().copied();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let pair = [bytes.next()?, bytes.next()?];
            decoded.push(u8::from_str_radix(std::str::from_utf8(&pair).ok()?, 16).ok()?);
        } else {
            decoded.push(b);
        }
    }
    let path = String::from_utf8(decoded).ok()?;
    if path.contains('\0') {
        return None;
    }
    if !host.is_empty() && host != "localhost" {
        return Some(format!("//{host}{path}"));
    }
    if path.as_bytes().get(2) == Some(&b':') && path.as_bytes()[1].is_ascii_alphabetic() {
        return Some(path[1..].to_owned());
    }
    Some(path)
}
pub(super) fn reference(raw: &str, cwd: Option<&str>) -> Option<Value> {
    let decoded = decode(raw)?;
    let cwd = cwd.and_then(decode);
    let mut p = crate::records::path(&decoded, cwd.as_deref());
    if raw != decoded {
        p["nativePath"] = json!(raw);
    }
    Some(p)
}
