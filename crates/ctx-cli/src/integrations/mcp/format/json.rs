use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Map, Value};

use super::super::{SERVER_ARGS, SERVER_COMMAND, SERVER_NAME};
use super::ConfigStatus;

#[derive(Debug, Clone, Copy)]
pub(in crate::integrations::mcp) enum JsonRoot {
    McpServers,
    Mcp,
    ContextServers,
}

impl JsonRoot {
    fn key(self) -> &'static str {
        match self {
            Self::McpServers => "mcpServers",
            Self::Mcp => "mcp",
            Self::ContextServers => "context_servers",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::integrations::mcp) enum JsonServerShape {
    Plain,
    StdioType,
    OpenCodeLocal,
    CopilotLocal,
    ClineLocal,
}

pub(super) fn status(
    body: &str,
    root: JsonRoot,
    shape: JsonServerShape,
    path: &Path,
) -> Result<ConfigStatus> {
    let doc = parse(body, path)?;
    let object = doc
        .as_object()
        .ok_or_else(|| anyhow!("JSON config root must be an object"))?;
    let Some(servers) = object.get(root.key()) else {
        return Ok(ConfigStatus::Missing);
    };
    let servers = servers
        .as_object()
        .ok_or_else(|| anyhow!("{} must be an object", root.key()))?;
    let Some(server) = servers.get(SERVER_NAME) else {
        return Ok(ConfigStatus::Missing);
    };
    Ok(if server_is_current(server, shape) {
        ConfigStatus::Current
    } else {
        ConfigStatus::Conflict
    })
}

pub(super) fn upsert(
    body: &str,
    root: JsonRoot,
    shape: JsonServerShape,
    force: bool,
    path: &Path,
) -> Result<String> {
    let mut doc = if body.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        parse(body, path)?
    };
    let object = doc
        .as_object_mut()
        .ok_or_else(|| anyhow!("JSON config root must be an object"))?;
    let root_value = object
        .entry(root.key().to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = root_value
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must be an object", root.key()))?;
    if let Some(existing) = servers.get(SERVER_NAME) {
        if server_is_current(existing, shape) {
            return render(&doc);
        }
        if !force {
            return Err(anyhow!(
                "existing ctx MCP server has different command or args"
            ));
        }
    }
    servers.insert(SERVER_NAME.to_owned(), server_value(shape));
    render(&doc)
}

fn parse(body: &str, path: &Path) -> Result<Value> {
    if path
        .extension()
        .is_some_and(|extension| extension == "jsonc")
    {
        jsonc_parser::parse_to_serde_value::<Value>(body, &Default::default())
            .with_context(|| format!("parse JSONC config {}", path.display()))
    } else {
        serde_json::from_str(body).with_context(|| format!("parse JSON config {}", path.display()))
    }
}

fn render(value: &Value) -> Result<String> {
    let mut body = serde_json::to_string_pretty(value)?;
    body.push('\n');
    Ok(body)
}

fn server_value(shape: JsonServerShape) -> Value {
    match shape {
        JsonServerShape::Plain => json!({
            "command": SERVER_COMMAND,
            "args": SERVER_ARGS,
        }),
        JsonServerShape::StdioType => json!({
            "type": "stdio",
            "command": SERVER_COMMAND,
            "args": SERVER_ARGS,
        }),
        JsonServerShape::OpenCodeLocal => json!({
            "type": "local",
            "command": [SERVER_COMMAND, "mcp", "serve"],
            "enabled": true,
        }),
        JsonServerShape::CopilotLocal => json!({
            "type": "local",
            "command": SERVER_COMMAND,
            "args": SERVER_ARGS,
            "tools": ["*"],
        }),
        JsonServerShape::ClineLocal => json!({
            "command": SERVER_COMMAND,
            "args": SERVER_ARGS,
            "disabled": false,
            "autoApprove": [],
        }),
    }
}

fn server_is_current(value: &Value, shape: JsonServerShape) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    match shape {
        JsonServerShape::Plain => {
            command_string_is_current(object) && args_are_current(object.get("args"))
        }
        JsonServerShape::StdioType => {
            object.get("type").and_then(Value::as_str) == Some("stdio")
                && command_string_is_current(object)
                && args_are_current(object.get("args"))
        }
        JsonServerShape::OpenCodeLocal => {
            object.get("type").and_then(Value::as_str) == Some("local")
                && object.get("enabled").and_then(Value::as_bool) != Some(false)
                && command_array_is_current(object.get("command"))
        }
        JsonServerShape::CopilotLocal => {
            object.get("type").and_then(Value::as_str) == Some("local")
                && command_string_is_current(object)
                && args_are_current(object.get("args"))
        }
        JsonServerShape::ClineLocal => {
            command_string_is_current(object)
                && args_are_current(object.get("args"))
                && object.get("disabled").and_then(Value::as_bool) != Some(true)
        }
    }
}

fn command_string_is_current(object: &Map<String, Value>) -> bool {
    object.get("command").and_then(Value::as_str) == Some(SERVER_COMMAND)
}

fn command_array_is_current(value: Option<&Value>) -> bool {
    string_array_is(value, &[SERVER_COMMAND, "mcp", "serve"])
}

fn args_are_current(value: Option<&Value>) -> bool {
    string_array_is(value, SERVER_ARGS)
}

fn string_array_is(value: Option<&Value>, expected: &[&str]) -> bool {
    let Some(args) = value.and_then(Value::as_array) else {
        return false;
    };
    args.len() == expected.len()
        && args
            .iter()
            .zip(expected.iter().copied())
            .all(|(arg, expected)| arg.as_str() == Some(expected))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::{json, Value};

    use super::*;

    fn json_path() -> &'static Path {
        Path::new("config.json")
    }

    fn jsonc_path() -> &'static Path {
        Path::new("mimocode.jsonc")
    }

    #[test]
    fn writer_adds_ctx_and_is_idempotent() {
        let first = upsert(
            r#"{"other":true}"#,
            JsonRoot::McpServers,
            JsonServerShape::StdioType,
            false,
            json_path(),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(value["other"], true);
        assert_eq!(value["mcpServers"]["ctx"]["command"], "ctx");
        assert_eq!(value["mcpServers"]["ctx"]["args"], json!(["mcp", "serve"]));
        assert_eq!(value["mcpServers"]["ctx"]["type"], "stdio");
        assert_eq!(
            status(
                &first,
                JsonRoot::McpServers,
                JsonServerShape::StdioType,
                json_path()
            )
            .unwrap(),
            ConfigStatus::Current
        );
        let second = upsert(
            &first,
            JsonRoot::McpServers,
            JsonServerShape::StdioType,
            false,
            json_path(),
        )
        .unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn writer_preserves_conflicting_ctx_unless_forced() {
        let original = r#"{"mcpServers":{"ctx":{"command":"old","args":[]}}}"#;
        assert_eq!(
            status(
                original,
                JsonRoot::McpServers,
                JsonServerShape::Plain,
                json_path()
            )
            .unwrap(),
            ConfigStatus::Conflict
        );
        assert!(upsert(
            original,
            JsonRoot::McpServers,
            JsonServerShape::Plain,
            false,
            json_path(),
        )
        .is_err());
        let forced = upsert(
            original,
            JsonRoot::McpServers,
            JsonServerShape::Plain,
            true,
            json_path(),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&forced).unwrap();
        assert_eq!(value["mcpServers"]["ctx"]["command"], "ctx");
    }

    #[test]
    fn writer_reports_invalid_shapes() {
        assert!(upsert(
            "[]",
            JsonRoot::McpServers,
            JsonServerShape::Plain,
            false,
            json_path()
        )
        .is_err());
        assert!(upsert(
            r#"{"mcpServers":[]}"#,
            JsonRoot::McpServers,
            JsonServerShape::Plain,
            false,
            json_path(),
        )
        .is_err());
    }

    #[test]
    fn opencode_writer_uses_command_array_shape() {
        let body = upsert(
            "",
            JsonRoot::Mcp,
            JsonServerShape::OpenCodeLocal,
            false,
            json_path(),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            value["mcp"]["ctx"]["command"],
            json!(["ctx", "mcp", "serve"])
        );
        assert_eq!(value["mcp"]["ctx"]["type"], "local");
        assert_eq!(
            status(
                &body,
                JsonRoot::Mcp,
                JsonServerShape::OpenCodeLocal,
                json_path()
            )
            .unwrap(),
            ConfigStatus::Current
        );
    }

    #[test]
    fn opencode_shape_rejects_stdio_style_server() {
        let body = r#"{"mcp":{"ctx":{"type":"stdio","command":"ctx","args":["mcp","serve"]}}}"#;
        assert_eq!(
            status(
                body,
                JsonRoot::Mcp,
                JsonServerShape::OpenCodeLocal,
                json_path()
            )
            .unwrap(),
            ConfigStatus::Conflict
        );
    }

    #[test]
    fn current_detection_rejects_non_string_array_items() {
        let opencode = r#"{"mcp":{"ctx":{"type":"local","command":["ctx","mcp","serve",1]}}}"#;
        assert_eq!(
            status(
                opencode,
                JsonRoot::Mcp,
                JsonServerShape::OpenCodeLocal,
                json_path()
            )
            .unwrap(),
            ConfigStatus::Conflict
        );

        let plain = r#"{"mcpServers":{"ctx":{"command":"ctx","args":["mcp","serve",1]}}}"#;
        assert_eq!(
            status(
                plain,
                JsonRoot::McpServers,
                JsonServerShape::Plain,
                json_path()
            )
            .unwrap(),
            ConfigStatus::Conflict
        );
    }

    #[test]
    fn jsonc_configs_with_comments_are_parsed_and_updated() {
        let body = r#"{
          // keep parsing existing MiMo JSONC
          "mcp": {
            "other": {
              "type": "local",
              "command": ["other"],
            },
          },
        }"#;
        assert_eq!(
            status(
                body,
                JsonRoot::Mcp,
                JsonServerShape::OpenCodeLocal,
                jsonc_path()
            )
            .unwrap(),
            ConfigStatus::Missing
        );
        let updated = upsert(
            body,
            JsonRoot::Mcp,
            JsonServerShape::OpenCodeLocal,
            false,
            jsonc_path(),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(
            value["mcp"]["ctx"]["command"],
            json!(["ctx", "mcp", "serve"])
        );
    }
}
