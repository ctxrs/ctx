use super::*;
pub(super) fn trigger(s: &Spool, w: &Writer, r: &mut Value, d: &Value) -> Result<()> {
    let native = d["source"].as_str();
    let schedule = native
        .and_then(|n| n.strip_prefix("schedule-"))
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()));
    if schedule.is_some() || d["isAutopilotContinuation"] == true {
        r["author"] = json!("harness");
        r["origin"] = json!("injected");
    } else if native.is_some_and(|n| n.starts_with("agent-")) {
        r["author"] = json!("agent");
        r["origin"] = json!("inter_session");
    }
    let Some(turn) = w.turn.as_deref() else {
        return Ok(());
    };
    if s.entity(&format!("copilot:turn:{turn}"))?.is_some() {
        return Ok(());
    }
    if let Some(id) = schedule {
        r["trigger"] = json!({"kind":"schedule","native":native.unwrap(),"id":id});
        if let Some(schedule) = s.entity(&format!("copilot:schedule:{id}"))? {
            r["trigger"]["schedule"] = schedule;
        }
    } else if d["isAutopilotContinuation"] == true {
        r["trigger"] = json!({"kind":"auto_continuation"});
    } else if let Some(source) = native.filter(|n| n.starts_with("agent-")) {
        r["trigger"] = json!({"kind":"peer","native":source});
    }
    Ok(())
}
pub(super) fn convert(s: &Spool, w: &mut Writer, v: &Value, _cwd: Option<&str>) -> Result<()> {
    let d = &v["data"];
    let kind = v["type"].as_str().unwrap_or("unknown");
    let state = match kind {
        "assistant.turn_start" => Some("running"),
        "assistant.turn_end" | "session.idle" => Some("idle"),
        "session.resume" => Some("resumed"),
        "session.shutdown" => Some("ended"),
        "session.error" => Some("error"),
        _ => None,
    };
    if let Some(state) = state {
        let mut r = json!({"type":"state","state":state});
        if kind == "session.error" {
            r["error"] = json!({});
            copy(&mut r["error"], "message", d, "message");
            copy(&mut r["error"], "code", d, "errorType");
        }
        retain(&mut r, w.provider, v, &[]);
        emit(w, r, v)?;
    } else if kind == "session.schedule_created" {
        if let Some(id) = d["id"].as_u64() {
            let mut schedule = json!({});
            for k in ["intervalMs", "at", "cron", "tz", "recurring", "selfPaced"] {
                copy(&mut schedule, k, d, k);
            }
            s.save(&format!("copilot:schedule:{id}"), &schedule)?;
        }
        w.vendor(v, kind)?;
    } else if matches!(
        kind,
        "session.model_change" | "session.mode_changed" | "session.start"
    ) {
        let mut r = json!({"type":"config_change"});
        let model = d["newModel"]
            .as_str()
            .or_else(|| d["selectedModel"].as_str());
        if let Some(model) = model {
            r["model"] = json!({"id":model});
        }
        copy(&mut r, "mode", d, "mode");
        copy(&mut r, "effort", d, "reasoningEffort");
        if r.as_object().unwrap().len() == 1 {
            w.vendor(v, kind)?;
        } else {
            if kind == "session.start" {
                r["snapshot"] = json!(true);
            }
            retain(&mut r, w.provider, v, &[]);
            emit(w, r, v)?;
        }
    } else if matches!(kind, "session.compaction_complete" | "session.truncation") {
        let truncate = kind == "session.truncation";
        let mut r = json!({"type":"compaction","kind":if truncate{"prune"}else{"summary"},"replaces":{"state":"unknown"}});
        entity(
            &mut r,
            "compactionId",
            d["compactionAttemptId"]
                .as_str()
                .or_else(|| v["id"].as_str()),
            format!("compaction:{}", w.id()),
        );
        if let Some(summary) = d.get("summaryContent") {
            r["summary"] = json!(parts(summary, w.provider));
        } else {
            r["capture"]["summary"] = json!({"state":"absent"});
        }
        if let Some(success) = d["success"].as_bool() {
            r["status"] = json!(if success { "completed" } else { "failed" });
        }
        let (before, after) = if truncate {
            (
                "preTruncationTokensInMessages",
                "postTruncationTokensInMessages",
            )
        } else {
            ("preCompactionTokens", "postCompactionTokens")
        };
        if let Some(n) = d.get(before) {
            r["tokens"]["before"] = n.clone();
        }
        if let Some(n) = d.get(after) {
            r["tokens"]["after"] = n.clone();
        }
        retain(&mut r, w.provider, v, &[]);
        emit(w, r, v)?;
    } else {
        w.vendor(v, kind)?;
    }
    Ok(())
}
