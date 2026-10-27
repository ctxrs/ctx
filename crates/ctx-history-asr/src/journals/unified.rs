//! Mistral Vibe 3 committed unified-session-store/v1 generations.
use crate::{
    batch10::document,
    capture, content,
    records::{copy, Writer},
    source::{self, Source},
    spool::Spool,
    Error, Provider, Result,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
const P: Provider = Provider::MistralVibe;
fn child(root: &Path, name: &str) -> Result<PathBuf> {
    if Path::new(name)
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(Error::Invalid("invalid Vibe store component path".into()));
    }
    Ok(root.join(name))
}
fn verify(path: &Path, expected: &str) -> Result<()> {
    let mut file = fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size == 0 {
        return Err(Error::Invalid("empty Vibe document".into()));
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0];
    file.read_exact(&mut last)?;
    file.seek(SeekFrom::Start(0))?;
    // The writer hashes canonical bytes, excluding the required final newline.
    let length = if last[0] == b'\n' { size - 1 } else { size };
    let mut hash = Sha256::new();
    std::io::copy(&mut file.take(length), &mut hash)?;
    if format!("{:x}", hash.finalize()) != expected {
        return Err(Error::Invalid("Vibe document digest mismatch".into()));
    }
    Ok(())
}
fn native_document(spool: &mut Spool, component: &str, path: &Path) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(&spool.bundle)?;
    std::io::copy(&mut fs::File::open(path)?, temporary.as_file_mut())?;
    temporary.flush()?;
    let blob = spool.blob(temporary)?;
    spool.push(
        &json!({"_vibe_component":component,"_native_blob":blob}),
        None,
    )?;
    Ok(())
}
fn read(path: &Path, limit: usize, digest: Option<&str>) -> Result<Value> {
    if let Some(expected) = digest {
        verify(path, expected)?;
    }
    capture::object(path, limit)
}
fn manifest(root: &Path, limit: usize) -> Result<(Value, Value, PathBuf)> {
    let c = read(&root.join("CURRENT"), limit, None)?;
    if c["store_format"] != "mistral.vibe.unified-session-store/v1"
        || c["store_format_minor"].as_u64().is_none_or(|n| n > 10)
    {
        return Err(Error::Invalid(
            "unsupported Vibe unified store version".into(),
        ));
    }
    let gen = c["generation"]
        .as_str()
        .ok_or_else(|| Error::Invalid("missing Vibe generation".into()))?;
    let dir = child(&root.join("generations"), gen)?;
    let m = read(
        &dir.join("manifest.json"),
        limit,
        c["manifest_sha256"].as_str(),
    )?;
    if m["session_id"] != c["session_id"]
        || m["generation"] != c["generation"]
        || m["snapshot_sequence"] != c["snapshot_sequence"]
    {
        return Err(Error::Invalid(
            "inconsistent Vibe generation identity".into(),
        ));
    }
    Ok((c, m, dir))
}
pub(super) fn inspect(root: &Path, limit: usize) -> Result<Source> {
    let (c, m, dir) = manifest(root, limit)?;
    let id = c["session_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Invalid("missing Vibe session ID".into()))?
        .to_owned();
    let runtime = read(
        &child(
            &dir,
            m["runtime_state"]["path"]
                .as_str()
                .unwrap_or("runtime-state.json"),
        )?,
        limit,
        m["runtime_state"]["sha256"].as_str(),
    )?;
    let mut header = json!({"_unified":true,"metadata":runtime["session_metadata"],"current":c});
    header["cwd"] = runtime["session_metadata"]["cwd"].clone();
    let journal = child(
        root,
        m["recovery_journal_segment"]["path"]
            .as_str()
            .unwrap_or("journal"),
    )?;
    let stamp = format!(
        "{}:{}:{}",
        c,
        fs::metadata(&journal)?.len(),
        format_args!("{:?}", fs::metadata(&journal)?.modified()?)
    );
    Ok(Source {
        provider: P,
        path: root.into(),
        session: id.clone(),
        segment: id,
        header,
        stamp,
    })
}
fn push(spool: &mut Spool, component: &str, v: Value) -> Result<()> {
    let pos = spool.push(&json!({"_vibe_component":component,"value":v}), None)?;
    if component == "message" {
        for b in v["message"]["content"].as_array().into_iter().flatten() {
            if b["type"] == "tool_call" {
                if let Some(id) = b["id"].as_str() {
                    spool.db.execute(
                        "INSERT INTO native_calls VALUES(?1,?2)",
                        rusqlite::params![id, pos],
                    )?;
                }
            }
        }
    }
    Ok(())
}
pub(crate) fn ingest(spool: &mut Spool, root: &Path) -> Result<()> {
    let (before, m, dir) = manifest(root, spool.limit)?;
    push(spool, "manifest", m.clone())?;
    for (field, array, component) in [
        ("checkpoint", "context/messages", "message"),
        ("runtime_state", "", "runtime"),
        ("projection_state", "snapshot/history/entries", "projection"),
    ] {
        let desc = &m[field];
        let path = child(
            &dir,
            desc["path"]
                .as_str()
                .ok_or_else(|| Error::Invalid("missing Vibe document path".into()))?,
        )?;
        let limit = spool.limit;
        spool.source_bytes += fs::metadata(&path)?.len();
        // Stream legacy monolithic transcripts too. Chunked generations retain
        // an empty list in this document and name the ordered immutable chunks.
        let arrays = if array.is_empty() {
            vec![]
        } else {
            vec![array]
        };
        if let Some(digest) = desc["sha256"].as_str() {
            verify(&path, digest)?;
        }
        document::read(&path, &arrays, limit, &mut |_, _, raw| {
            push(spool, component, document::decode(raw)?)
        })?;
        native_document(spool, field, &path)?;
        for digest in desc["chunks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::Invalid("invalid Vibe chunk digest".into()));
            }
            let path = root.join("chunks").join(format!("{digest}.json"));
            verify(&path, digest)?;
            spool.source_bytes += fs::metadata(&path)?.len();
            capture::array(&path, limit, &mut |_, v, _| {
                push(
                    spool,
                    component,
                    v.unwrap_or_else(|| json!({"type":"_gap","reason":"conversion_loss"})),
                )
            })?;
        }
    }
    let path = child(
        root,
        m["recovery_journal_segment"]["path"]
            .as_str()
            .unwrap_or("journal"),
    )?;
    let mut input = source::reader(&path)?;
    let mut bytes = Vec::new();
    while let Some((size, large)) = source::line(&mut *input, &mut bytes, spool.limit)? {
        spool.source_bytes += size as u64;
        push(
            spool,
            "recovery",
            if large {
                json!({"type":"_gap","reason":"conversion_loss"})
            } else {
                source::decode(&bytes)
                    .ok_or_else(|| Error::Invalid("invalid Vibe recovery journal".into()))?
            },
        )?;
    }
    if read(&root.join("CURRENT"), spool.limit, None)? != before {
        return Err(Error::Invalid(
            "Vibe generation changed during capture; retry".into(),
        ));
    }
    Ok(())
}
pub(super) fn convert(spool: &Spool, w: &mut Writer, row: &Value) -> Result<()> {
    let component = row["_vibe_component"].as_str().unwrap_or("record");
    let v = &row["value"];
    w.snapshot = true;
    w.capture_scope = true;
    if v["type"] == "_gap" {
        w.emit(json!({"type":"gap","reason":v["reason"]}), &Value::Null)?;
        return Ok(());
    }
    if let Some(blob) = row.get("_native_blob") {
        w.emit(
            json!({"type":format!("{}/{component}",P.domain()),"native":{"blob":blob}}),
            &Value::Null,
        )?;
        return Ok(());
    }
    if component != "message" {
        let r = json!({"type":format!("{}/{component}",P.domain()),"native":{"record":v}});
        w.emit(r, &Value::Null)?;
        return Ok(());
    }
    let m = &v["message"];
    let role = m["role"].as_str();
    if role == Some("tool") {
        let mut r = json!({"type":"tool_result","status":match m["outcome"].as_str(){Some("success")=>"completed",Some("error")=>"failed",_=>"unknown"},"linkage":"none","raw":m["content"],"capture":{"content":{"state":"absent"}},"native":{"record":v}});
        if let Some(id) = m["tool_call_id"].as_str() {
            r["callId"] = json!(id);
            r["linkage"] = json!(if spool.ambiguous_call(id)? {
                "ambiguous"
            } else {
                "exact"
            });
        }
        w.emit(r, &Value::Null)?;
        return Ok(());
    }
    if !matches!(role, Some("system" | "user" | "assistant" | "developer")) {
        w.vendor(v, "checkpoint_message")?;
        return Ok(());
    }
    let blocks: Vec<_> = m["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] != "tool_call")
        .cloned()
        .collect();
    let mut r = json!({"type":"message","messageId":format!("message:{}",w.id()),"idSource":"derived","role":role,"content":content::parts(&json!(blocks),P),"native":{"record":v}});
    if role == Some("assistant") {
        r["author"] = json!("model");
    }
    w.emit(r, &Value::Null)?;
    for b in m["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "tool_call")
    {
        let mut r = json!({"type":"tool_call","requestedBy":"model","callIdSource":"native","native":{"record":b}});
        copy(&mut r, "callId", b, "id");
        copy(&mut r, "name", b, "name");
        if r["callId"].is_null() {
            r["callId"] = json!(w.id());
            r["callIdSource"] = json!("derived");
        }
        if b["arguments"]["raw"].is_string() {
            r["inputText"] = b["arguments"]["raw"].clone();
        } else {
            copy(&mut r, "input", &b["arguments"], "value");
        }
        w.emit(r, &Value::Null)?;
    }
    Ok(())
}
