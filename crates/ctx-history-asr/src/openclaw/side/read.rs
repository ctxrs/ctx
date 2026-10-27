use super::super::P;
use crate::{
    native as n,
    records::{meta, session_ref},
    spool::Spool,
    Result,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

pub(crate) fn ingest(
    s: &mut Spool,
    db: &Connection,
    path: &Path,
    id: &str,
    scope: (&str, &str),
    h: &mut Value,
    bundle: &Connection,
) -> Result<()> {
    let (lane, agent) = scope;
    let shared = path
        .ancestors()
        .find(|p| p.file_name().is_some_and(|s| s == "agents"))
        .and_then(Path::parent)
        .map(|p| p.join("state/openclaw.sqlite"));
    let state = shared
        .as_ref()
        .filter(|p| p.is_file())
        .map(|p| n::open(p))
        .transpose()?;
    // Immutable logs serialize after the transcript in their own native order.
    if let Some(state) = state.as_ref() {
        scoped(
            s,
            state,
            "audit_events",
            "session_id",
            id,
            "sequence",
            false,
        )?;
        if n::has(state, "skill_workshop_changes")? {
            let order = if n::columns(state, "skill_workshop_changes")?
                .iter()
                .any(|c| c == "cursor_id")
            {
                "cursor_id"
            } else {
                "created_at_ms,rowid"
            };
            n::scan(state,&format!("SELECT * FROM skill_workshop_changes WHERE agent_id=?1 AND session_key=?2 ORDER BY {order}"),[agent,lane],s.limit,|r|{
                if owned(s,&r["run_id"])?{n::stage(s,"skill_workshop_changes",r["change_id"].as_str().unwrap_or(""),r.clone(),false)?;}else{n::lane_fact(bundle,P,lane,"skill_workshop_changes",r["change_id"].as_str().unwrap_or(""),&r)?;n::loss(s,"_ai.openclaw/unassigned_facts")?;}Ok(())
            })?;
        }
    }
    scoped(
        s,
        db,
        "trajectory_runtime_events",
        "session_id",
        id,
        "seq",
        false,
    )?;
    // Mutable rows always follow every source-positioned record.
    if let Some(state) = state.as_ref() {
        if n::has(state, "operator_approvals")? {
            n::scan(state,"SELECT approval_id,source_session_id,source_run_id,source_tool_call_id,status,decision,terminal_reason,presentation_json,created_at_ms,expires_at_ms,resolved_at_ms,resolver_kind,resolver_id,consumed_at_ms,consumed_by FROM operator_approvals WHERE source_session_id=?1 ORDER BY approval_id",[id],s.limit,|r|{
                if let Some(call)=r["source_tool_call_id"].as_str(){s.save(&format!("approval-call:{call}"),&json!(true))?;}
                n::stage(s,"operator_approvals",r["approval_id"].as_str().unwrap_or(""),r.clone(),true)
            })?;
        }
        if n::has(state, "delivery_queue_entries")? {
            n::scan(
                state,
                "SELECT * FROM delivery_queue_entries WHERE session_key=?1 ORDER BY id",
                [lane],
                s.limit,
                |r| {
                    let e = n::parse(&r["entry_json"]);
                    let owner = e["sessionGeneration"]["sessionId"].as_str().or_else(|| {
                        (e["deliveryCompletion"]["kind"] == "pending-final")
                            .then(|| e["deliveryCompletion"]["sessionId"].as_str())
                            .flatten()
                    });
                    let key = r["id"].as_str().unwrap_or("");
                    if let Some(owner) = owner {
                        s.save(&format!("queue-owner:{key}"), &json!(owner))?;
                        if owner == id {
                            s.save(&format!("queue:{key}"), &r)?;
                            n::stage(s, "delivery_queue_entries", key, r.clone(), true)?;
                        }
                    } else {
                        n::lane_fact(bundle, P, lane, "delivery_queue_entries", key, &r)?;
                        n::loss(s, "_ai.openclaw/unassigned_facts")?;
                    }
                    Ok(())
                },
            )?;
        }
        if n::has(state, "cron_run_receipts")? {
            let order = if n::columns(state, "cron_run_receipts")?
                .iter()
                .any(|c| c == "cursor_id")
            {
                "cursor_id"
            } else {
                "started_at_ms,rowid"
            };
            n::scan(
                state,
                &format!("SELECT * FROM cron_run_receipts WHERE agent_id=?1 ORDER BY {order}"),
                [agent],
                s.limit,
                |r| {
                    if owned(s, &r["request_run_id"])? {
                        s.save(
                            &format!("job:{}", r["job_id"].as_str().unwrap_or("")),
                            &json!(true),
                        )?;
                        n::stage(
                            s,
                            "cron_run",
                            r["receipt_id"].as_str().unwrap_or(""),
                            r.clone(),
                            true,
                        )?;
                    }
                    Ok(())
                },
            )?;
        }
        if n::has(state, "cron_jobs")? {
            n::scan(
                state,
                "SELECT * FROM cron_jobs WHERE agent_id=?1 ORDER BY store_key,job_id",
                [agent],
                s.limit,
                |r| {
                    let key = json!([r["store_key"], r["job_id"]]).to_string();
                    if s.entity(&format!("job:{}", r["job_id"].as_str().unwrap_or("")))?
                        .is_some()
                    {
                        let job = n::parse(&r["job_json"]);
                        n::stage(
                            s,
                            "cron_job",
                            &key,
                            json!({"jobId":r["job_id"],"name":r["name"],"enabled":r["enabled"],"schedule":job["schedule"],"sessionTarget":job["sessionTarget"]}),
                            true,
                        )?;
                    }
                    if n::has(state, "config_machine_state")? {
                        let receipt = n::one(
                            state,
                            "SELECT value_json FROM config_machine_state WHERE state_key=?1",
                            [format!(
                                "automation-default:{}:{agent}",
                                r["store_key"].as_str().unwrap_or("")
                            )],
                            s.limit,
                        )?;
                        {
                            let row = receipt;
                            let receipt = n::parse(&row["value_json"]);
                            if receipt["phase"] == "complete"
                                && receipt.get("heartbeatEnabled").is_some()
                            {
                                s.save(
                                    &format!(
                                        "heartbeat-job:{}",
                                        receipt["jobId"].as_str().unwrap_or("")
                                    ),
                                    &receipt["provisionedAtMs"],
                                )?;
                            }
                        }
                    }
                    Ok(())
                },
            )?;
        }
        if n::has(state, "plugin_state_entries")? {
            n::scan(state,"SELECT value_json FROM plugin_state_entries WHERE plugin_id='codex' AND namespace='app-server-thread-bindings' AND entry_key=?1",[format!("session:{agent}:{id}")],s.limit,|r|{
                let v=n::parse(&r["value_json"]);if v["state"]=="active"&&(v["sessionId"].is_null()||v["sessionId"]==id){if let Some(thread)=v["binding"]["threadId"].as_str(){h["asr"]["lineage"]["executedBy"]=json!([{"session":session_ref(crate::Provider::Codex,thread),"evidence":"native_field"}]);}}Ok(())
            })?;
        }
    } else {
        n::loss(s, "_ai.openclaw/shared_state_unavailable")?;
    }
    if n::has(db, "conversation_deliveries")? {
        n::scan(db,"SELECT * FROM conversation_deliveries WHERE source_session_key=?1 ORDER BY operation_id",[lane],s.limit,|r|{
            let queue=r["queue_id"].as_str().unwrap_or("");let owner=s.entity(&format!("queue-owner:{queue}"))?;
            if owner.as_ref().and_then(Value::as_str)==Some(id){s.save(&format!("delivered-queue:{queue}"),&json!(true))?;n::stage(s,"conversation_deliveries",r["operation_id"].as_str().unwrap_or(""),r.clone(),true)?;}
            else if owner.is_none(){n::lane_fact(bundle,P,lane,"conversation_deliveries",r["operation_id"].as_str().unwrap_or(""),&r)?;n::loss(s,"_ai.openclaw/unassigned_facts")?;}Ok(())
        })?;
    }
    for (table, order) in [
        ("session_reactions", "message_id,identity_id,emoji"),
        ("session_pending_inputs", "rowid"),
        ("session_input_completions", "rowid"),
        ("session_progress_cards", "rowid"),
        ("session_goal_operations", "rowid"),
        ("message_tool_run_outcomes", "rowid"),
    ] {
        scoped(s, db, table, "session_id", id, order, true)?;
    }
    if n::has(db, "session_transcript_cold_archives")? {
        let cold = n::one(
            db,
            "SELECT session_id FROM session_transcript_cold_archives WHERE session_id=?1",
            [id],
            s.limit,
        )?;
        if !cold.is_null() {
            n::stage(s, "cold_archive", id, cold, true)?;
        }
    }
    for table in ["session_progress_cards", "heartbeat_outcomes"] {
        if n::has(db, table)? {
            n::scan(
                db,
                &format!("SELECT * FROM {table} WHERE session_key=?1"),
                [lane],
                s.limit,
                |r| {
                    n::lane_fact(bundle, P, lane, table, lane, &r)?;
                    n::loss(s, "_ai.openclaw/unassigned_facts")
                },
            )?;
        }
    }
    let node = s.entity("node")?.unwrap_or(Value::Null);
    if let Some(claude) = node["claudeCliSessionId"]
        .as_str()
        .or(node["cliSessionBindings"]["claude-cli"]["sessionId"].as_str())
    {
        let binding = json!({"session":session_ref(crate::Provider::Claude,claude),"evidence":"native_field"});
        if let Some(a) = h["asr"]["lineage"]["executedBy"].as_array_mut() {
            a.push(binding);
        } else {
            h["asr"]["lineage"]["executedBy"] = json!([binding]);
        }
    }
    let window = s.entity("window")?.unwrap_or(Value::Null);
    if window["agent_harness_id"]
        .as_str()
        .is_some_and(|h| h != "openclaw")
    {
        for loss in [
            "inference",
            "usage",
            "cost",
            "_ai.openclaw/backing_harness_detail",
        ] {
            n::loss(s, loss)?;
        }
    }
    let mut subagent = json!({});
    for k in ["spawnDepth", "subagentRole", "subagentControlScope"] {
        crate::records::copy(&mut subagent, k, &node, k);
    }
    if subagent.as_object().is_some_and(|o| !o.is_empty()) {
        meta(&mut h["asr"], P, "subagent", &subagent);
    }
    Ok(())
}
fn owned(s: &Spool, run: &Value) -> Result<bool> {
    Ok(if let Some(id) = run.as_str() {
        s.entity(&format!("owned-run:{id}"))?.is_some()
    } else {
        false
    })
}
fn scoped(
    s: &mut Spool,
    db: &Connection,
    table: &str,
    column: &str,
    id: &str,
    order: &str,
    snapshot: bool,
) -> Result<()> {
    if !n::has(db, table)? || !n::columns(db, table)?.iter().any(|c| c == column) {
        return Ok(());
    }
    // Source table names are fixed above. rowid is scoped to this native table,
    // never interpreted as a cross-store clock.
    let cols = n::columns(db, table)?;
    let order = if order == "seq" && !cols.iter().any(|c| c == "seq") {
        "rowid"
    } else {
        order
    };
    let mut stmt = db.prepare(&format!("PRAGMA table_info({table})"))?;
    let keys = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(5)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut keys: Vec<_> = keys.into_iter().filter(|(pk, _)| *pk > 0).collect();
    keys.sort();
    let fields = if cols.iter().any(|c| c == "event_payload") {
        format!(
            "{}, event_payload IS NOT NULL AS _compressed",
            cols.iter()
                .filter(|c| c.as_str() != "event_payload")
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(",")
        )
    } else {
        "*".to_owned()
    };
    n::scan(
        db,
        &format!("SELECT rowid AS _rowid,{fields} FROM {table} WHERE {column}=?1 ORDER BY {order}"),
        [id],
        s.limit,
        |mut r| {
            let key = if keys.is_empty() {
                r["_captureIdentity"] = json!(true);
                r["_rowid"].to_string()
            } else {
                json!(keys
                    .iter()
                    .map(|(_, k)| json!([k, r[k]]))
                    .collect::<Vec<_>>())
                .to_string()
            };
            if r["_compressed"] == 1 {
                n::loss(s, "_ai.openclaw/compressed_runtime")?;
                return n::stage(
                    s,
                    "_gap",
                    &format!("{table}:{key}"),
                    json!({"reason":"conversion_loss"}),
                    snapshot,
                );
            }
            n::stage(s, table, &key, r, snapshot)
        },
    )
}
