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
    compact_json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
            "data": data,
        }
    }))
}

pub(super) fn json_rpc_error(code: i64, message: &str, data: Option<Value>) -> Value {
    compact_json(json!({
        "code": code,
        "message": message,
        "data": data,
    }))
}
