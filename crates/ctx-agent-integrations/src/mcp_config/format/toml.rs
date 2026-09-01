use anyhow::{anyhow, Context, Result};
use toml_edit::{
    value as toml_value, Array as TomlArray, DocumentMut, Item, Table, Value as TomlValue,
};

use super::super::SERVER_NAME;
use super::{server_command, ConfigStatus};

pub fn status(body: &str) -> Result<ConfigStatus> {
    let doc = body.parse::<DocumentMut>().context("parse TOML config")?;
    let Some(servers) = doc.get("mcp_servers") else {
        return Ok(ConfigStatus::Missing);
    };
    let servers = servers
        .as_table()
        .ok_or_else(|| anyhow!("mcp_servers must be a TOML table"))?;
    let Some(server) = servers.get(SERVER_NAME) else {
        return Ok(ConfigStatus::Missing);
    };
    Ok(if server.as_table().is_some_and(server_is_current) {
        ConfigStatus::Current
    } else {
        ConfigStatus::Conflict
    })
}

pub fn upsert(body: &str, force: bool) -> Result<String> {
    let mut doc = if body.trim().is_empty() {
        DocumentMut::new()
    } else {
        body.parse::<DocumentMut>().context("parse TOML config")?
    };
    if !doc.contains_key("mcp_servers") {
        doc["mcp_servers"] = Item::Table(Table::new());
    }
    let servers = doc["mcp_servers"]
        .as_table_mut()
        .ok_or_else(|| anyhow!("mcp_servers must be a TOML table"))?;
    if let Some(existing) = servers.get(SERVER_NAME).and_then(Item::as_table) {
        if server_is_current(existing) {
            return Ok(doc.to_string());
        }
        if !force {
            return Err(anyhow!(
                "existing ctx MCP server has different command or args"
            ));
        }
    }
    let mut table = Table::new();
    let command = server_command();
    table["command"] = toml_value(command.executable());
    let mut args = TomlArray::default();
    for arg in command.args() {
        args.push(*arg);
    }
    table["args"] = Item::Value(TomlValue::Array(args));
    servers[SERVER_NAME] = Item::Table(table);
    Ok(doc.to_string())
}

pub fn remove(body: &str, force: bool) -> Result<String> {
    let mut doc = body.parse::<DocumentMut>().context("parse TOML config")?;
    let Some(servers) = doc.get_mut("mcp_servers") else {
        return Ok(body.to_owned());
    };
    let servers = servers
        .as_table_mut()
        .ok_or_else(|| anyhow!("mcp_servers must be a TOML table"))?;
    let Some(existing) = servers.get(SERVER_NAME) else {
        return Ok(body.to_owned());
    };
    if !existing.as_table().is_some_and(server_is_current) && !force {
        return Err(anyhow!(
            "existing ctx MCP server has different command or args"
        ));
    }
    servers.remove(SERVER_NAME);
    servers.set_implicit(false);
    Ok(doc.to_string())
}

fn server_is_current(table: &Table) -> bool {
    let command = server_command();
    let command_ok = table
        .get("command")
        .and_then(Item::as_str)
        .is_some_and(|value| value == command.executable());
    let args_ok = table
        .get("args")
        .and_then(Item::as_array)
        .is_some_and(|args| {
            args.len() == command.args().len()
                && args
                    .iter()
                    .zip(command.args().iter().copied())
                    .all(|(value, expected)| value.as_str() == Some(expected))
        });
    command_ok && args_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_preserves_existing_settings_and_is_idempotent() {
        let first = upsert("model = \"gpt-5\"\n", false).unwrap();
        assert!(first.contains("model = \"gpt-5\""));
        assert!(first.contains("[mcp_servers.ctx]"));
        assert_eq!(status(&first).unwrap(), ConfigStatus::Current);
        let second = upsert(&first, false).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn malformed_mixed_args_are_conflicting_and_repaired_only_when_forced() {
        let malformed = r#"[mcp_servers.ctx]
command = "ctx"
args = ["mcp", 7, "serve"]
"#;
        assert_eq!(status(malformed).unwrap(), ConfigStatus::Conflict);
        assert!(upsert(malformed, false).is_err());

        let repaired = upsert(malformed, true).unwrap();
        assert_eq!(status(&repaired).unwrap(), ConfigStatus::Current);
        let parsed = repaired.parse::<DocumentMut>().unwrap();
        let args = parsed["mcp_servers"][SERVER_NAME]["args"]
            .as_array()
            .unwrap();
        assert_eq!(args.len(), server_command().args().len());
        assert!(args
            .iter()
            .zip(server_command().args())
            .all(|(value, expected)| value.as_str() == Some(*expected)));
    }

    #[test]
    fn remover_preserves_unrelated_toml_and_empty_server_table() {
        let original = r#"model = "gpt-5"

[mcp_servers.ctx]
command = "ctx"
args = ["mcp", "serve"]
"#;
        let removed = remove(original, false).unwrap();
        assert!(removed.contains("model = \"gpt-5\""));
        assert!(removed.contains("[mcp_servers]"));
        assert!(!removed.contains("[mcp_servers.ctx]"));
        assert_eq!(status(&removed).unwrap(), ConfigStatus::Missing);
        assert_eq!(remove(&removed, false).unwrap(), removed);
    }

    #[test]
    fn remover_requires_force_for_any_conflicting_ctx_key() {
        let conflict = "[mcp_servers]\nctx = \"custom\"\n";
        assert_eq!(status(conflict).unwrap(), ConfigStatus::Conflict);
        assert!(remove(conflict, false).is_err());
        let removed = remove(conflict, true).unwrap();
        assert!(removed.contains("[mcp_servers]"));
        assert!(!removed.contains("ctx ="));
        assert!(remove("not valid = [", true).is_err());
    }
}
