use anyhow::{anyhow, Context, Result};
use toml_edit::{
    value as toml_value, Array as TomlArray, DocumentMut, Item, Table, Value as TomlValue,
};

use super::super::{SERVER_ARGS, SERVER_COMMAND, SERVER_NAME};
use super::ConfigStatus;

pub(super) fn status(body: &str) -> Result<ConfigStatus> {
    let doc = body.parse::<DocumentMut>().context("parse TOML config")?;
    let Some(server) = doc
        .get("mcp_servers")
        .and_then(Item::as_table)
        .and_then(|servers| servers.get(SERVER_NAME))
        .and_then(Item::as_table)
    else {
        return Ok(ConfigStatus::Missing);
    };
    Ok(if server_is_current(server) {
        ConfigStatus::Current
    } else {
        ConfigStatus::Conflict
    })
}

pub(super) fn upsert(body: &str, force: bool) -> Result<String> {
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
    table["command"] = toml_value(SERVER_COMMAND);
    let mut args = TomlArray::default();
    for arg in SERVER_ARGS {
        args.push(*arg);
    }
    table["args"] = Item::Value(TomlValue::Array(args));
    servers[SERVER_NAME] = Item::Table(table);
    Ok(doc.to_string())
}

fn server_is_current(table: &Table) -> bool {
    let command_ok = table
        .get("command")
        .and_then(Item::as_str)
        .is_some_and(|command| command == SERVER_COMMAND);
    let args_ok = table
        .get("args")
        .and_then(Item::as_array)
        .is_some_and(|args| {
            args.iter()
                .filter_map(TomlValue::as_str)
                .eq(SERVER_ARGS.iter().copied())
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
}
