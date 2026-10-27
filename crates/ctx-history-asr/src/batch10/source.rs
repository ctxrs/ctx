use super::document;
use crate::{source::Source, spool::Spool, Error, Input, Provider, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn paths(path: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(path)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}
fn stamp(path: &Path, hash: &mut Sha256) -> Result<()> {
    if path.is_dir() {
        for p in paths(path)? {
            if !p.is_symlink() {
                stamp(&p, hash)?;
            }
        }
    } else {
        let m = fs::metadata(path)?;
        hash.update(path.file_name().unwrap_or_default().as_encoded_bytes());
        hash.update(m.len().to_le_bytes());
        hash.update(format!("{:?}", m.modified()?));
    }
    Ok(())
}
fn source_stamp(path: &Path, p: Provider) -> Result<Sha256> {
    let mut hash = Sha256::new();
    stamp(path, &mut hash)?;
    if p == Provider::CodeBuddy && path.file_name().is_some_and(|s| s == "index.json") {
        stamp(&path.with_file_name("messages"), &mut hash)?;
    }
    if p == Provider::RovoDev && path.with_file_name("metadata.json").is_file() {
        stamp(&path.with_file_name("metadata.json"), &mut hash)?;
    }
    Ok(hash)
}
fn arrays(p: Provider) -> &'static [&'static str] {
    match p {
        Provider::Continue => &["history"],
        Provider::Auggie => &["chatHistory", "chat_history"],
        Provider::RovoDev => &[
            "message_history",
            "messages",
            "session_context/message_history",
            "conversation/messages",
        ],
        _ => &[],
    }
}
fn header(path: &Path, p: Provider, limit: usize) -> Result<Option<(String, Value)>> {
    let mut n = match p {
        Provider::OpenHands => {
            let state = path.join("base_state.json");
            if state.is_file() {
                document::read(&state, &[], limit, &mut |_, _, _| Ok(()))?
            } else {
                json!({})
            }
        }
        Provider::CodeBuddy if path.extension().is_some_and(|e| e == "jsonl") => {
            let mut r = crate::source::reader(path)?;
            let mut b = Vec::new();
            let mut n = json!({});
            for _ in 0..64 {
                let Some((_, large)) = crate::source::line(&mut *r, &mut b, limit)? else {
                    break;
                };
                if !large {
                    if let Ok(v) = document::decode(&b) {
                        if v["sessionId"].is_string() {
                            n = v;
                            break;
                        }
                    }
                }
            }
            n
        }
        Provider::CodeBuddy => document::whole(path, limit)?,
        _ => document::read(path, arrays(p), limit, &mut |_, _, _| Ok(()))?,
    };
    if p == Provider::RovoDev {
        let side = path.with_file_name("metadata.json");
        if side.is_file() {
            n["_ctx_metadata"] = document::whole(&side, limit)?;
        }
    }
    let id = match p {
        Provider::OpenHands => n["id"]
            .as_str()
            .or_else(|| path.file_name().and_then(|s| s.to_str())),
        Provider::CodeBuddy if path.extension().is_some_and(|e| e == "jsonl") => n["sessionId"]
            .as_str()
            .or_else(|| path.file_stem().and_then(|s| s.to_str())),
        Provider::CodeBuddy => path
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str()),
        Provider::RovoDev => n
            .pointer("/_ctx_metadata/session_id")
            .or_else(|| n.pointer("/_ctx_metadata/sessionId"))
            .and_then(Value::as_str)
            .or_else(|| super::string(&n, &["session_id", "sessionId"]))
            .or_else(|| {
                path.parent()
                    .and_then(Path::file_name)
                    .and_then(|s| s.to_str())
            }),
        _ => super::string(&n, &["sessionId", "session_id"]),
    }
    .filter(|s| !s.is_empty())
    .map(str::to_owned);
    let recognized = match p {
        Provider::Continue => n.get("_ctx_count_history").is_some(),
        Provider::Auggie => n
            .get("_ctx_count_chatHistory")
            .or_else(|| n.get("_ctx_count_chat_history"))
            .is_some(),
        Provider::RovoDev => n
            .get("_ctx_count_message_history")
            .or_else(|| n.get("_ctx_count_messages"))
            .or_else(|| n.pointer("/session_context/_ctx_count_message_history"))
            .or_else(|| n.pointer("/conversation/_ctx_count_messages"))
            .is_some(),
        _ => true,
    };
    Ok(id.filter(|_| recognized).map(|id| (id, n)))
}
pub(crate) fn root(p: Provider, path: &Path) -> PathBuf {
    if p == Provider::OpenHands {
        for ancestor in path.ancestors() {
            if ancestor
                .parent()
                .is_some_and(|p| p.file_name().is_some_and(|s| s == "v1_conversations"))
            {
                return ancestor.to_owned();
            }
        }
        if path.is_file()
            && path
                .parent()
                .is_some_and(|p| p.file_name().is_some_and(|s| s == "events"))
        {
            if let Some(root) = path.parent().and_then(Path::parent) {
                return root.to_owned();
            }
        }
    }
    path.to_owned()
}
fn event_files(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for p in paths(path)? {
        if p.is_symlink() {
            continue;
        }
        if p.is_dir() {
            event_files(&p, out)?;
        } else if p.extension().is_some_and(|s| s == "json")
            && p.file_name().is_some_and(|s| s != "base_state.json")
        {
            out.push(p);
        }
    }
    Ok(())
}
pub(crate) fn visit(
    input: &Input,
    limit: usize,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    let p = input.provider;
    let path = &input.path;
    if !path.exists() {
        return Ok(());
    }
    if path.is_symlink() {
        return visit(
            &Input {
                provider: p,
                path: fs::canonicalize(path)?,
            },
            limit,
            visitor,
            unidentified,
        );
    }
    let conversation = p == Provider::OpenHands
        && path.is_dir()
        && (path.join("events").is_dir()
            || path
                .parent()
                .is_some_and(|p| p.file_name().is_some_and(|s| s == "v1_conversations")));
    if path.is_dir() && !conversation {
        for child in paths(path)? {
            if !child.is_symlink() {
                visit(
                    &Input {
                        provider: p,
                        path: child,
                    },
                    limit,
                    visitor,
                    unidentified,
                )?;
            }
        }
        return Ok(());
    }
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let candidate = match p {
        Provider::OpenHands => conversation,
        Provider::CodeBuddy => {
            name.ends_with(".jsonl")
                || name == "index.json"
                    && path.parent().is_some_and(|p| p.join("messages").is_dir())
        }
        Provider::RovoDev => name == "session_context.json",
        _ => name.ends_with(".json") && name != "sessions.json",
    };
    if !candidate {
        return Ok(());
    }
    let Some((session, header)) = header(path, p, limit)? else {
        *unidentified += 1;
        return Ok(());
    };
    let hash = source_stamp(path, p)?;
    visitor(Source {
        provider: p,
        path: path.clone(),
        segment: session.clone(),
        session,
        header,
        stamp: format!("{:x}", hash.finalize()),
    })
}

pub(crate) fn ingest(spool: &mut Spool, path: &Path, p: Provider) -> Result<()> {
    let before = source_stamp(path, p)?;
    match p {
        Provider::OpenHands => {
            let mut files = Vec::new();
            if path.join("events").is_dir() {
                files = paths(&path.join("events"))?;
            } else {
                event_files(path, &mut files)?;
            }
            files.sort_by_key(|p| {
                (
                    p.file_name()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.strip_prefix("event-"))
                        .and_then(|s| s.split('-').next())
                        .and_then(|s| s.parse::<u64>().ok()),
                    p.clone(),
                )
            });
            for file in files {
                if file.extension().is_some_and(|s| s == "json") && file.is_file() {
                    spool.source_bytes += fs::metadata(&file)?.len();
                    match document::whole(&file, spool.limit) {
                        Ok(v) => {
                            let id = super::string(&v, &["id"]).map(str::to_owned);
                            spool.insert_document_item("event", v, id, false)?;
                        }
                        Err(_) => spool.insert_document_item(
                            "gap",
                            json!({"reason":"unparseable"}),
                            None,
                            false,
                        )?,
                    }
                }
            }
            let state = path.join("base_state.json");
            if state.is_file() {
                spool.source_bytes += fs::metadata(&state)?.len();
                let n = document::read(&state, &[], spool.limit, &mut |_, _, _| Ok(()))?;
                spool.insert_document_item("state", n, Some("base_state".into()), true)?;
            }
        }
        Provider::CodeBuddy if path.extension().is_some_and(|s| s == "jsonl") => {
            let mut r = crate::source::reader(path)?;
            let mut b = Vec::new();
            while let Some((size, large)) = crate::source::line(&mut *r, &mut b, spool.limit)? {
                spool.source_bytes += size as u64;
                if b.iter().all(u8::is_ascii_whitespace) && !large {
                    continue;
                }
                let v = if large {
                    None
                } else {
                    document::decode(&b).ok()
                };
                if let Some(v) = v {
                    let id = super::string(&v, &["id"]).map(str::to_owned);
                    spool.insert_document_item("cli", v, id, false)?;
                } else {
                    spool.insert_document_item(
                        "gap",
                        json!({"reason":if large{"conversion_loss"}else{"unparseable"}}),
                        None,
                        true,
                    )?;
                }
            }
        }
        Provider::CodeBuddy => {
            let n = document::whole(path, spool.limit)?;
            spool.source_bytes += fs::metadata(path)?.len();
            for reference in n["messages"].as_array().into_iter().flatten() {
                let Some(id) = reference["id"].as_str().filter(|s| {
                    !s.is_empty() && !s.contains(['/', '\\']) && *s != ".." && *s != "."
                }) else {
                    return Err(Error::Invalid("invalid CodeBuddy message file ID".into()));
                };
                let file = path.with_file_name("messages").join(format!("{id}.json"));
                match document::whole(&file, spool.limit) {
                    Ok(mut v) => {
                        spool.source_bytes += fs::metadata(&file)?.len();
                        v["_ctx_reference"] = reference.clone();
                        spool.insert_document_item("ide", v, Some(id.into()), true)?;
                    }
                    Err(_) => spool.insert_document_item(
                        "gap",
                        json!({"reason":"not_retained","messageId":id}),
                        None,
                        true,
                    )?,
                }
            }
            spool.insert_document_item("metadata", n, Some("catalog".into()), true)?;
        }
        _ => {
            spool.source_bytes += fs::metadata(path)?.len();
            let limit = spool.limit;
            let n = document::read(
                path,
                arrays(p),
                limit,
                &mut |_, _, raw| match document::decode(raw) {
                    Ok(v) => {
                        let id = match p {
                            Provider::Continue => super::string(&v["message"], &["id"]),
                            Provider::Auggie => super::string(
                                v.get("exchange").unwrap_or(&v),
                                &["request_id", "requestId"],
                            ),
                            _ => super::string(&v, &["id", "message_id"]),
                        }
                        .map(str::to_owned);
                        spool.insert_document_item("item", v, id, true)
                    }
                    Err(_) => spool.insert_document_item(
                        "gap",
                        json!({"reason":"unparseable"}),
                        None,
                        true,
                    ),
                },
            )?;
            spool.insert_document_item("metadata", n, Some("document".into()), true)?;
            if p == Provider::RovoDev {
                let side = path.with_file_name("metadata.json");
                if side.is_file() {
                    spool.source_bytes += fs::metadata(&side)?.len();
                    let n = document::whole(&side, limit)?;
                    spool.insert_document_item("metadata", n, Some("metadata".into()), true)?;
                }
            }
        }
    }
    let after = source_stamp(path, p)?;
    if before.finalize() != after.finalize() {
        return Err(Error::Invalid(
            "native source changed during ASR capture; retry export".into(),
        ));
    }
    Ok(())
}
