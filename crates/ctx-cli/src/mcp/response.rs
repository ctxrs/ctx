use anyhow::Error;
use serde_json::{json, Value};

use super::{compact_json, render_tool_text};

pub(super) fn tool_result(structured: Value) -> Value {
    let text = render_tool_text(&structured);
    json!({
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ],
        "structuredContent": structured,
    })
}

pub(super) fn tool_error_result(err: Error) -> Value {
    if let Some(error) =
        err.downcast_ref::<ctx_history_capture::complete_content::CompleteContentError>()
    {
        let structured = crate::complete_content::complete_content_error_json(error);
        return json!({
            "isError": true,
            "content": [
                {
                    "type": "text",
                    "text": error.to_string(),
                }
            ],
            "structuredContent": structured,
        });
    }
    if let Some(error_code) = crate::pro::stable_error_code(&err) {
        return json!({
            "isError": true,
            "content": [
                {
                    "type": "text",
                    "text": error_code,
                }
            ],
            "structuredContent": {
                "error": error_code,
                "error_code": error_code,
            }
        });
    }
    let error = err.to_string();
    json!({
        "isError": true,
        "content": [
            {
                "type": "text",
                "text": error.clone(),
            }
        ],
        "structuredContent": {
            "error": error,
        }
    })
}

pub(super) fn success_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

pub(super) fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let error = compact_json(json!({
        "code": code,
        "message": message,
        "data": data,
    }));
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error,
    })
}

pub(super) fn invalid_request_response(id: Option<&Value>) -> Value {
    let id = match id {
        Some(id @ (Value::String(_) | Value::Number(_))) => id.clone(),
        None | Some(Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_)) => {
            Value::Null
        }
    };
    error_response(id, -32600, "Invalid Request", None)
}

pub(super) fn json_rpc_error(code: i64, message: &str, data: Option<Value>) -> Value {
    compact_json(json!({
        "code": code,
        "message": message,
        "data": data,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{error_response, invalid_request_response};

    #[test]
    fn error_response_preserves_required_null_id_while_pruning_optional_data() {
        let response = error_response(serde_json::Value::Null, -32700, "Parse error", None);

        assert!(response.as_object().unwrap().contains_key("id"));
        assert!(response["id"].is_null());
        assert_eq!(response["error"]["code"], -32700);
        assert!(!response["error"].as_object().unwrap().contains_key("data"));
    }

    #[test]
    fn error_response_preserves_string_and_numeric_ids_exactly() {
        let string_id = invalid_request_response(Some(&json!("request-7")));
        let numeric_id = invalid_request_response(Some(&json!(7)));

        assert_eq!(string_id["id"], "request-7");
        assert_eq!(numeric_id["id"], 7);
    }

    #[test]
    fn invalid_request_response_uses_null_for_unknown_or_invalid_ids() {
        let unknown = invalid_request_response(None);
        assert!(unknown.as_object().unwrap().contains_key("id"));
        assert!(unknown["id"].is_null());

        for id in [json!(null), json!(true), json!([])] {
            let response = invalid_request_response(Some(&id));
            assert!(response.as_object().unwrap().contains_key("id"));
            assert!(response["id"].is_null());
        }
    }
}
