//! Current-state sidecars are serialized only after every journal position.
use super::common::*;
use crate::{
    records::{copy, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(crate) fn tail(spool: &mut Spool, w: &mut Writer, header: &Value) -> Result<()> {
    let mut m = header["metadata"].clone();
    scrub_settings(&mut m);
    w.position(w.pos + 1);
    w.identity = Some("sidecar-metadata".into());
    w.snapshot = true;
    if m.as_object().is_some_and(|o| !o.is_empty()) && w.provider != Provider::Tabnine {
        if let Some(reason) = m["_asr_gap"].as_str() {
            emit(w, json!({"type":"gap","reason":reason}), &json!({}))?;
        } else {
            let mut info = json!({"type":"session_info"});
            match w.provider {
                Provider::Junie => copy(&mut info, "title", &m, "taskName"),
                Provider::MistralVibe => {
                    copy(&mut info, "title", &m, "title");
                    if m["title_source"] == "manual" {
                        info["titleSource"] = json!("user");
                    }
                    // auto includes fallback prompt slices, not necessarily model titles.
                }
                Provider::Mux => copy(&mut info, "title", &m, "name"),
                _ => {}
            }
            retain(&mut info, w, "metadata", &m);
            emit(w, info, &json!({}))?;
        }
    }
    if w.provider == Provider::MistralVibe {
        w.slot(10_000);
        let stats = &m["stats"];
        let u = usage(
            stats,
            [
                "session_prompt_tokens",
                "session_completion_tokens",
                "session_cached_tokens",
                "cache_write_tokens",
                "session_total_llm_tokens",
            ],
        );
        if u.as_object().is_some_and(|o| !o.is_empty()) {
            let mut r = json!({"type":"usage_summary","scope":"session","cumulative":true,"includesChildren":null,"usage":u});
            if let Some(n) = count(&stats["context_tokens"]) {
                r["contextWindow"] = json!({"used":n});
            }
            if let Some(cost) = stats["session_cost"].as_f64().filter(|c| *c >= 0.0) {
                r["cost"] = json!({"amount":cost,"currency":"USD","source":"harness_estimated"});
            }
            retain(&mut r, w, "stats", stats);
            emit(w, r, &json!({}))?;
        }
        w.slot(20_000);
        if m["config"].is_object() || m["agent_profile"].is_object() {
            let mut r = json!({"type":"config_change","snapshot":true,"options":{}});
            if let Some(model) = m["config"]["active_model"].as_str() {
                r["model"] = json!({"id":model});
            }
            if let Some(name) = m["agent_profile"]["name"].as_str() {
                r["agent"] = json!(name);
            }
            retain(&mut r, w, "config", &m["config"]);
            retain(&mut r, w, "agentProfile", &m["agent_profile"]);
            emit(w, r, &json!({}))?;
        }
        w.slot(30_000);
        if let Some(prompt) = m["system_prompt"].get("content") {
            emit(
                w,
                json!({"type":"instructions","kind":"system_prompt","content":parts(prompt)}),
                &json!({}),
            )?;
        }
        w.slot(40_000);
        if let Some(tools) = m["tools_available"].as_array() {
            let mut r = json!({"type":"instructions","kind":"tool_definitions","content":[]});
            retain(&mut r, w, "toolsAvailable", &json!(tools));
            emit(w, r, &json!({}))?;
        }
    }
    if w.provider == Provider::Mux {
        if let Some(index) = header.get("archiveIndex") {
            w.position(w.pos + 1);
            w.snapshot = true;
            w.identity = Some("sidecar-archive-index".into());
            vendor(w, index, "subagent_transcripts")?;
        }

        if let Some(mut p) = spool.entity("journal:partial")? {
            crate::privacy::sanitize(spool, &mut p)?;
            w.position(w.pos + 1);
            w.snapshot = true;
            if let Some(id) = p["id"].as_str() {
                if super::mux::unique_message_id(spool, id)? {
                    w.identity = Some(format!("j{:x}", Sha256::digest(id.as_bytes())));
                } else {
                    w.capture_scope = true;
                }
            } else {
                w.capture_scope = true;
            }
            if let Some(reason) = p["_asr_gap"].as_str() {
                emit(w, json!({"type":"gap","reason":reason}), &json!({}))?;
            } else {
                if spool.entity("journal:partial-effective")? == Some(json!(true)) {
                    super::mux::convert(spool, w, &p, true)?;
                } else {
                    w.identity = Some("sidecar-unselected-partial".into());
                    w.capture_scope = false;
                    vendor(w, &p, "unselected_partial")?;
                }
            }
        }
    }
    w.snapshot = false;
    Ok(())
}
/// Config credentials and MCP connection values are never archival facts.
/// Keep their absence explicit without examining or copying external config.
fn scrub_settings(v: &mut Value) {
    match v {
        Value::Object(o) => {
            for (key, value) in o.iter_mut() {
                if matches!(
                    key.to_ascii_lowercase().as_str(),
                    "api_key"
                        | "apikey"
                        | "access_token"
                        | "refresh_token"
                        | "password"
                        | "secret"
                        | "authorization"
                        | "headers"
                        | "extra_headers"
                        | "env"
                        | "mcp_servers"
                ) {
                    *value = json!("[omitted by ASR secret policy]");
                } else {
                    scrub_settings(value);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(scrub_settings),
        _ => {}
    }
}
