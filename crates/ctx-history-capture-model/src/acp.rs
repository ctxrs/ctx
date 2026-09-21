//! Readers for Agent Client Protocol payloads.
//!
//! Several harnesses persist ACP values verbatim: some wrap them in a
//! JSON-RPC envelope (`params.update`), some store a bare `ToolCall` or
//! `ToolCallUpdate`, and some nest the update one level down. The readers here
//! accept all three shapes and expose only the parts of the protocol whose
//! schema has been audited.
//!
//! Only explicitly recognized content block types are read. An unrecognized
//! type yields no text rather than a best-effort stringification, so a future
//! ACP block type cannot silently become searchable history before anyone has
//! audited its shape.

use serde_json::Value;

/// Resolves the ACP update out of whichever envelope carries it.
///
/// Falls through to the value itself, so a provider that stores a bare
/// `ToolCall` or `ToolCallUpdate` reads identically to one that stores the
/// JSON-RPC notification around it.
pub fn acp_update(value: &Value) -> &Value {
    value
        .pointer("/params/update")
        .or_else(|| value.get("update"))
        .unwrap_or(value)
}

/// The `sessionUpdate` discriminant, when the payload is an update envelope.
pub fn acp_update_kind(value: &Value) -> Option<&str> {
    acp_update(value)
        .get("sessionUpdate")
        .and_then(Value::as_str)
}

/// The status of a tool call that has finished, or `None` while it is still
/// running or in a state this module does not treat as terminal.
pub fn acp_terminal_status(value: &Value) -> Option<&str> {
    acp_update(value)
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| matches!(*status, "completed" | "failed"))
}

/// User-visible text from an ACP content value.
///
/// Accepts a bare string, an array of content blocks joined by newlines, and
/// the `text`, `content`, and `diff` block variants. Any other block type —
/// image, audio, embedded resource, or a variant added after this was written —
/// yields `None`, and an array yields `None` if any element does, so a partial
/// read never masquerades as complete content.
pub fn acp_visible_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .map(acp_visible_text)
                .collect::<Option<Vec<_>>>()?;
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("text") => object
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned),
            Some("content") => object.get("content").and_then(acp_visible_text),
            Some("diff") => serde_json::to_string(value).ok(),
            Some(_) | None => None,
        },
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn update_resolves_every_envelope_including_a_bare_value() {
        let bare = json!({"sessionUpdate": "tool_call", "status": "completed"});
        assert_eq!(acp_update(&bare), &bare);
        assert_eq!(acp_update_kind(&bare), Some("tool_call"));

        let nested = json!({"update": bare.clone()});
        assert_eq!(acp_update(&nested), &bare);

        let rpc = json!({"params": {"update": bare.clone()}});
        assert_eq!(acp_update(&rpc), &bare);
        assert_eq!(acp_update_kind(&rpc), Some("tool_call"));
    }

    #[test]
    fn terminal_status_admits_only_finished_calls() {
        for status in ["completed", "failed"] {
            let value = json!({"status": status});
            assert_eq!(acp_terminal_status(&value), Some(status));
        }
        for status in ["pending", "in_progress", "cancelled"] {
            assert_eq!(acp_terminal_status(&json!({"status": status})), None);
        }
        assert_eq!(acp_terminal_status(&json!({})), None);
    }

    #[test]
    fn visible_text_reads_the_audited_union_and_refuses_the_rest() {
        assert_eq!(acp_visible_text(&json!("plain")).as_deref(), Some("plain"));
        assert_eq!(
            acp_visible_text(&json!({"type": "text", "text": "hello"})).as_deref(),
            Some("hello")
        );
        assert_eq!(
            acp_visible_text(&json!({
                "type": "content",
                "content": {"type": "text", "text": "nested"}
            }))
            .as_deref(),
            Some("nested")
        );
        assert_eq!(
            acp_visible_text(&json!([
                {"type": "text", "text": "one"},
                {"type": "text", "text": "two"}
            ]))
            .as_deref(),
            Some("one\ntwo")
        );

        // A diff block is retained as its serialized form.
        assert!(acp_visible_text(&json!({"type": "diff", "path": "a"}))
            .is_some_and(|text| text.contains("\"diff\"")));

        // Unaudited and future variants stay contentless, and one unreadable
        // element makes the whole array unreadable.
        assert_eq!(acp_visible_text(&json!({"type": "image"})), None);
        assert_eq!(acp_visible_text(&json!({"type": "resource_link"})), None);
        assert_eq!(acp_visible_text(&json!({})), None);
        assert_eq!(
            acp_visible_text(&json!([{"type": "text", "text": "one"}, {"type": "image"}])),
            None
        );
        assert_eq!(acp_visible_text(&json!([])), None);
        assert_eq!(acp_visible_text(&Value::Null), None);
        assert_eq!(acp_visible_text(&json!(7)), None);
    }
}
