mod read;
use super::P;
use crate::{
    native as n,
    records::{copy, meta, Writer},
    spool::Spool,
    Result,
};
pub(super) use read::ingest;
use serde_json::{json, Value};

pub(super) fn convert(s: &Spool, w: &mut Writer, h: &mut Value, v: &Value) -> Result<()> {
    let row = &v["row"];
    let table = v["table"].as_str().unwrap_or("");
    let mut r = match table {
        "operator_approvals" => permission(s, row)?,
        "skill_workshop_changes" => {
            let op = match row["action"].as_str() {
                Some("create") => Some("install"),
                Some("archive" | "remove_file") => Some("remove"),
                _ => None,
            };
            let mut r = json!({"type":"config_change","capability":{"kind":"skill","name":row["skill_name"]}});
            if let Some(op) = op {
                r["capability"]["op"] = json!(op);
            } else {
                meta(&mut r, P, "skillAction", &row["action"]);
            }
            copy(&mut r, "turnId", row, "run_id");
            n::set_time(&mut r, "ts", &row["created_at_ms"]);
            meta(
                &mut r,
                P,
                "skillChange",
                &json!({"actor":row["actor"],"versionId":row["version_id"],"summary":row["summary"]}),
            );
            r
        }
        "session_nodes" => {
            if row.is_null() {
                return Ok(());
            }
            let mut options = json!({});
            for key in [
                "permissionMode",
                "sandboxMode",
                "queueMode",
                "sendPolicy",
                "elevatedLevel",
                "ttsAuto",
            ] {
                copy(&mut options, key, row, key);
            }
            if options.as_object().is_some_and(|o| !o.is_empty()) {
                let mut config = json!({"type":"config_change","snapshot":true,"options":options});
                copy(&mut config, "permissionMode", row, "permissionMode");
                if let Some(mode) = row.get("sandboxMode") {
                    config["sandbox"] = json!({"mode":mode});
                }
                n::emit(s, w, config, v)?;
            }
            let goal = &row["goal"];
            if goal["id"].is_string() {
                let mut plan = json!({"type":"plan","planId":goal["id"],"planType":"goal","content":super::messages::parts(&goal["objective"])});
                copy(&mut plan, "status", goal, "status");
                if let Some(budget) = goal.get("tokenBudget") {
                    plan["budget"]["tokens"] = budget.clone();
                }
                if let Some(used) = goal.get("tokensUsed") {
                    plan["used"]["tokens"] = used.clone();
                }
                n::retain(&mut plan, P, "goal", goal);
                n::emit(s, w, plan, v)?;
            }
            let mut info = json!({"type":"session_info"});
            if let Some(title) = row.get("label").or(row.get("displayName")) {
                info["title"] = title.clone();
            }
            let mut node = json!({});
            for key in [
                "createdVia",
                "owner",
                "category",
                "icon",
                "snoozedUntil",
                "lastReadAt",
            ] {
                copy(&mut node, key, row, key);
            }
            meta(&mut info, P, "node", &node);
            info
        }
        "session_windows" => {
            let mut r = json!({"type":"session_info"});
            n::retain(&mut r, P, "window", row);
            r
        }
        "head" => {
            let Some(head) = super::structure::attachment(s, &row["leaf_event_id"])? else {
                return Ok(());
            };
            if head.is_null() {
                return Ok(());
            }
            json!({"type":"session_info","heads":[head]})
        }
        "session_reactions" => {
            let target = row["message_id"]
                .as_str()
                .map(|id| s.entity(&format!("message:{id}")))
                .transpose()?
                .flatten();
            let Some(target) = target else {
                n::loss(s, "_ai.openclaw/reaction_target")?;
                return n::vendor(s, w, v);
            };
            let fields = json!({"senderIdentity":{"type":"profile","id":row["identity_id"]},"senderName":row["identity_label"]});
            let mut r = json!({"type":"ai.openclaw/reaction","target":{"message":target["_record"]},"emoji":row["emoji"],"op":"add"});
            if let Some(p) = super::messages::participant(s, &fields, None)? {
                r["by"] = json!({"participant":p});
            }
            r
        }
        "conversation_deliveries" => {
            let status = match row["status"].as_str() {
                Some("created" | "queued") => "queued",
                Some("sent" | "replied") => "sent",
                Some("suppressed") => "suppressed",
                Some("rejected") => "failed",
                _ => "unknown",
            };
            let mut r = json!({"type":"delivery","status":status});
            copy(&mut r, "reason", row, "rejection_error");
            if let Some(id) = row["platform_message_id"].as_str() {
                r["platformMessageIds"] = json!([id]);
            }
            if h["conversations"]
                .as_array()
                .is_some_and(|cs| cs.iter().any(|c| c["id"] == row["conversation_id"]))
            {
                r["conversation"] = row["conversation_id"].clone();
            }
            if let Some(queue) = row["queue_id"].as_str() {
                if let Some(q) = s.entity(&format!("queue:{queue}"))? {
                    copy(&mut r, "attempt", &q, "retry_count");
                }
                meta(&mut r, P, "queueId", &json!(queue));
            }
            meta(&mut r, P, "operationId", &row["operation_id"]);
            r
        }
        "delivery_queue_entries" => {
            if s.entity(&format!(
                "delivered-queue:{}",
                row["id"].as_str().unwrap_or("")
            ))?
            .is_some()
            {
                return Ok(());
            }
            let mut r = json!({"type":"delivery","status":if row["status"]=="failed"{"failed"}else if row["recovery_state"]=="unknown_after_send"{"unknown"}else{"queued"}});
            copy(&mut r, "reason", row, "last_error");
            copy(&mut r, "attempt", row, "retry_count");
            meta(&mut r, P, "queueId", &row["id"]);
            r
        }
        "audit_events" if row["kind"] == "agent_run" => {
            let state = match row["action"].as_str() {
                Some("agent.run.started") => Some("running"),
                Some("agent.run.completed" | "agent.run.finished") => {
                    Some(if row["status"] == "failed" {
                        "error"
                    } else {
                        "idle"
                    })
                }
                _ => None,
            };
            let Some(state) = state else {
                return n::vendor(s, w, v);
            };
            let mut r = json!({"type":"state","state":state});
            copy(&mut r, "turnId", row, "run_id");
            n::set_time(&mut r, "ts", &row["occurred_at"]);
            r
        }
        "audit_events" if row["direction"] == "outbound" => {
            let mut r = json!({"type":"delivery","status":row["message_outcome"].as_str().filter(|s|["sent","suppressed","failed"].contains(s)).unwrap_or("unknown")});
            copy(&mut r, "turnId", row, "run_id");
            copy(&mut r, "reason", row, "reason_code");
            n::set_time(&mut r, "ts", &row["occurred_at"]);
            r
        }
        "cold_archive" => {
            n::loss(s, "_ai.openclaw/cold_archive")?;
            json!({"type":"gap","reason":"not_retained"})
        }
        _ => return n::vendor(s, w, v),
    };
    if table != "session_nodes" {
        n::retain(
            &mut r,
            P,
            if table == "audit_events" {
                "audit"
            } else {
                "row"
            },
            row,
        );
    }
    n::emit(s, w, r, v)?;
    Ok(())
}
fn permission(s: &Spool, row: &Value) -> Result<Value> {
    let mut r = json!({"type":"permission","requestId":row["approval_id"]});
    let decision = if row["status"] == "cancelled" {
        Some("cancelled")
    } else {
        match row["decision"].as_str() {
            Some("allow-once") => Some("allow_once"),
            Some("allow-always") => Some("allow_always"),
            Some("deny") => Some("reject_once"),
            _ => None,
        }
    };
    if let Some(d) = decision {
        r["decision"] = json!(d);
    }
    if row["terminal_reason"].is_null()
        || matches!(row["terminal_reason"].as_str(), Some("user" | "timeout"))
    {
        match row["status"].as_str() {
            Some("pending") => {
                r["status"] = json!("pending");
                r.as_object_mut().map(|o| o.remove("decision"));
            }
            Some("expired") => r["status"] = json!("expired"),
            Some("allowed" | "denied" | "cancelled") if decision.is_some() => {
                r["status"] = json!("resolved")
            }
            _ => {}
        }
    }
    copy(&mut r, "turnId", row, "source_run_id");
    if let Some(call) = row["source_tool_call_id"].as_str() {
        if !s.ambiguous_call(call)? {
            r["subject"] = json!({"callId":call});
        }
    }
    n::set_time(&mut r, "decidedAt", &row["resolved_at_ms"]);
    if let Some(id) = row["resolver_id"].as_str() {
        match row["resolver_kind"].as_str() {
            Some("device") => r["decider"] = json!({"device":id}),
            Some("channel") => r["decider"] = json!({"address":id}),
            _ => {}
        }
    }
    if r.get("decider").is_some() {
        meta(&mut r["decider"], P, "resolverKind", &row["resolver_kind"]);
    }
    let id = row["approval_id"].as_str().unwrap_or("");
    if let Some(reviews) = s.entity(&format!("reviews:{id}"))?.or(s.entity(&format!(
        "reviews:{}",
        row["source_tool_call_id"].as_str().unwrap_or("")
    ))?) {
        r["reviews"] = json!(reviews
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| {
                let by = v["label"].as_str().or(v["id"].as_str())?;
                let mut r = json!({"by":{"address":by}});
                copy(&mut r, "verdict", v, "status");
                copy(&mut r, "rationale", v, "rationale");
                if let Some(risk) = v.get("riskLevel") {
                    meta(&mut r, P, "risk", risk);
                }
                Some(r)
            })
            .collect::<Vec<_>>());
    }
    meta(&mut r, P, "presented", &n::parse(&row["presentation_json"]));
    for (key, field) in [
        ("expiresAt", "expires_at_ms"),
        ("requestedAt", "created_at_ms"),
        ("consumedAt", "consumed_at_ms"),
    ] {
        if let Some(t) = n::time(&row[field]) {
            meta(&mut r, P, key, &json!(t));
        }
    }
    if let Some(consumer) = row["consumed_by"].as_str() {
        meta(&mut r, P, "consumerId", &json!(consumer));
    }
    if let Some(reason) = row.get("terminal_reason").filter(|v| !v.is_null()) {
        meta(&mut r, P, "terminalReason", reason);
    }
    Ok(r)
}
