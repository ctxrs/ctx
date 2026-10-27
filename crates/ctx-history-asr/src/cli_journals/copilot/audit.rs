use super::*;
pub(super) fn convert(s: &Spool, w: &mut Writer, v: &Value) -> Result<bool> {
    let d = &v["data"];
    let kind = v["type"].as_str().unwrap_or("");
    match kind {
        "system.message" | "instruction.update" => {
            let mut r = json!({"type":"instructions","kind":"system_prompt","content":parts(&d["content"],w.provider)});
            copy(&mut r, "name", d, "name");
            retain(&mut r, w.provider, v, &[]);
            emit(w, r, v)?;
        }
        "hook.start" | "hook.end" => {
            let event = match d["hookType"].as_str().unwrap_or("unknown") {
                "preToolUse" => "pre_tool_use".to_owned(),
                "postToolUse" => "post_tool_use".to_owned(),
                "userPromptSubmitted" => "user_prompt_submit".to_owned(),
                "sessionStart" => "session_start".to_owned(),
                "sessionEnd" => "session_end".to_owned(),
                "stop" => "stop".to_owned(),
                x => format!("_com.github.copilot-cli/{x}"),
            };
            let mut r = json!({"type":"hook","event":event});
            copy(&mut r, "hookId", d, "hookInvocationId");
            if let Some(success) = d["success"].as_bool() {
                r["status"] = json!(if success { "completed" } else { "failed" });
            }
            if let Some(output) = d.get("output") {
                r["output"] = json!(parts(output, w.provider));
            }
            retain(&mut r, w.provider, v, &[]);
            if let Some(id) = d["hookInvocationId"].as_str() {
                w.revise(s, &format!("copilot:hook:{id}"), r, v)?;
            } else {
                emit(w, r, v)?;
            }
        }
        "permission.requested" | "permission.completed" => {
            let Some(id) = d["requestId"].as_str() else {
                return Ok(false);
            };
            let key = format!("copilot:permission:{id}");
            let mut r = s
                .entity(&key)?
                .unwrap_or_else(|| json!({"type":"permission","requestId":id,"idSource":"native"}));
            r.as_object_mut().unwrap().remove("_record");
            r.as_object_mut().unwrap().remove("supersedes");
            if let Some(call) = d["toolCallId"]
                .as_str()
                .or_else(|| d["permissionRequest"]["toolCallId"].as_str())
            {
                if !s.ambiguous_call(call)? {
                    r["subject"] = json!({"callId":call});
                }
            }
            if kind == "permission.requested" {
                if d["resolvedByHook"] != true {
                    r["status"] = json!("pending");
                }
            } else {
                let decision = match d["result"]["kind"].as_str().unwrap_or("unknown") {
                    "approved" => "allow_once".into(),
                    "approved-for-session" | "approved-read-only-for-session" => {
                        "allow_session".into()
                    }
                    "denied-interactively-by-user" | "denied-by-rules" => "reject_once".into(),
                    x => format!("_com.github.copilot-cli/{x}"),
                };
                r["decision"] = json!(decision);
                r["status"] = json!("resolved");
                let by = match d["decisionSource"].as_str() {
                    Some("human_response") => Some("user"),
                    Some("hook") => Some("hook"),
                    Some("host_policy") => Some("policy"),
                    _ => None,
                };
                if let Some(by) = by {
                    r["decidedBy"] = json!(by);
                }
            }
            retain(&mut r, w.provider, v, &[]);
            w.revise(s, &key, r, v)?;
        }
        "user_input.requested" | "user_input.completed" => {
            let Some(id) = d["requestId"].as_str() else {
                return Ok(false);
            };
            let key = format!("copilot:input:{id}");
            let mut r=s.entity(&key)?.unwrap_or_else(||json!({"type":"input_request","requestId":id,"idSource":"native","mode":"questions","capture":{"questions":{"state":"absent"}}}));
            r.as_object_mut().unwrap().remove("_record");
            r.as_object_mut().unwrap().remove("supersedes");
            if let Some(q) = d["question"].as_str() {
                r["questions"] = json!([{"id":"0","text":q}]);
                r.as_object_mut().unwrap().remove("capture");
                if let Some(choices) = d["choices"].as_array() {
                    r["questions"][0]["options"] = json!(choices
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|c| json!({"label":c}))
                        .collect::<Vec<_>>());
                }
                copy(&mut r["questions"][0], "allowOther", d, "allowFreeform");
                copy(&mut r["questions"][0], "secret", d, "secret");
            }
            copy(&mut r, "callId", d, "toolCallId");
            if kind == "user_input.requested" {
                r["status"] = json!("pending");
            } else if let Some(answer) = d["answer"].as_str() {
                r["status"] = json!("answered");
                if r["questions"][0]["secret"] != true {
                    r["answers"] = json!([if d["wasFreeform"] == false {
                        json!({"questionId":"0","selected":[answer]})
                    } else {
                        json!({"questionId":"0","text":answer})
                    }]);
                }
            }
            retain(&mut r, w.provider, v, &[]);
            w.revise(s, &key, r, v)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}
