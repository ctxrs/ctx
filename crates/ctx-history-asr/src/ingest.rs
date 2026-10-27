//! Journal acquisition owns native field indexing; the spool only stores rows.
use crate::{source, spool::Spool, Provider, Result};
use rusqlite::params;
use serde_json::{json, Value};
use std::{
    io::{Seek, Write},
    path::Path,
};
pub(crate) fn ingest(
    spool: &mut Spool,
    path: &Path,
    provider: Provider,
    segment: &str,
    allocate: &mut dyn FnMut(&str) -> Result<u64>,
) -> Result<()> {
    if matches!(
        provider,
        Provider::Gemini | Provider::Antigravity | Provider::QwenCode
    ) {
        return crate::conversation_source::ingest(spool, path, provider, segment, allocate);
    }
    if provider.journal() {
        return crate::journals::ingest::ingest(spool, path, provider, segment);
    }
    if provider.batch10() {
        return crate::batch10::source::ingest(spool, path, provider);
    }
    let mut reader = source::reader(path)?;
    let mut bytes = Vec::new();
    let mut physical = 0;
    // The native SQLite cursor bounds values before decoding; its envelope and
    // JSON escaping add bytes but are not another native truncation boundary.
    let read_limit = if provider.is_sqlite() {
        spool.limit.saturating_mul(8).saturating_add(65536)
    } else {
        spool.limit
    };
    while let Some((size, oversized)) = source::line(&mut *reader, &mut bytes, read_limit)? {
        physical += 1;
        spool.source_bytes += size as u64;
        spool.source_lines += 1;
        if bytes.iter().all(u8::is_ascii_whitespace) && !oversized {
            continue;
        }
        let value = (!oversized).then(|| source::decode(&bytes)).flatten();
        let v = value.unwrap_or_else(|| json!({"type":"_gap", "reason":if oversized {"conversion_loss"} else {"unparseable"}}));
        if provider == Provider::Copilot {
            let child = serde_json::from_str::<(String, String)>(segment).ok();
            if v["agentId"].as_str() != child.as_ref().map(|(_, id)| id.as_str()) {
                continue;
            }
        }
        let local = if provider.cli_journal() {
            crate::cli_journals::local(provider, &v, physical)
        } else if provider == Provider::Codex {
            v["ordinal"]
                .as_u64()
                .map(|n| format!("o{n}"))
                .unwrap_or_else(|| format!("l{physical}"))
        } else if matches!(
            provider,
            Provider::Pi | Provider::GrokBuild | Provider::DeepSeekHarness
        ) {
            crate::jsonl::local(provider, &v, physical)
        } else {
            v["uuid"]
                .as_str()
                .map(|id| format!("uuid:{id}"))
                .unwrap_or_else(|| format!("l{physical}"))
        };
        let pos = allocate(&local)?;
        if provider == Provider::Codex {
            spool.db.execute(
                "INSERT OR REPLACE INTO native_positions VALUES(?1,?2,?3,?4)",
                params![
                    pos,
                    segment,
                    physical,
                    v.get("ordinal").map(Value::to_string)
                ],
            )?;
        }
        crate::privacy::questions(spool, &v)?;
        let off = spool.bytes.stream_position()?;
        let serialized = serde_json::to_vec(&v)?;
        spool.bytes.write_all(&serialized)?;
        let payload = if provider == Provider::Codex {
            &v["payload"]
        } else {
            &v
        };
        let kind = if provider == Provider::GrokBuild {
            crate::jsonl::update(&v)["sessionUpdate"].as_str()
        } else {
            v["type"].as_str()
        }
        .unwrap_or("unknown");
        let native = if provider == Provider::Codex {
            payload["id"].as_str()
        } else if provider == Provider::Pi {
            v["id"].as_str()
        } else if provider == Provider::GrokBuild {
            crate::jsonl::event_id(&v)
        } else {
            v["uuid"].as_str()
        };
        let response = if provider.is_sqlite() {
            v["message_id"].as_str()
        } else if kind == "assistant" {
            v.pointer("/message/id").and_then(Value::as_str)
        } else {
            None
        };
        let call = payload["call_id"].as_str();
        if provider.is_sqlite() {
            crate::opencode_source::index_calls(spool, &v, pos)?;
        }
        if provider == Provider::Codex
            && kind == "response_item"
            && matches!(
                payload["type"].as_str(),
                Some(
                    "function_call" | "custom_tool_call" | "local_shell_call" | "tool_search_call"
                )
            )
        {
            if let Some(id) = call.or_else(|| payload["id"].as_str()) {
                spool
                    .db
                    .execute("INSERT INTO native_calls VALUES(?1,?2)", params![id, pos])?;
            }
        }
        if provider == Provider::Claude {
            spool.db.execute(
                "DELETE FROM native_calls WHERE position>=?1 AND position<?2",
                params![
                    pos * crate::records::STRIDE,
                    (pos + 1) * crate::records::STRIDE
                ],
            )?;
            spool
                .db
                .execute("DELETE FROM tool_results WHERE position=?1", [pos])?;
            for (index, block) in v
                .pointer("/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                if block["type"] == "tool_use" {
                    if let Some(id) = block["id"].as_str() {
                        spool.db.execute(
                            "INSERT INTO native_calls VALUES(?1,?2)",
                            params![id, pos * crate::records::STRIDE + index as u64],
                        )?;
                    }
                }
                if block["type"] == "tool_result" {
                    if let Some(id) = block["tool_use_id"].as_str() {
                        spool
                            .db
                            .execute("INSERT INTO tool_results VALUES(?1,?2)", params![id, pos])?;
                    }
                }
            }
        }
        crate::jsonl::index(spool, provider, &v, pos)?;
        if provider.cli_journal() {
            crate::cli_journals::index(spool, provider, &v, pos)?;
        }
        // Preserve every observation; a native ordinal can repeat on distinct rows.
        spool.db.execute(
            "INSERT OR REPLACE INTO rows VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                pos,
                off,
                serialized.len(),
                kind,
                native,
                response,
                if provider == Provider::Pi {
                    v["parentId"].as_str()
                } else {
                    v["parentUuid"].as_str()
                },
                call,
                segment,
                if provider == Provider::DeepSeekHarness {
                    v["seq"].as_u64().or_else(|| v["seq0"].as_u64())
                } else {
                    v["ordinal"].as_u64()
                },
                spool.source_lines
            ],
        )?;
    }
    spool.bytes.flush()?;
    Ok(())
}
