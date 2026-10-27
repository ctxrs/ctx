use super::P;
use crate::{
    native as n,
    records::{copy, session_ref, Writer},
    spool::Spool,
    Result,
};
use serde_json::{json, Value};
pub(super) fn channel(c: &str) -> bool {
    [
        "whatsapp", "telegram", "slack", "discord", "imessage", "signal", "email", "sms", "web",
    ]
    .contains(&c)
}
pub(super) fn options(opts: &[Value]) -> Vec<Value> {
    opts.iter()
        .filter_map(|v| {
            if v.is_string() {
                Some(json!({"label":v}))
            } else {
                let label = v["label"].as_str()?;
                let mut o = json!({"label":label});
                copy(&mut o, "value", v, "value");
                copy(&mut o, "description", v, "description");
                Some(o)
            }
        })
        .collect()
}
pub(super) fn convert(s: &Spool, w: &mut Writer, h: &Value, v: &Value) -> Result<()> {
    let row = &v["row"];
    let input = v["table"] == "messages_in";
    let data = n::parse(&row["content"]);
    let id = row["id"].as_str().unwrap_or("");
    let secret = data["questionId"]
        .as_str()
        .map(|q| s.entity(&format!("secret-question:{q}")))
        .transpose()?
        .flatten()
        .is_some();
    if row["kind"] == "system" || (!input && data["operation"].is_string()) {
        // A request to edit/react/reset is not evidence the operation succeeded,
        // and this IO record does not identify the backing tool call.
        if secret && input {
            let mut safe = v.clone();
            safe["row"].as_object_mut().map(|r| r.remove("content"));
            return n::vendor(s, w, &safe);
        }
        return n::vendor(s, w, v);
    }
    let mut r = json!({"type":"message","messageId":id,"role":if input{"user"}else{"assistant"},"content":[],"capture":{"content":{"state":"absent"}},"displayCorrespondence":"native"});
    if secret && input {
        r["capture"]["displayContent"] = json!({"state":"redacted","reason":"policy"});
    } else {
        let text = data["text"]
            .as_str()
            .or(data["markdown"].as_str())
            .or(data["fallbackText"].as_str());
        r["displayContent"] = if let Some(text) = text {
            json!([{"type":"text","text":text}])
        } else if !data.is_null() {
            json!([{"type":"data","data":data,"schema":"dev.nanoclaw/message-content"}])
        } else {
            json!([{"type":"text","text":row["content"]}])
        };
        n::retain(&mut r, P, "content", &data);
    }
    n::set_time(&mut r, "ts", &row["timestamp"]);
    if input {
        if let Some(source) = row["source_session_id"].as_str() {
            r["from"] = json!({"session":session_ref(P,source)});
            r["author"] = json!("agent");
            r["origin"] = json!("inter_session");
        } else if matches!(row["kind"].as_str(), Some("task" | "recurring_task")) {
            r["author"] = json!("harness");
            r["origin"] = json!("injected");
        } else {
            r["origin"] = json!("queued");
            if let Some(sender) = data["senderId"]
                .as_str()
                .or(data["author"]["userId"].as_str())
            {
                let pid = n::stable_id(&json!([row["channel_type"], sender]).to_string());
                let mut p = json!({"id":pid,"ids":[{"scheme":row["channel_type"].as_str().unwrap_or("nanoclaw"),"value":sender}]});
                if let Some(name) = data["sender"]
                    .as_str()
                    .or(data["author"]["fullName"].as_str())
                    .or(data["author"]["userName"].as_str())
                {
                    p["displayName"] = json!(name);
                }
                if data["author"]["isBot"] == true {
                    p["kind"] = json!("bot");
                }
                s.save(&format!("participant:{pid}"), &p)?;
                r["from"] = json!({"participant":pid});
            }
        }
        // Wake eligibility is not a persisted turn boundary or proof of delivery.
        if data["isMention"].is_boolean() {
            r["addressed"] = data["isMention"].clone();
        }
    } else {
        r["audience"] = json!("user");
    }
    if let Some(mid) = data["id"].as_str().filter(|_| row["kind"] == "chat-sdk") {
        r["platform"] = json!({"messageIds":[mid]});
    }
    if let Some(channel) = row["channel_type"].as_str().filter(|c| channel(c)) {
        if let Some(cs) = h["conversations"].as_array() {
            if let Some(c) = cs
                .iter()
                .find(|c| c["channel"] == channel && c["nativeId"] == row["platform_id"])
            {
                r["conversation"] = c["id"].clone();
            }
        }
    }
    let mut raw = row.clone();
    if secret && input {
        if let Some(o) = raw.as_object_mut() {
            o.remove("content");
        }
    }
    n::retain(&mut r, P, "row", &raw);
    // Each store is a distinct order domain; don't manufacture a context edge
    // from the last inbound row to the first outbound row.
    if !input && s.entity("outbound-started")?.is_none() {
        r["branchesFrom"] = json!({"state":"unknown"});
        s.save("outbound-started", &json!(true))?;
    }
    let record = n::emit(s, w, r, v)?;
    s.save(
        &format!("{}-message:{id}", if input { "in" } else { "out" }),
        &json!(record),
    )?;
    if !input && data["type"] == "ask_question" {
        question(s, w, v, &data, secret)?;
    }
    Ok(())
}
fn question(s: &Spool, w: &mut Writer, v: &Value, data: &Value, secret: bool) -> Result<()> {
    let Some(id) = data["questionId"].as_str() else {
        return Ok(());
    };
    let Some(text) = data["question"].as_str().or(data["title"].as_str()) else {
        return Ok(());
    };
    let mut q = json!({"id":id,"text":text});
    copy(&mut q, "header", data, "title");
    if let Some(opts) = data["options"].as_array() {
        q["options"] = json!(options(opts));
    }
    if secret {
        q["secret"] = json!(true);
    }
    let mut r = json!({"type":"input_request","requestId":id,"mode":"questions","questions":[q]});
    if let Some(answer) = s.entity(&format!("answer:{id}"))? {
        if answer["ambiguous"] == true {
            n::emit(s, w, r, v)?;
            return Ok(());
        }
        r["status"] = json!("answered");
        if secret {
            r["capture"] = json!({"answers":{"state":"redacted","reason":"policy"}});
        } else {
            if let Some(record) = s.entity(&format!(
                "in-message:{}",
                answer["id"].as_str().unwrap_or("")
            ))? {
                r["answerRef"] = record;
            }
            let mut a = json!({"questionId":id});
            if let Some(v) = answer["content"].get("selectedOption") {
                a["selected"] = json!([v]);
            }
            copy(&mut a, "text", &answer["content"], "text");
            r["answers"] = json!([a]);
        }
    }
    n::emit(s, w, r, v)?;
    Ok(())
}
