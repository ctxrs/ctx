//! Native capture boundaries shared by rewritten task documents and transactional logs.
use crate::{
    records::{copy, meta},
    source::Source,
    spool::Spool,
    Error, Input, Provider, Result,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufReader, Read, Write},
    path::Path,
};

pub(crate) fn visit(
    input: &Input,
    limit: usize,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    if !input.path.exists() {
        return Ok(());
    }
    let path = fs::canonicalize(&input.path)?;
    let dir = if path.is_file() {
        path.parent().unwrap_or(&path)
    } else {
        &path
    };
    let source = if input.provider == Provider::Fx {
        crate::fx::inspect(dir, limit)?
    } else {
        crate::task::inspect(dir, input.provider, limit)?
    };
    if let Some(source) = source {
        return visitor(source);
    }
    if path.is_file() {
        *unidentified += 1;
        return Ok(());
    }
    // No directory symlink traversal. File arguments and aliases select their native owner.
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            visit(
                &Input {
                    provider: input.provider,
                    path: entry.path(),
                },
                limit,
                visitor,
                unidentified,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn object(path: &Path, limit: usize) -> Result<Value> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::Invalid(
            "native metadata exceeds record bound".into(),
        ));
    }
    crate::source::decode(&bytes)
        .ok_or_else(|| Error::Invalid("invalid native metadata object".into()))
}

pub(crate) fn stamp(dir: &Path, files: &[&str]) -> Result<String> {
    let mut hash = Sha256::new();
    for name in files {
        hash.update(name.as_bytes());
        match fs::metadata(dir.join(name)) {
            Ok(m) => {
                hash.update(m.len().to_le_bytes());
                hash.update(format!("{:?}", m.modified()?).as_bytes());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => hash.update(b"absent"),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Stream a JSON array one item at a time. Never allocate a whole task document.
/// A torn or malformed document fails the capture, leaving its published bundle intact.
pub(crate) fn array(
    path: &Path,
    limit: usize,
    item: &mut dyn FnMut(u64, Option<Value>, u64) -> Result<()>,
) -> Result<()> {
    let mut bytes = BufReader::new(File::open(path)?).bytes();
    let next = |bytes: &mut std::io::Bytes<BufReader<File>>| -> Result<Option<u8>> {
        for b in bytes.by_ref() {
            let b = b?;
            if !b.is_ascii_whitespace() {
                return Ok(Some(b));
            }
        }
        Ok(None)
    };
    if next(&mut bytes)? != Some(b'[') {
        return Err(Error::Invalid(
            "native task document is not an array".into(),
        ));
    }
    let mut index = 0;
    let mut first = next(&mut bytes)?;
    if first != Some(b']') {
        loop {
            let mut retained = Vec::new();
            let mut size = 0;
            let mut depth = 0i32;
            let mut quoted = false;
            let mut escape = false;
            let mut current =
                first.ok_or_else(|| Error::Invalid("incomplete task array".into()))?;
            if current == b']' {
                return Err(Error::Invalid("trailing task array comma".into()));
            }
            let delimiter;
            loop {
                if !quoted && depth == 0 && matches!(current, b',' | b']') {
                    delimiter = current;
                    break;
                }
                size += 1;
                if retained.len() < limit {
                    retained.push(current);
                }
                if quoted {
                    if escape {
                        escape = false;
                    } else if current == b'\\' {
                        escape = true;
                    } else if current == b'"' {
                        quoted = false;
                    }
                } else {
                    match current {
                        b'"' => quoted = true,
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => depth -= 1,
                        _ => {}
                    }
                    if !(0..=128).contains(&depth) {
                        return Err(Error::Invalid("invalid task JSON nesting".into()));
                    }
                }
                current = bytes
                    .next()
                    .transpose()?
                    .ok_or_else(|| Error::Invalid("incomplete task array".into()))?;
            }
            let value = if size > limit as u64 {
                None
            } else {
                Some(
                    std::str::from_utf8(&retained)
                        .ok()
                        .and_then(ctx_history_asr_core::numbers::decode)
                        .unwrap_or_else(|| json!({"type":"_gap","reason":"unparseable"})),
                )
            };
            item(index, value, size)?;
            index += 1;
            if delimiter == b']' {
                break;
            }
            first = next(&mut bytes)?;
        }
    }
    if next(&mut bytes)?.is_some() {
        return Err(Error::Invalid("trailing task document bytes".into()));
    }
    Ok(())
}

pub(crate) fn header(provider: Provider, id: &str, n: &Value) -> Value {
    let mut h = json!({"type":"session","formatVersion":"0.1","agent":{"name":provider.agent()},"sessionId":id,"producer":{"name":"ctx","version":env!("CARGO_PKG_VERSION"),"method":"converter","instance":"ctx-asr-batch11-1","source":{"format":provider.format()},"lossy":["order","message.author","lineage","copies","revision_history","_rs.ctx/nontranscript_state","_rs.ctx/vendor_only","_rs.ctx/native_line_limit","_rs.ctx/non_utf8_content"]},"profiles":["core","coding","usage"],"contentLevel":"full","workspace":{"roots":[{"id":"unknown","path":null}]}});
    if let Some(cwd) = n["cwd"].as_str().filter(|p| p.starts_with('/')) {
        h["workspace"]["roots"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"workspace","path":cwd}));
    }
    copy(&mut h, "startedAt", n, "startedAt");
    h
}

pub(crate) fn millis(v: &Value) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(v.as_i64()?)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

pub(crate) fn unsafe_numbers(v: &Value) -> bool {
    !ctx_history_asr_core::numbers::safe(v)
}

pub(crate) fn retain(
    record: &mut Value,
    provider: Provider,
    source: &Value,
    spool: &Spool,
) -> Result<()> {
    externalize(record, spool)?;
    let bytes = ctx_history_asr_core::numbers::text(source).into_bytes();
    if bytes.len() > 65536 {
        let mut file = tempfile::NamedTempFile::new_in(&spool.bundle)?;
        file.write_all(&bytes)?;
        meta(record, provider, "source", &spool.blob(file)?);
    } else {
        meta(record, provider, "source", source);
    }
    Ok(())
}
fn externalize(v: &mut Value, spool: &Spool) -> Result<()> {
    use base64::Engine;
    if matches!(v["type"].as_str(), Some("image" | "audio")) {
        if let Some(encoded) = v["data"].as_str().filter(|s| s.len() > 65536) {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| Error::Invalid("invalid native base64".into()))?;
            if bytes.len() > 65536 {
                let mut file = tempfile::NamedTempFile::new_in(&spool.bundle)?;
                file.write_all(&bytes)?;
                let mut blob = spool.blob(file)?;
                blob["mimeType"] = v["mimeType"].clone();
                v.as_object_mut().unwrap().remove("data");
                v["blob"] = blob;
            }
        }
    }
    if let Some(items) = v.as_array_mut() {
        for item in items {
            externalize(item, spool)?;
        }
    } else if let Some(items) = v.as_object_mut() {
        for item in items.values_mut() {
            externalize(item, spool)?;
        }
    }
    Ok(())
}
