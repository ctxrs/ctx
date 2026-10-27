use crate::{Error, Input, Provider, Result};
use serde_json::Value;
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

pub(crate) fn reader(path: &Path) -> Result<Box<dyn BufRead>> {
    let file = File::open(path)?;
    if path.extension().is_some_and(|e| e == "zst" || e == "zstd") {
        Ok(Box::new(BufReader::new(zstd::stream::read::Decoder::new(
            file,
        )?)))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

/// Drains over-bound lines without retaining them. Incomplete tails are not items.
pub(crate) fn line(
    reader: &mut dyn BufRead,
    bytes: &mut Vec<u8>,
    limit: usize,
) -> Result<Option<(usize, bool)>> {
    bytes.clear();
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(None);
        }
        let end = available.iter().position(|b| *b == b'\n');
        let n = end.map_or(available.len(), |i| i + 1);
        total = total
            .checked_add(n)
            .ok_or_else(|| Error::Invalid("native line size overflow".into()))?;
        if bytes.len() < limit {
            bytes.extend_from_slice(&available[..n.min(limit - bytes.len())]);
        }
        reader.consume(n);
        if end.is_some() {
            return Ok(Some((total, total > limit)));
        }
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Option<Value> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(ctx_history_capture_model::exact_json_value)
        .filter(Value::is_object)
}

#[derive(Clone)]
pub(crate) struct Source {
    pub provider: Provider,
    pub path: PathBuf,
    pub session: String,
    pub header: Value,
    pub stamp: String,
    pub segment: String,
}

pub(crate) fn visit(
    input: &Input,
    limit: usize,
    staging: &Path,
    namespace: Option<&str>,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    if input.provider.journal() {
        return crate::journals::source::visit(input, limit, visitor, unidentified);
    }
    if input.path.is_file() {
        if input.path.is_symlink() {
            return visit(
                &Input {
                    provider: input.provider,
                    path: fs::canonicalize(&input.path)?,
                },
                limit,
                staging,
                namespace,
                visitor,
                unidentified,
            );
        }
        return if input.provider.is_sqlite() {
            crate::sqlite::visit(input, limit, staging, namespace, visitor, unidentified)
        } else {
            inspect(input, limit, visitor, unidentified)
        };
    }
    if !input.path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&input.path)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            visit(
                &Input {
                    provider: input.provider,
                    path: entry.path(),
                },
                limit,
                staging,
                namespace,
                visitor,
                unidentified,
            )?;
        } else if ty.is_file() || ty.is_symlink() {
            let mut path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !(name.ends_with(".jsonl")
                || (input.provider == Provider::Codex && name.ends_with(".jsonl.zst"))
                || (input.provider == Provider::DeepSeekHarness && name.ends_with(".jsonl.zstd"))
                || (input.provider == Provider::Gemini && name.ends_with(".json"))
                || (input.provider.is_sqlite() && (name.ends_with(".db") || name.ends_with(".sqlite") || name.ends_with(".sqlite3"))))
            {
                continue;
            }
            if ty.is_symlink() {
                if !path.is_file() {
                    *unidentified += 1;
                    continue;
                }
                // Alias filenames are not another native session/segment identity.
                path = fs::canonicalize(path)?;
            }
            if (name.ends_with(".jsonl.zst") || name.ends_with(".jsonl.zstd"))
                && path.with_extension("").is_file()
            {
                continue;
            }
            visit(
                &Input {
                    provider: input.provider,
                    path,
                },
                limit,
                staging,
                namespace,
                visitor,
                unidentified,
            )?;
        }
    }
    Ok(())
}

fn inspect(
    input: &Input,
    limit: usize,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    if matches!(
        input.provider,
        Provider::Gemini | Provider::Antigravity | Provider::QwenCode
    ) {
        return crate::conversation_source::inspect(input, limit, visitor, unidentified);
        }
    if input.provider.cli_journal() {
        return crate::cli_journals::source::inspect(input, limit, visitor, unidentified);
    }
    let mut input_reader = reader(&input.path)?;
    let mut buffer = Vec::new();
    let mut metas = Vec::new();
    let mut claude = None;
    for _ in 0..64 {
        let Some((_, oversized)) = line(&mut *input_reader, &mut buffer, limit)? else {
            break;
        };
        if oversized {
            continue;
        }
        let Some(v) = decode(&buffer) else {
            continue;
        };
        match input.provider {
            Provider::Codex if v["type"] == "session_meta" => metas.push(v),
            Provider::Claude if v["sessionId"].as_str().is_some_and(|s| !s.is_empty()) => {
                claude = Some((String::new(), v));
                break;
            }
            Provider::Pi | Provider::GrokBuild | Provider::DeepSeekHarness => {
                if let Some(id) = crate::jsonl::session_id(input.provider, &v) {
                    claude = Some((id, v));
                    break;
                }
            }
            _ => {}
        }
    }
    let (session, mut header) = match input.provider {
        Provider::Claude => {
            let Some((_, v)) = claude else {
                *unidentified += 1;
                return Ok(());
            };
            let id = v["agentId"]
                .as_str()
                .filter(|_| {
                    v["isSidechain"] == true
                        || input
                            .path
                            .parent()
                            .is_some_and(|p| p.ends_with("subagents"))
                })
                .or_else(|| v["sessionId"].as_str())
                .ok_or_else(|| Error::Invalid("missing native Claude identity".into()))?;
            (id.to_owned(), v)
        }
        Provider::Gemini | Provider::Antigravity | Provider::QwenCode => unreachable!(),
        Provider::Codex => {
            use ctx_history_provider_codex::codex::catalog::{
                codex_canonical_session_id_from_path, select_native_session_metadata,
            };
            let expected = codex_canonical_session_id_from_path(&input.path);
            let selected = select_native_session_metadata(&metas, expected.as_deref());
            let selected = match selected {
                Ok(Some(v)) => v,
                Ok(None) | Err(ctx_history_provider_codex::CaptureError::InvalidPayload(_)) => {
                    *unidentified += 1;
                    return Ok(());
                }
                Err(e) => return Err(crate::Error::Provider(e)),
            };
            let v = &selected["payload"];
            let Some(id) = v["id"].as_str() else {
                *unidentified += 1;
                return Ok(());
            };
            (id.to_owned(), v.clone())
        }
        Provider::Pi | Provider::GrokBuild | Provider::DeepSeekHarness => {
            let Some(value) = claude else {
                *unidentified += 1;
                return Ok(());
            };
            value
        }
        _ => return Err(Error::Invalid("unexpected journal provider".into())),
    };
    if input.provider == Provider::Claude {
        let sidecar = input.path.with_extension("meta.json");
        if sidecar.is_file() {
            let mut bytes = Vec::new();
            File::open(sidecar)?
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() <= limit {
                if let Some(meta) = decode(&bytes) {
                    header["_sidecar"] = meta;
                }
            }
        }
    }
    let metadata = fs::metadata(&input.path)?;
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sidecar_stamp = fs::metadata(input.path.with_extension("meta.json"))
        .ok()
        .map(|m| format!("{}:{:?}", m.len(), m.modified().ok()))
        .unwrap_or_default();
    let parent_stamp = if input.provider == Provider::Pi {
        crate::jsonl::parent_header(&mut header, limit)?
    } else {
        String::new()
    };
    let stamp = format!(
        "{}:{modified}:{sidecar_stamp}:{parent_stamp}",
        metadata.len()
    );
    let segment = segment_id(&input.path, &session, input.provider);
    visitor(Source {
        provider: input.provider,
        path: input.path.clone(),
        session,
        header,
        stamp,
        segment,
    })
}

/// The registry's native rollout suffix is an ID; directories and timestamps are not.
fn segment_id(path: &Path, session: &str, provider: Provider) -> String {
    if provider == Provider::Codex {
        ctx_history_provider_codex::codex::catalog::codex_revert_rollout_id(path, session)
            .unwrap_or(session)
            .to_owned()
    } else {
        session.to_owned()
    }
}
