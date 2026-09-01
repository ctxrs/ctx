use std::{fmt, path::Path};

use anyhow::{anyhow, Context, Result};
use serde::{
    de::{Error as _, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::{json, Map, Value};

use super::super::SERVER_NAME;
use super::{server_command, ConfigStatus, ServerCommand};

#[derive(Debug, Clone, Copy)]
pub enum JsonRoot {
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
pub enum JsonServerShape {
    Plain,
    StdioType,
    OpenCodeLocal,
    CopilotLocal,
    ClineLocal,
}

pub fn status(
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

pub fn upsert(
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

pub fn remove(
    body: &str,
    root: JsonRoot,
    shape: JsonServerShape,
    force: bool,
    path: &Path,
) -> Result<String> {
    let mut doc = parse(body, path)?;
    let object = doc
        .as_object_mut()
        .ok_or_else(|| anyhow!("JSON config root must be an object"))?;
    let Some(root_value) = object.get_mut(root.key()) else {
        return Ok(body.to_owned());
    };
    let servers = root_value
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must be an object", root.key()))?;
    let Some(existing) = servers.get(SERVER_NAME) else {
        return Ok(body.to_owned());
    };
    if !server_is_current(existing, shape) && !force {
        return Err(anyhow!(
            "existing ctx MCP server has different command or args"
        ));
    }
    servers.remove(SERVER_NAME);
    render(&doc)
}

fn parse(body: &str, path: &Path) -> Result<Value> {
    if path
        .extension()
        .is_some_and(|extension| extension == "jsonc")
    {
        jsonc_parser::parse_to_serde_value::<StrictJsonValue>(body, &Default::default())
            .map(StrictJsonValue::into_inner)
            .with_context(|| format!("parse JSONC config {}", path.display()))
    } else {
        serde_json::from_str::<StrictJsonValue>(body)
            .map(StrictJsonValue::into_inner)
            .with_context(|| format!("parse JSON config {}", path.display()))
    }
}

struct StrictJsonValue(Value);

impl StrictJsonValue {
    fn into_inner(self) -> Value {
        self.0
    }
}

impl<'de> Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonValueVisitor)
    }
}

struct StrictJsonValueVisitor;

impl<'de> Visitor<'de> for StrictJsonValueVisitor {
    type Value = StrictJsonValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let number = serde_json::Number::from_f64(value)
            .ok_or_else(|| E::custom("JSON number must be finite"))?;
        Ok(StrictJsonValue(Value::Number(number)))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictJsonValue>()? {
            values.push(value.into_inner());
        }
        Ok(StrictJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some((key, value)) = object.next_entry::<String, StrictJsonValue>()? {
            if values.contains_key(&key) {
                return Err(A::Error::custom("duplicate JSON object key"));
            }
            values.insert(key, value.into_inner());
        }
        Ok(StrictJsonValue(Value::Object(values)))
    }
}

fn render(value: &Value) -> Result<String> {
    let mut body = serde_json::to_string_pretty(value)?;
    body.push('\n');
    Ok(body)
}

fn server_value(shape: JsonServerShape) -> Value {
    server_value_for_command(shape, server_command())
}

fn server_value_for_command(shape: JsonServerShape, command: ServerCommand<'_>) -> Value {
    match shape {
        JsonServerShape::Plain => json!({
            "command": command.executable(),
            "args": command.args(),
        }),
        JsonServerShape::StdioType => json!({
            "type": "stdio",
            "command": command.executable(),
            "args": command.args(),
        }),
        JsonServerShape::OpenCodeLocal => json!({
            "type": "local",
            "command": command.argv(),
            "enabled": true,
        }),
        JsonServerShape::CopilotLocal => json!({
            "type": "local",
            "command": command.executable(),
            "args": command.args(),
            "tools": ["*"],
        }),
        JsonServerShape::ClineLocal => json!({
            "command": command.executable(),
            "args": command.args(),
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
    object.get("command").and_then(Value::as_str) == Some(server_command().executable())
}

fn command_array_is_current(value: Option<&Value>) -> bool {
    string_array_is(value, &server_command().argv())
}

fn args_are_current(value: Option<&Value>) -> bool {
    string_array_is(value, server_command().args())
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
    fn writer_preserves_adversarial_argv_without_shell_quoting() {
        let command = ServerCommand::new(
            r"C:\Program Files\ctx & tools\ctx-雪.exe",
            &[
                "",
                "two words",
                "$env:TEMP; $(touch /tmp/nope) | Out-Null",
                "O'Brien",
                "%PATH% ^ !",
            ],
        );

        let split = server_value_for_command(JsonServerShape::Plain, command);
        assert_eq!(split["command"], r"C:\Program Files\ctx & tools\ctx-雪.exe");
        assert_eq!(split["args"], json!(command.args()));

        let combined = server_value_for_command(JsonServerShape::OpenCodeLocal, command);
        assert_eq!(combined["command"], json!(command.argv()));
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

    #[test]
    fn remover_preserves_unrelated_json_and_empty_server_container() {
        let original =
            r#"{"other":true,"mcpServers":{"ctx":{"command":"ctx","args":["mcp","serve"]}}}"#;
        let removed = remove(
            original,
            JsonRoot::McpServers,
            JsonServerShape::Plain,
            false,
            json_path(),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&removed).unwrap();
        assert_eq!(value["other"], true);
        assert_eq!(value["mcpServers"], json!({}));
        assert_eq!(
            status(
                &removed,
                JsonRoot::McpServers,
                JsonServerShape::Plain,
                json_path()
            )
            .unwrap(),
            ConfigStatus::Missing
        );
        assert_eq!(
            remove(
                &removed,
                JsonRoot::McpServers,
                JsonServerShape::Plain,
                false,
                json_path(),
            )
            .unwrap(),
            removed
        );
    }

    #[test]
    fn remover_requires_force_for_conflicts_and_never_accepts_invalid_json() {
        let conflict = r#"{"mcpServers":{"ctx":{"command":"custom","args":[]}}}"#;
        assert!(remove(
            conflict,
            JsonRoot::McpServers,
            JsonServerShape::Plain,
            false,
            json_path(),
        )
        .is_err());
        let forced = remove(
            conflict,
            JsonRoot::McpServers,
            JsonServerShape::Plain,
            true,
            json_path(),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&forced).unwrap();
        assert_eq!(value["mcpServers"], json!({}));
        assert!(remove(
            "{ not json",
            JsonRoot::McpServers,
            JsonServerShape::Plain,
            true,
            json_path(),
        )
        .is_err());
    }

    #[test]
    fn remover_supports_jsonc_and_preserves_unrelated_values() {
        let body = r#"{
          // existing MiMo config
          "theme": "dark",
          "mcp": {
            "ctx": {"type": "local", "command": ["ctx", "mcp", "serve"]},
          },
        }"#;
        let removed = remove(
            body,
            JsonRoot::Mcp,
            JsonServerShape::OpenCodeLocal,
            false,
            jsonc_path(),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&removed).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["mcp"], json!({}));
    }

    #[test]
    fn duplicate_object_keys_are_invalid_in_json_and_jsonc() {
        let duplicate_ctx = r#"{
          "mcpServers": {
            "ctx": {"command": "custom", "args": []},
            "ctx": {"command": "ctx", "args": ["mcp", "serve"]}
          }
        }"#;
        let duplicate_parent = r#"{
          "mcpServers": {"other": {"command": "other"}},
          "mcpServers": {"ctx": {"command": "ctx", "args": ["mcp", "serve"]}}
        }"#;

        for (path, body) in [
            (json_path(), duplicate_ctx),
            (json_path(), duplicate_parent),
            (jsonc_path(), duplicate_ctx),
            (jsonc_path(), duplicate_parent),
        ] {
            assert!(status(body, JsonRoot::McpServers, JsonServerShape::Plain, path).is_err());
            assert!(remove(
                body,
                JsonRoot::McpServers,
                JsonServerShape::Plain,
                true,
                path,
            )
            .is_err());
        }
    }
}
