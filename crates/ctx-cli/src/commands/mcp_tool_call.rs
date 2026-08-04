use ctx_history_core::CoreRecord;
use serde_json::{json, Value};

use crate::ui::sanitize_untrusted_history_body_for_terminal;

/// Human-oriented views stay compact even though Core preserves up to 64 KiB
/// for each exact component. Machine formats always retain the full strings.
pub(crate) const MCP_TOOL_CALL_DISPLAY_MAX_CHARS: usize = 256;
pub(crate) const MCP_TOOL_CALL_JSON_GUIDANCE: &str =
    "MCP identity display truncated; use --format json or --format jsonl for exact values.";
pub(crate) const MCP_TOOL_CALL_STRUCTURED_GUIDANCE: &str =
    "MCP identity display truncated; inspect structuredContent for exact JSON values.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpToolCallDisplay {
    pub(crate) server: String,
    pub(crate) tool: String,
    pub(crate) truncated: bool,
}

/// Adds the provider-neutral machine field only when Core has exact evidence.
/// Presentation content projections intentionally do not affect this metadata.
pub(crate) fn insert_mcp_tool_call(event: &mut Value, record: &CoreRecord) {
    let Some(attribution) = record.mcp_tool_call.as_ref() else {
        return;
    };
    let Some(object) = event.as_object_mut() else {
        return;
    };
    object.insert(
        "mcp_tool_call".to_owned(),
        json!({
            "server": attribution.server,
            "tool": attribution.tool,
        }),
    );
}

pub(crate) fn mcp_tool_call_display(event: &Value) -> Option<McpToolCallDisplay> {
    let attribution = event.get("mcp_tool_call")?;
    let (server, server_truncated) = display_component(attribution.get("server")?.as_str()?);
    let (tool, tool_truncated) = display_component(attribution.get("tool")?.as_str()?);
    Some(McpToolCallDisplay {
        server,
        tool,
        truncated: server_truncated || tool_truncated,
    })
}

pub(crate) fn escape_markdown_structure(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '<'
                | '>'
                | '('
                | ')'
                | '#'
                | '+'
                | '-'
                | '.'
                | '!'
                | '|'
                | '='
                | '~'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

pub(crate) fn append_mcp_tool_call_text(
    output: &mut String,
    event: &Value,
    indent: &str,
    guidance: &str,
) -> bool {
    let Some(attribution) = mcp_tool_call_display(event) else {
        return false;
    };
    output.push_str(&format!("{indent}mcp_server: {}\n", attribution.server));
    output.push_str(&format!("{indent}mcp_tool: {}\n", attribution.tool));
    if attribution.truncated {
        output.push_str(&format!("{indent}mcp_display_truncated: true\n"));
        output.push_str(&format!("{indent}mcp_display_guidance: {guidance}\n"));
    }
    true
}

pub(crate) fn append_mcp_tool_call_markdown(output: &mut String, event: &Value) -> bool {
    let Some(attribution) = mcp_tool_call_display(event) else {
        return false;
    };
    output.push_str(&format!(
        "- MCP server: {}\n",
        escape_markdown_structure(&attribution.server)
    ));
    output.push_str(&format!(
        "- MCP tool: {}\n",
        escape_markdown_structure(&attribution.tool)
    ));
    if attribution.truncated {
        output.push_str(&format!("\n> {MCP_TOOL_CALL_JSON_GUIDANCE}\n"));
    }
    true
}

fn display_component(value: &str) -> (String, bool) {
    let mut characters = value.chars();
    let retained = characters
        .by_ref()
        .take(MCP_TOOL_CALL_DISPLAY_MAX_CHARS)
        .collect::<String>();
    let truncated = characters.next().is_some();
    let mut escaped = reversible_display_escape(&retained);
    if truncated {
        escaped.push_str("… [display truncated]");
    }
    (escaped, truncated)
}

fn reversible_display_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character == '\\' {
            escaped.push_str("\\\\");
            continue;
        }
        escaped.push_str(&sanitize_untrusted_history_body_for_terminal(
            &character.to_string(),
        ));
    }
    escaped
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn display_is_reversible_safe_bounded_and_marks_truncation() {
        let server = format!(
            "literal\\n\n# heading\u{202e}\u{1b}[2J{}",
            "x".repeat(MCP_TOOL_CALL_DISPLAY_MAX_CHARS)
        );
        let event = json!({
            "mcp_tool_call": {
                "server": server,
                "tool": "tool|`[]",
            }
        });

        let display = mcp_tool_call_display(&event).unwrap();
        assert!(display.truncated);
        assert!(display.server.contains("literal\\\\n\\n"));
        assert!(display.server.contains("\\u{202e}"));
        assert!(display.server.contains("\\x1b[2J"));
        assert!(!display.server.contains('\u{202e}'));
        assert!(!display.server.contains('\u{1b}'));
        assert!(display.server.ends_with("… [display truncated]"));

        let markdown = escape_markdown_structure(&display.tool);
        assert_eq!(markdown, "tool\\|\\`\\[\\]");
    }
}
