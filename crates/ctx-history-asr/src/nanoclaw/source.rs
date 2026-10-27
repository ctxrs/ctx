use super::P;
use crate::{
    native as n,
    records::{meta, session_ref},
    source::Source,
    spool::Spool,
    Error, Input, Result,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn central(path: &Path) -> PathBuf {
    if path.is_file() {
        path.to_owned()
    } else if path.join("data/v2.db").is_file() {
        path.join("data/v2.db")
    } else {
        path.join("v2.db")
    }
}
pub(crate) fn visit(
    input: &Input,
    limit: usize,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    let path = central(&input.path);
    if !path.is_file() {
        return Ok(());
    }
    let db = n::open(&path)?;
    if !n::has(&db, "sessions")? {
        *unidentified += 1;
        return Ok(());
    }
    n::scan(
        &db,
        "SELECT id,agent_group_id FROM sessions ORDER BY id",
        [],
        limit,
        |r| {
            if let (Some(id), Some(group)) = (r["id"].as_str(), r["agent_group_id"].as_str()) {
                if !n::safe_segment(id) || !n::safe_segment(group) {
                    return Err(Error::Invalid(
                        "NanoClaw session identity is not a path segment".into(),
                    ));
                }
                visitor(Source {
                    provider: P,
                    path: path.clone(),
                    session: id.into(),
                    header: json!({"group":group}),
                    stamp: String::new(),
                    segment: id.into(),
                })
            } else {
                *unidentified += 1;
                Ok(())
            }
        },
    )
}
pub(crate) fn ingest(s: &mut Spool, path: &Path, id: &str, h: &mut Value) -> Result<()> {
    let db = n::open(path)?;
    let session = n::one(&db, "SELECT * FROM sessions WHERE id=?1", [id], s.limit)?;
    let group = session["agent_group_id"]
        .as_str()
        .ok_or_else(|| Error::Invalid("NanoClaw session disappeared".into()))?;
    if !n::safe_segment(group) || !n::safe_segment(id) {
        return Err(Error::Invalid(
            "NanoClaw session identity is not a path segment".into(),
        ));
    }
    let dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .join("v2-sessions")
        .join(group)
        .join(id);
    h["asr"] = json!({});
    n::set_time(&mut h["asr"], "startedAt", &session["created_at"]);
    meta(&mut h["asr"], P, "agentGroupId", &json!(group));
    // No root/parent claim: catalog membership alone says neither.
    if n::has(&db, "messaging_groups")? {
        if let Some(g) = session["messaging_group_id"].as_str() {
            let row = n::one(
                &db,
                "SELECT * FROM messaging_groups WHERE id=?1",
                [g],
                s.limit,
            )?;
            if let Some(channel) = row["channel_type"]
                .as_str()
                .filter(|c| super::messages::channel(c))
            {
                let cid = n::stable_id(
                    &json!([
                        row["channel_type"],
                        row["platform_id"],
                        session["thread_id"]
                    ])
                    .to_string(),
                );
                let mut c = json!({"id":cid,"channel":channel,"nativeId":row["platform_id"]});
                if session["thread_id"].is_string() {
                    c["kind"] = json!("thread");
                } else if row["is_group"] == 1 {
                    c["kind"] = json!("group");
                } else if row["is_group"] == 0 {
                    c["kind"] = json!("direct");
                }
                n::retain(&mut c, P, "row", &row);
                h["asr"]["conversations"] = json!([c]);
            }
        }
    }
    let mut components = Vec::new();
    for (file, table) in [
        ("inbound.db", "messages_in"),
        ("outbound.db", "messages_out"),
    ] {
        let path = dir.join(file);
        if !path.is_file() {
            n::loss(s, "_dev.nanoclaw/missing_store")?;
            n::stage(s, "_gap", file, json!({"reason":"not_retained"}), false)?;
            continue;
        }
        let db = n::open(&path)?;
        if !n::has(&db, table)? {
            return Err(Error::Invalid(format!("NanoClaw {file} lacks {table}")));
        }
        components.push((db, table));
    }
    // Mutable IO rows, followed by other snapshots. No timestamp join between
    // stores: host's nextEvenSeq reads only inbound, so parity is not chronology.
    for (db, table) in &components {
        n::scan(
            db,
            &format!("SELECT * FROM {table} ORDER BY seq,id"),
            [],
            s.limit,
            |r| {
                if let Some(reason) = r["_gap"].as_str() {
                    return n::stage(
                        s,
                        "_gap",
                        &format!("{table}:{}", s.source_bytes),
                        json!({"reason":reason}),
                        true,
                    );
                }
                let key = r["id"]
                    .as_str()
                    .ok_or_else(|| Error::Invalid("NanoClaw message lacks its native ID".into()))?;
                let content = n::parse(&r["content"]);
                if let Some(q) = content["questionId"].as_str() {
                    if *table == "messages_in" {
                        let answer_key = format!("answer:{q}");
                        if s.entity(&answer_key)?.is_some() {
                            s.save(&answer_key, &json!({"ambiguous":true}))?;
                            n::loss(s, "_dev.nanoclaw/ambiguous_answers")?;
                        } else {
                            s.save(&answer_key, &json!({"id":key,"content":content}))?;
                        }
                    } else if content["type"] == "ask_question" {
                        s.save(&format!("question:{q}"), &json!(key))?;
                        if content["secret"] == true {
                            s.save(&format!("secret-question:{q}"), &json!(true))?;
                        }
                    }
                }
                n::stage(s, table, key, r.clone(), true)
            },
        )?;
    }
    n::stage(s, "sessions", id, session.clone(), true)?;
    if n::has(&db, "agent_groups")? {
        let row = n::one(
            &db,
            "SELECT * FROM agent_groups WHERE id=?1",
            [group],
            s.limit,
        )?;
        if !row.is_null() {
            n::stage(s, "agent_groups", group, row, true)?;
        }
    }
    if n::has(&db, "pending_questions")? {
        n::scan(
            &db,
            "SELECT * FROM pending_questions WHERE session_id=?1 ORDER BY question_id",
            [id],
            s.limit,
            |r| {
                n::stage(
                    s,
                    "pending_questions",
                    r["question_id"].as_str().unwrap_or(""),
                    r.clone(),
                    true,
                )
            },
        )?;
    }
    for (db, table) in &components {
        let tails: &[(&str, &str)] = if *table == "messages_in" {
            &[
                ("delivered", "message_out_id"),
                ("destinations", "name"),
                ("session_routing", "id"),
            ]
        } else {
            &[
                ("processing_ack", "message_id"),
                ("session_state", "key"),
                ("container_state", "id"),
            ]
        };
        for (table, key) in tails {
            if !n::has(db, table)? {
                continue;
            }
            n::scan(
                db,
                &format!("SELECT * FROM {table} ORDER BY {key}"),
                [],
                s.limit,
                |r| {
                    if *table == "session_state" {
                        // Opaque legacy sdk_session_id has no reliable provider; never
                        // infer it from today's configuration.
                        let provider = match r["key"].as_str() {
                            Some("continuation:claude") => Some(crate::Provider::Claude),
                            Some("continuation:codex") => Some(crate::Provider::Codex),
                            _ => None,
                        };
                        if let (Some(p), Some(id)) = (provider, r["value"].as_str()) {
                            let binding =
                                json!({"session":session_ref(p,id),"evidence":"native_field"});
                            if let Some(a) = h["asr"]["lineage"]["executedBy"].as_array_mut() {
                                a.push(binding);
                            } else {
                                h["asr"]["lineage"]["executedBy"] = json!([binding]);
                            }
                        }
                    }
                    n::stage(s, table, &r[*key].to_string(), r.clone(), true)
                },
            )?;
        }
    }
    for loss in [
        "inference",
        "usage",
        "cost",
        "reasoning",
        "tool_result.content",
        "_dev.nanoclaw/model_prompt",
        "_dev.nanoclaw/backing_transcripts",
        "_dev.nanoclaw/current_state_only",
    ] {
        n::loss(s, loss)?;
    }
    Ok(())
}
