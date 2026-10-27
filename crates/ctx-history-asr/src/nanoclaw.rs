//! NanoClaw 2.2 session catalog and inbound/outbound storage. These are UI/IO
//! records, not a retained model request or the backing harness transcript.
mod messages;
mod source;
use crate::{
    native as n,
    records::{copy, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};
pub(crate) use source::{ingest, visit};
pub(super) const P: Provider = Provider::NanoClaw;

pub(crate) fn convert(s: &Spool, w: &mut Writer, h: &mut Value, v: &Value) -> Result<()> {
    let row = &v["row"];
    let table = v["table"].as_str().unwrap_or("");
    if matches!(table, "messages_in" | "messages_out") {
        return messages::convert(s, w, h, v);
    }
    let mut r = match table {
        "sessions" => {
            let mut r = json!({"type":"session_info"});
            n::retain(&mut r, P, "session", row);
            r
        }
        "delivered" => {
            let mut r = json!({"type":"delivery","status":match row["status"].as_str(){Some("delivered")=>"sent",Some("failed")=>"failed",_=>"unknown"}});
            if let Some(id) = row["message_out_id"].as_str() {
                if let Some(message) = s.entity(&format!("out-message:{id}"))? {
                    r["message"] = message;
                }
            }
            if let Some(id) = row["platform_message_id"].as_str() {
                r["platformMessageIds"] = json!([id]);
            }
            n::set_time(&mut r, "ts", &row["delivered_at"]);
            r
        }
        "pending_questions" => {
            let id = row["question_id"].as_str().unwrap_or("");
            if s.entity(&format!("question:{id}"))?.is_some() {
                return Ok(());
            }
            let mut r = json!({"type":"input_request","requestId":id,"mode":"questions","status":"pending","questions":[{"id":id,"text":row["title"]}]});
            if let Some(opts) = n::parse(&row["options_json"]).as_array() {
                r["questions"][0]["options"] = json!(messages::options(opts));
            }
            r
        }
        "agent_groups" => {
            let mut r = json!({"type":"config_change","snapshot":true,"agent":row["id"]});
            copy(&mut r, "options", row, "options");
            r
        }
        _ => return n::vendor(s, w, v),
    };
    n::retain(&mut r, P, "row", row);
    n::emit(s, w, r, v)?;
    Ok(())
}
