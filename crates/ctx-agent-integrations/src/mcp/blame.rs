use super::{invalid_tool_request, object_schema};
use crate::tool_backend::{ToolBackendError, ToolOperation};
use ctx_attribution_model::BlameTarget;
use serde_json::{json, Value};

const MCP_BLAME_LIMIT: u32 = 8;
const MCP_BLAME_CURSOR_BYTES: usize = 256;

pub(super) fn parse(arguments: &Value) -> Result<ToolOperation, ToolBackendError> {
    let target: BlameTarget =
        serde_json::from_value(arguments.get("target").cloned().unwrap_or(Value::Null))
            .map_err(|_| invalid_tool_request("invalid blame target"))?;
    target
        .validate()
        .map_err(|_| invalid_tool_request("invalid blame target"))?;
    let limit = match arguments.get("limit") {
        None | Some(Value::Null) => MCP_BLAME_LIMIT,
        Some(value) => value
            .as_u64()
            .and_then(|limit| u32::try_from(limit).ok())
            .ok_or_else(|| invalid_tool_request("limit must be an integer"))?,
    };
    if !(1..=MCP_BLAME_LIMIT).contains(&limit) {
        return Err(invalid_tool_request("limit is outside the fixed bound"));
    }
    let cursor = match arguments.get("cursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(cursor))
            if !cursor.is_empty()
                && cursor.len() <= MCP_BLAME_CURSOR_BYTES
                && cursor.is_ascii() =>
        {
            Some(cursor.clone())
        }
        Some(_) => return Err(invalid_tool_request("cursor is invalid")),
    };
    Ok(ToolOperation::Blame {
        target,
        limit,
        cursor,
    })
}

pub(super) fn tool_definition() -> Value {
    let repository = json!({
        "type": "string",
        "minLength": 1,
        "description": "Logical repository identity; required with a PR number and optional for other selectors."
    });
    let target = json!({
        "oneOf": [
            object_schema(
                json!({
                    "kind": { "type": "string", "const": "file" },
                    "path": { "type": "string", "minLength": 1, "description": "Repository-relative committed file path." },
                    "repository": repository.clone(),
                    "lines": object_schema(
                        json!({
                            "start": { "type": "integer", "minimum": 1 },
                            "end": { "type": "integer", "minimum": 1 }
                        }),
                        vec!["start", "end"]
                    )
                }),
                vec!["kind", "path"]
            ),
            object_schema(
                json!({
                    "kind": { "type": "string", "const": "commit" },
                    "oid": { "type": "string", "minLength": 1, "description": "Full or unambiguous abbreviated Git commit ID." },
                    "repository": repository.clone()
                }),
                vec!["kind", "oid"]
            ),
            object_schema(
                json!({
                    "kind": { "type": "string", "const": "pull_request" },
                    "selector": { "type": "string", "minLength": 1, "description": "Positive PR number or canonical GitHub, GitLab, or Codeberg PR/MR URL." },
                    "repository": repository
                }),
                vec!["kind", "selector"]
            )
        ]
    });
    json!({
        "name": "blame",
        "title": "Agent Blame",
        "description": "Return complete cited provenance for committed file lines, a commit, or a pull request. PR activity and commit production remain separate. Blame reads the local attribution index and never writes provider history or repositories. Run ctx import --all when attribution indexing is pending.",
        "inputSchema": object_schema(
            json!({
                "target": target,
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MCP_BLAME_LIMIT,
                    "default": MCP_BLAME_LIMIT,
                    "description": "Maximum complete matches to return."
                },
                "cursor": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MCP_BLAME_CURSOR_BYTES,
                    "description": "Opaque continuation cursor from a previous blame page."
                }
            }),
            vec!["target"]
        ),
        "annotations": {
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_targets_and_released_transport_bounds_are_preserved() {
        for target in [
            json!({"kind":"file","path":"src/lib.rs","lines":{"start":2,"end":4}}),
            json!({"kind":"commit","oid":"abc123"}),
            json!({"kind":"pull_request","selector":"12","repository":"example/repo"}),
        ] {
            let operation =
                parse(&json!({"target":target,"limit":8,"cursor":"continued"})).unwrap();
            assert!(matches!(
                operation,
                ToolOperation::Blame {
                    limit: 8,
                    cursor: Some(_),
                    ..
                }
            ));
        }
        for args in [
            json!({"target":{"kind":"file","path":"a.rs","extra":true}}),
            json!({"target":{"kind":"file","path":"a.rs","lines":{"start":0,"end":1}}}),
            json!({"target":{"kind":"pull_request","selector":"12"}}),
            json!({"target":{"kind":"commit","oid":"abc123"},"limit":9}),
            json!({"target":{"kind":"commit","oid":"abc123"},"cursor":"x".repeat(257)}),
        ] {
            assert!(parse(&args).is_err(), "{args}");
        }
    }
}
