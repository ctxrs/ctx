//! Provider-native layouts, corroborated by the owning parsers and writer sources.
use crate::{
    source::{self, Source},
    Error, Input, Provider, Result,
};
use serde_json::{json, Value};
use std::{fs, io::Read, path::Path};

pub(crate) fn document(path: &Path, limit: usize) -> Result<Option<Value>> {
    if !path.is_file() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Ok(Some(json!({"_asr_gap":"conversion_loss"})));
    }
    Ok(Some(
        super::ingest::decode(&bytes).unwrap_or(json!({"_asr_gap":"unparseable"})),
    ))
}
fn stamp(path: &Path) -> Result<String> {
    match fs::metadata(path) {
        Ok(m) => Ok(format!("{}:{:?}", m.len(), m.modified()?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("absent".into()),
        Err(e) => Err(e.into()),
    }
}
pub(crate) fn visit(
    input: &Input,
    limit: usize,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    if input.provider == Provider::MistralVibe && input.path.join("CURRENT").is_file() {
        return visitor(super::unified::inspect(&input.path, limit)?);
    }
    if input.path.is_dir() {
        // The inventory is metadata only. Bodies are read one session/line at a time.
        let mut entries = fs::read_dir(&input.path)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            if e.file_type()?.is_symlink() {
                continue;
            }
            visit(
                &Input {
                    provider: input.provider,
                    path: e.path(),
                },
                limit,
                visitor,
                unidentified,
            )?;
        }
        return Ok(());
    }
    if !input.path.is_file() {
        return Ok(());
    }
    let p = &input.path;
    let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if input.provider == Provider::Cursor && matches!(name, "state.vscdb" | "store.db") {
        *unidentified += 1;
        return Ok(()); // No documented native decoder in ctx for either database.
    }
    let candidate = match input.provider {
        Provider::Junie => name == "events.jsonl",
        Provider::MistralVibe => name == "messages.jsonl",
        Provider::Mux => {
            name == "chat.jsonl"
                || (name == "chat-archive.jsonl" && !p.with_file_name("chat.jsonl").is_file())
                || (name == "partial.json"
                    && !p.with_file_name("chat.jsonl").is_file()
                    && !p.with_file_name("chat-archive.jsonl").is_file())
        }
        Provider::Tabnine => name.ends_with(".jsonl"),
        Provider::Cursor => {
            name.ends_with(".jsonl")
                && p.parent()
                    .and_then(Path::parent)
                    .is_some_and(|d| d.ends_with("agent-transcripts"))
        }
        _ => false,
    };
    if !candidate {
        return Ok(());
    }
    let dir = p
        .parent()
        .ok_or_else(|| Error::Invalid("native source has no parent".into()))?;
    let mut header = json!({});
    let mut sidecars = Vec::new();
    let mut reader = source::reader(p)?;
    let mut bytes = Vec::new();
    let mut first = None;
    for _ in 0..64 {
        let Some((_, oversized)) = source::line(&mut *reader, &mut bytes, limit)? else {
            break;
        };
        if !oversized {
            if let Some(v) = source::decode(&bytes) {
                first = Some(v);
                break;
            }
        }
    }
    let first = first.unwrap_or(json!({}));
    let folder = dir
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty());
    let session = match input.provider {
        Provider::Junie => {
            let Some(id) = folder.filter(|s| s.starts_with("session-")) else {
                *unidentified += 1;
                return Ok(());
            };
            let index = dir.parent().unwrap_or(dir).join("index.jsonl");
            if index.is_file() {
                let mut r = source::reader(&index)?;
                while let Some((_, large)) = source::line(&mut *r, &mut bytes, limit)? {
                    if !large {
                        if let Some(v) = source::decode(&bytes) {
                            if v["sessionId"] == id {
                                header["metadata"] = v;
                            }
                        }
                    }
                }
            }
            sidecars.push(index);
            id.to_owned()
        }
        Provider::MistralVibe => {
            let meta = dir.join("meta.json");
            header["metadata"] = document(&meta, limit)?.unwrap_or(json!({}));
            sidecars.push(meta);
            let Some(id) = header["metadata"]["session_id"]
                .as_str()
                .filter(|s| !s.is_empty())
            else {
                *unidentified += 1;
                return Ok(());
            };
            id.to_owned()
        }
        Provider::Tabnine => {
            let Some(id) = first["sessionId"].as_str().filter(|s| !s.is_empty()) else {
                *unidentified += 1;
                return Ok(());
            };
            header["metadata"] = first.clone();
            if first["kind"] == "subagent" && dir.parent().is_some_and(|d| d.ends_with("chats")) {
                header["parent"] = json!(folder);
            }
            id.to_owned()
        }
        Provider::Mux => {
            let meta = dir.join("metadata.json");
            let partial = dir.join("partial.json");
            header["metadata"] = document(&meta, limit)?.unwrap_or(json!({}));
            // Cleanup rolls descendants into ancestor archives and rewrites the
            // index's parentWorkspaceId. The holder is not necessarily the spawner.
            if dir
                .parent()
                .is_some_and(|d| d.ends_with("subagent-transcripts"))
            {
                header["archiveOwner"] = json!(dir
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .and_then(|s| s.to_str()));
            }
            let index = dir.join("subagent-transcripts.json");
            if let Some(value) = document(&index, limit)? {
                header["archiveIndex"] = value;
            }
            sidecars.extend([meta, partial, index, dir.join("chat-archive.jsonl")]);
            let Some(id) = folder else {
                *unidentified += 1;
                return Ok(());
            };
            id.to_owned()
        }
        Provider::Cursor => {
            let Some(id) = folder.filter(|id| name == format!("{id}.jsonl")) else {
                *unidentified += 1;
                return Ok(());
            };
            let projects = dir.parent().and_then(Path::parent).and_then(Path::parent);
            if !projects.is_some_and(|d| d.ends_with("projects")) {
                *unidentified += 1;
                return Ok(());
            }
            id.to_owned()
        }
        _ => return Err(Error::Invalid("not a journal provider".into())),
    };
    let mut revision = stamp(p)?;
    for sidecar in sidecars {
        revision.push_str(&stamp(&sidecar)?);
    }
    visitor(Source {
        provider: input.provider,
        path: p.clone(),
        segment: session.clone(),
        session,
        header,
        stamp: revision,
    })
}
