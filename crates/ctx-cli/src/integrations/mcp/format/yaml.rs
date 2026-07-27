use anyhow::{anyhow, Context, Result};

use super::super::SERVER_NAME;
use super::{server_command, ConfigStatus};

pub(super) fn status_continue(body: &str) -> Result<ConfigStatus> {
    let doc: serde_yaml::Value = serde_yaml::from_str(body).context("parse YAML config")?;
    let Some(servers) = mapping_get(&doc, "mcpServers") else {
        return Ok(ConfigStatus::Missing);
    };
    let servers = servers
        .as_sequence()
        .ok_or_else(|| anyhow!("mcpServers must be a YAML sequence"))?;
    let Some(server) = continue_server_by_name(servers) else {
        return Ok(ConfigStatus::Missing);
    };
    Ok(if continue_server_is_current(server) {
        ConfigStatus::Current
    } else {
        ConfigStatus::Conflict
    })
}

pub(super) fn upsert_continue(body: &str, force: bool) -> Result<String> {
    let mut doc = if body.trim().is_empty() {
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            serde_yaml::Value::String("name".to_owned()),
            serde_yaml::Value::String("ctx MCP".to_owned()),
        );
        mapping.insert(
            serde_yaml::Value::String("version".to_owned()),
            serde_yaml::Value::String("0.0.1".to_owned()),
        );
        mapping.insert(
            serde_yaml::Value::String("schema".to_owned()),
            serde_yaml::Value::String("v1".to_owned()),
        );
        serde_yaml::Value::Mapping(mapping)
    } else {
        serde_yaml::from_str(body).context("parse YAML config")?
    };
    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("YAML config root must be a mapping"))?;
    let servers_key = serde_yaml::Value::String("mcpServers".to_owned());
    let servers = root
        .entry(servers_key)
        .or_insert_with(|| serde_yaml::Value::Sequence(Vec::new()));
    let servers = servers
        .as_sequence_mut()
        .ok_or_else(|| anyhow!("mcpServers must be a YAML sequence"))?;
    if let Some(index) = continue_server_index(servers) {
        if continue_server_is_current(&servers[index]) {
            return render(&doc);
        }
        if !force {
            return Err(anyhow!(
                "existing ctx MCP server has different command or args"
            ));
        }
        servers[index] = continue_server_value();
    } else {
        servers.push(continue_server_value());
    }
    render(&doc)
}

pub(super) fn status_goose(body: &str) -> Result<ConfigStatus> {
    let doc: serde_yaml::Value = serde_yaml::from_str(body).context("parse YAML config")?;
    let Some(extensions) = mapping_get(&doc, "extensions") else {
        return Ok(ConfigStatus::Missing);
    };
    let Some(server) = mapping_get(extensions, SERVER_NAME) else {
        return Ok(ConfigStatus::Missing);
    };
    Ok(if goose_server_is_current(server) {
        ConfigStatus::Current
    } else {
        ConfigStatus::Conflict
    })
}

pub(super) fn upsert_goose(body: &str, force: bool) -> Result<String> {
    let mut doc = if body.trim().is_empty() {
        serde_yaml::Value::Mapping(Default::default())
    } else {
        serde_yaml::from_str(body).context("parse YAML config")?
    };
    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("YAML config root must be a mapping"))?;
    let extensions_key = serde_yaml::Value::String("extensions".to_owned());
    let extensions = root
        .entry(extensions_key)
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    let extensions = extensions
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("extensions must be a YAML mapping"))?;
    let ctx_key = serde_yaml::Value::String(SERVER_NAME.to_owned());
    if let Some(existing) = extensions.get(&ctx_key) {
        if goose_server_is_current(existing) {
            return render(&doc);
        }
        if !force {
            return Err(anyhow!(
                "existing ctx MCP extension has different command or args"
            ));
        }
    }
    extensions.insert(ctx_key, goose_server_value());
    render(&doc)
}

fn continue_server_by_name(servers: &[serde_yaml::Value]) -> Option<&serde_yaml::Value> {
    continue_server_index(servers).map(|index| &servers[index])
}

fn continue_server_index(servers: &[serde_yaml::Value]) -> Option<usize> {
    servers.iter().position(|server| {
        mapping_get(server, "name").and_then(serde_yaml::Value::as_str) == Some(SERVER_NAME)
    })
}

fn continue_server_value() -> serde_yaml::Value {
    let command = server_command();
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(
        serde_yaml::Value::String("name".to_owned()),
        serde_yaml::Value::String(SERVER_NAME.to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("type".to_owned()),
        serde_yaml::Value::String("stdio".to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("command".to_owned()),
        serde_yaml::Value::String(command.executable().to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("args".to_owned()),
        serde_yaml::Value::Sequence(
            command
                .args()
                .iter()
                .map(|arg| serde_yaml::Value::String((*arg).to_owned()))
                .collect(),
        ),
    );
    serde_yaml::Value::Mapping(mapping)
}

fn continue_server_is_current(value: &serde_yaml::Value) -> bool {
    let expected = server_command();
    let Some(mapping) = value.as_mapping() else {
        return false;
    };
    let configured_command = mapping
        .get(serde_yaml::Value::String("command".to_owned()))
        .and_then(serde_yaml::Value::as_str);
    let args = mapping
        .get(serde_yaml::Value::String("args".to_owned()))
        .and_then(serde_yaml::Value::as_sequence);
    configured_command == Some(expected.executable()) && args_are_current(args)
}

fn goose_server_value() -> serde_yaml::Value {
    let command = server_command();
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(
        serde_yaml::Value::String("enabled".to_owned()),
        serde_yaml::Value::Bool(true),
    );
    mapping.insert(
        serde_yaml::Value::String("name".to_owned()),
        serde_yaml::Value::String(SERVER_NAME.to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("display_name".to_owned()),
        serde_yaml::Value::String("ctx".to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("type".to_owned()),
        serde_yaml::Value::String("stdio".to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("cmd".to_owned()),
        serde_yaml::Value::String(command.executable().to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("args".to_owned()),
        serde_yaml::Value::Sequence(
            command
                .args()
                .iter()
                .map(|arg| serde_yaml::Value::String((*arg).to_owned()))
                .collect(),
        ),
    );
    mapping.insert(
        serde_yaml::Value::String("timeout".to_owned()),
        serde_yaml::Value::Number(300.into()),
    );
    serde_yaml::Value::Mapping(mapping)
}

fn goose_server_is_current(value: &serde_yaml::Value) -> bool {
    let expected = server_command();
    let Some(mapping) = value.as_mapping() else {
        return false;
    };
    let cmd = mapping
        .get(serde_yaml::Value::String("cmd".to_owned()))
        .and_then(serde_yaml::Value::as_str)
        .or_else(|| {
            mapping
                .get(serde_yaml::Value::String("command".to_owned()))
                .and_then(serde_yaml::Value::as_str)
        });
    let args = mapping
        .get(serde_yaml::Value::String("args".to_owned()))
        .and_then(serde_yaml::Value::as_sequence);
    cmd == Some(expected.executable()) && args_are_current(args)
}

fn args_are_current(args: Option<&Vec<serde_yaml::Value>>) -> bool {
    let expected = server_command();
    args.is_some_and(|args| {
        args.iter()
            .filter_map(serde_yaml::Value::as_str)
            .eq(expected.args().iter().copied())
    })
}

fn mapping_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    value
        .as_mapping()?
        .get(serde_yaml::Value::String(key.to_owned()))
}

fn render(value: &serde_yaml::Value) -> Result<String> {
    let mut body = serde_yaml::to_string(value)?;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goose_writer_adds_extension_and_is_idempotent() {
        let first = upsert_goose("GOOSE_MODEL: test\n", false).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&first).unwrap();
        let ctx = mapping_get(mapping_get(&value, "extensions").unwrap(), "ctx").unwrap();
        assert_eq!(mapping_get(ctx, "cmd").unwrap().as_str(), Some("ctx"));
        assert_eq!(status_goose(&first).unwrap(), ConfigStatus::Current);
        let second = upsert_goose(&first, false).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn continue_writer_adds_named_server_and_is_idempotent() {
        let first = upsert_continue("name: Local\nversion: 1.0.0\nschema: v1\n", false).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&first).unwrap();
        let servers = mapping_get(&value, "mcpServers")
            .unwrap()
            .as_sequence()
            .unwrap();
        let ctx = continue_server_by_name(servers).unwrap();
        assert_eq!(mapping_get(ctx, "command").unwrap().as_str(), Some("ctx"));
        assert_eq!(status_continue(&first).unwrap(), ConfigStatus::Current);
        let second = upsert_continue(&first, false).unwrap();
        assert_eq!(first, second);
    }
}
