use super::*;
pub(super) fn index(spool: &Spool, part: &Value) -> Result<()> {
    let data = &part["data"];
    if data["actionType"] == "elicitation" {
        let schema = &data["requested_schema"];
        let secret = schema["properties"].as_object().is_some_and(|o| {
            o.values()
                .any(|v| v["format"] == "password" || v["writeOnly"] == true)
        });
        if secret {
            if let Some(id) = data["id"].as_str() {
                spool.save(&format!("goose:secret:{id}"), &json!(true))?;
            }
        }
    }
    Ok(())
}
pub(crate) fn redact(spool: &Spool, row: &mut Value) -> Result<()> {
    let Some(mut parts) = parsed(row, "content_json") else {
        return Ok(());
    };
    let mut changed = false;
    for part in parts.as_array_mut().into_iter().flatten() {
        let data = &mut part["data"];
        if data["actionType"] == "elicitationResponse" {
            if let Some(id) = data["id"].as_str() {
                if spool.entity(&format!("goose:secret:{id}"))?.is_some() {
                    if let Some(object) = data.as_object_mut() {
                        object.remove("user_data");
                        object.insert("_ctxSecretAnswerRemoved".into(), json!(true));
                        changed = true;
                    }
                }
            }
        }
    }
    if changed {
        row["content_json"] = json!(parts.to_string());
    }
    Ok(())
}
pub(super) fn emit(spool: &Spool, writer: &mut Writer, part: &Value) -> Result<()> {
    let data = if part["type"] == "toolConfirmationRequest" {
        part
    } else {
        &part["data"]
    };
    let Some(id) = data["id"].as_str() else {
        return writer.vendor(part, "interaction");
    };
    let kind = if part["type"] == "toolConfirmationRequest" {
        "toolConfirmation"
    } else {
        data["actionType"].as_str().unwrap_or_default()
    };
    let mut out = match kind {
        "toolConfirmation" => {
            let mut v = json!({"type":"permission","requestId":id,"status":"pending"});
            if !spool.ambiguous_call(id)? {
                v["subject"] = json!({"callId":id});
            }
            copy(&mut v, "title", data, "toolName");
            copy(&mut v, "description", data, "prompt");
            v
        }
        "toolConfirmationResponse" => {
            let mut v = json!({"type":"permission","requestId":id,"status":"resolved"});
            let decision = match data["permission"].as_str() {
                Some("always_allow") => Some("allow_always"),
                Some("allow_once") => Some("allow_once"),
                Some("deny_once") => Some("reject_once"),
                Some("always_deny") => Some("reject_always"),
                Some("cancel") => Some("cancelled"),
                _ => None,
            };
            if let Some(decision) = decision {
                v["decision"] = json!(decision);
            }
            v
        }
        "elicitation" => {
            let mut v =
                json!({"type":"input_request","requestId":id,"mode":"form","status":"pending"});
            copy(&mut v, "message", data, "message");
            if safe(&data["requested_schema"]) {
                copy(&mut v, "requestedSchema", data, "requested_schema");
            }
            v
        }
        "elicitationResponse" => {
            let mut v = json!({"type":"input_request","requestId":id,"mode":"form","status":if data["action"]=="cancel" {"cancelled"}else if data["action"]=="decline" {"declined"}else{"answered"}});
            if data["_ctxSecretAnswerRemoved"] != true && safe(&data["user_data"]) {
                copy(&mut v, "response", data, "user_data");
            }
            v
        }
        _ => return writer.vendor(part, "interaction"),
    };
    if out["type"] == "input_request" && spool.entity(&format!("goose:secret:{id}"))?.is_some() {
        out["secret"] = json!(true);
    }
    part_meta(spool, &mut out, P, part)?;
    // Native request IDs give the join, never the nearest request by position.
    writer.revise(
        spool,
        &format!("goose-interaction:{}:{id}", out["type"]),
        out,
        &Value::Null,
    )?;
    Ok(())
}
