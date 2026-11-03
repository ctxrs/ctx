use anyhow::{anyhow, Context, Result};

use super::super::SERVER_NAME;
use super::{server_command, ConfigStatus};

pub fn status_continue(body: &str) -> Result<ConfigStatus> {
    let doc: yaml_serde::Value = yaml_serde::from_str(body).context("parse YAML config")?;
    let root = doc
        .as_mapping()
        .ok_or_else(|| anyhow!("YAML config root must be a mapping"))?;
    let Some(servers) = root.get(yaml_serde::Value::String("mcpServers".to_owned())) else {
        return Ok(ConfigStatus::Missing);
    };
    let servers = servers
        .as_sequence()
        .ok_or_else(|| anyhow!("mcpServers must be a YAML sequence"))?;
    let matching = servers
        .iter()
        .filter(|server| continue_server_has_name(server))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Ok(ConfigStatus::Missing);
    }
    Ok(
        if matching
            .iter()
            .all(|server| continue_server_is_current(server))
        {
            ConfigStatus::Current
        } else {
            ConfigStatus::Conflict
        },
    )
}

pub fn upsert_continue(body: &str, force: bool) -> Result<String> {
    let mut doc = if body.trim().is_empty() {
        let mut mapping = yaml_serde::Mapping::new();
        mapping.insert(
            yaml_serde::Value::String("name".to_owned()),
            yaml_serde::Value::String("ctx MCP".to_owned()),
        );
        mapping.insert(
            yaml_serde::Value::String("version".to_owned()),
            yaml_serde::Value::String("0.0.1".to_owned()),
        );
        mapping.insert(
            yaml_serde::Value::String("schema".to_owned()),
            yaml_serde::Value::String("v1".to_owned()),
        );
        yaml_serde::Value::Mapping(mapping)
    } else {
        yaml_serde::from_str(body).context("parse YAML config")?
    };
    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("YAML config root must be a mapping"))?;
    let servers_key = yaml_serde::Value::String("mcpServers".to_owned());
    let servers = root
        .entry(servers_key)
        .or_insert_with(|| yaml_serde::Value::Sequence(Vec::new()));
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

pub fn remove_continue(body: &str, force: bool) -> Result<String> {
    let mut doc: yaml_serde::Value = yaml_serde::from_str(body).context("parse YAML config")?;
    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("YAML config root must be a mapping"))?;
    let Some(servers) = root.get_mut(yaml_serde::Value::String("mcpServers".to_owned())) else {
        return Ok(body.to_owned());
    };
    let servers = servers
        .as_sequence_mut()
        .ok_or_else(|| anyhow!("mcpServers must be a YAML sequence"))?;
    let matching = servers
        .iter()
        .filter(|server| continue_server_has_name(server))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Ok(body.to_owned());
    }
    if !force
        && matching
            .iter()
            .any(|server| !continue_server_is_current(server))
    {
        return Err(anyhow!(
            "existing ctx MCP server has different command or args"
        ));
    }
    servers.retain(|server| !continue_server_has_name(server));
    render(&doc)
}

pub fn status_goose(body: &str) -> Result<ConfigStatus> {
    let doc: yaml_serde::Value = yaml_serde::from_str(body).context("parse YAML config")?;
    let root = doc
        .as_mapping()
        .ok_or_else(|| anyhow!("YAML config root must be a mapping"))?;
    let Some(extensions) = root.get(yaml_serde::Value::String("extensions".to_owned())) else {
        return Ok(ConfigStatus::Missing);
    };
    let extensions = extensions
        .as_mapping()
        .ok_or_else(|| anyhow!("extensions must be a YAML mapping"))?;
    let Some(server) = extensions.get(yaml_serde::Value::String(SERVER_NAME.to_owned())) else {
        return Ok(ConfigStatus::Missing);
    };
    Ok(if goose_server_is_current(server) {
        ConfigStatus::Current
    } else {
        ConfigStatus::Conflict
    })
}

pub fn upsert_goose(body: &str, force: bool) -> Result<String> {
    let mut doc = if body.trim().is_empty() {
        yaml_serde::Value::Mapping(Default::default())
    } else {
        yaml_serde::from_str(body).context("parse YAML config")?
    };
    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("YAML config root must be a mapping"))?;
    let extensions_key = yaml_serde::Value::String("extensions".to_owned());
    let extensions = root
        .entry(extensions_key)
        .or_insert_with(|| yaml_serde::Value::Mapping(Default::default()));
    let extensions = extensions
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("extensions must be a YAML mapping"))?;
    let ctx_key = yaml_serde::Value::String(SERVER_NAME.to_owned());
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

pub fn remove_goose(body: &str, force: bool) -> Result<String> {
    let mut doc: yaml_serde::Value = yaml_serde::from_str(body).context("parse YAML config")?;
    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("YAML config root must be a mapping"))?;
    let Some(extensions) = root.get_mut(yaml_serde::Value::String("extensions".to_owned())) else {
        return Ok(body.to_owned());
    };
    let extensions = extensions
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("extensions must be a YAML mapping"))?;
    let ctx_key = yaml_serde::Value::String(SERVER_NAME.to_owned());
    let Some(existing) = extensions.get(&ctx_key) else {
        return Ok(body.to_owned());
    };
    if !goose_server_is_current(existing) && !force {
        return Err(anyhow!(
            "existing ctx MCP extension has different command or args"
        ));
    }
    extensions.shift_remove(&ctx_key);
    render(&doc)
}

#[cfg(test)]
fn continue_server_by_name(servers: &[yaml_serde::Value]) -> Option<&yaml_serde::Value> {
    continue_server_index(servers).map(|index| &servers[index])
}

fn continue_server_index(servers: &[yaml_serde::Value]) -> Option<usize> {
    servers.iter().position(continue_server_has_name)
}

fn continue_server_has_name(server: &yaml_serde::Value) -> bool {
    mapping_get(server, "name").and_then(yaml_serde::Value::as_str) == Some(SERVER_NAME)
}

fn continue_server_value() -> yaml_serde::Value {
    let command = server_command();
    let mut mapping = yaml_serde::Mapping::new();
    mapping.insert(
        yaml_serde::Value::String("name".to_owned()),
        yaml_serde::Value::String(SERVER_NAME.to_owned()),
    );
    mapping.insert(
        yaml_serde::Value::String("type".to_owned()),
        yaml_serde::Value::String("stdio".to_owned()),
    );
    mapping.insert(
        yaml_serde::Value::String("command".to_owned()),
        yaml_serde::Value::String(command.executable().to_owned()),
    );
    mapping.insert(
        yaml_serde::Value::String("args".to_owned()),
        yaml_serde::Value::Sequence(
            command
                .args()
                .iter()
                .map(|arg| yaml_serde::Value::String((*arg).to_owned()))
                .collect(),
        ),
    );
    yaml_serde::Value::Mapping(mapping)
}

fn continue_server_is_current(value: &yaml_serde::Value) -> bool {
    let expected = server_command();
    let Some(mapping) = value.as_mapping() else {
        return false;
    };
    let configured_command = mapping
        .get(yaml_serde::Value::String("command".to_owned()))
        .and_then(yaml_serde::Value::as_str);
    let args = mapping
        .get(yaml_serde::Value::String("args".to_owned()))
        .and_then(yaml_serde::Value::as_sequence);
    configured_command == Some(expected.executable()) && args_are_current(args)
}

fn goose_server_value() -> yaml_serde::Value {
    let command = server_command();
    let mut mapping = yaml_serde::Mapping::new();
    mapping.insert(
        yaml_serde::Value::String("enabled".to_owned()),
        yaml_serde::Value::Bool(true),
    );
    mapping.insert(
        yaml_serde::Value::String("name".to_owned()),
        yaml_serde::Value::String(SERVER_NAME.to_owned()),
    );
    mapping.insert(
        yaml_serde::Value::String("display_name".to_owned()),
        yaml_serde::Value::String("ctx".to_owned()),
    );
    mapping.insert(
        yaml_serde::Value::String("type".to_owned()),
        yaml_serde::Value::String("stdio".to_owned()),
    );
    mapping.insert(
        yaml_serde::Value::String("cmd".to_owned()),
        yaml_serde::Value::String(command.executable().to_owned()),
    );
    mapping.insert(
        yaml_serde::Value::String("args".to_owned()),
        yaml_serde::Value::Sequence(
            command
                .args()
                .iter()
                .map(|arg| yaml_serde::Value::String((*arg).to_owned()))
                .collect(),
        ),
    );
    mapping.insert(
        yaml_serde::Value::String("timeout".to_owned()),
        yaml_serde::Value::Number(300.into()),
    );
    yaml_serde::Value::Mapping(mapping)
}

fn goose_server_is_current(value: &yaml_serde::Value) -> bool {
    let expected = server_command();
    let Some(mapping) = value.as_mapping() else {
        return false;
    };
    let cmd = mapping
        .get(yaml_serde::Value::String("cmd".to_owned()))
        .and_then(yaml_serde::Value::as_str)
        .or_else(|| {
            mapping
                .get(yaml_serde::Value::String("command".to_owned()))
                .and_then(yaml_serde::Value::as_str)
        });
    let args = mapping
        .get(yaml_serde::Value::String("args".to_owned()))
        .and_then(yaml_serde::Value::as_sequence);
    cmd == Some(expected.executable()) && args_are_current(args)
}

fn args_are_current(args: Option<&Vec<yaml_serde::Value>>) -> bool {
    let expected = server_command();
    args.is_some_and(|args| {
        args.len() == expected.args().len()
            && args
                .iter()
                .zip(expected.args().iter().copied())
                .all(|(value, expected)| value.as_str() == Some(expected))
    })
}

fn mapping_get<'a>(value: &'a yaml_serde::Value, key: &str) -> Option<&'a yaml_serde::Value> {
    value
        .as_mapping()?
        .get(yaml_serde::Value::String(key.to_owned()))
}

fn render(value: &yaml_serde::Value) -> Result<String> {
    let mut body = yaml_serde::to_string(value)?;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_writers_preserve_aliases_merge_keys_tags_and_scalar_types() {
        let original = "base: &base {enabled: true, retries: 3}\nalias: *base\nmetadata:\n  <<: *base\n  numeric_text: '001'\n  absent: null\n  tagged: !custom payload\n  note: |\n    first\n    second\n";
        for rendered in [
            upsert_continue(original, false).unwrap(),
            upsert_goose(original, false).unwrap(),
        ] {
            let value: yaml_serde::Value = yaml_serde::from_str(&rendered).unwrap();
            assert_eq!(value["alias"]["enabled"].as_bool(), Some(true));
            assert_eq!(value["alias"]["retries"].as_u64(), Some(3));
            assert_eq!(value["metadata"]["<<"]["enabled"].as_bool(), Some(true));
            assert_eq!(value["metadata"]["<<"]["retries"].as_u64(), Some(3));
            assert_eq!(value["metadata"]["numeric_text"].as_str(), Some("001"));
            assert!(value["metadata"]["absent"].is_null());
            assert_eq!(value["metadata"]["note"].as_str(), Some("first\nsecond\n"));
            let yaml_serde::Value::Tagged(tagged) = &value["metadata"]["tagged"] else {
                panic!("unrelated custom YAML tag was lost");
            };
            assert_eq!(tagged.tag.to_string(), "!custom");
            assert_eq!(tagged.value.as_str(), Some("payload"));
        }
    }

    #[test]
    fn yaml_writers_reject_duplicate_keys_and_multiple_documents() {
        for original in [
            "key: first\nkey: second\n",
            "key: first\n---\nkey: second\n",
        ] {
            assert!(upsert_continue(original, false).is_err());
            assert!(upsert_goose(original, false).is_err());
        }
    }

    #[test]
    fn goose_writer_adds_extension_and_is_idempotent() {
        let first = upsert_goose("GOOSE_MODEL: test\n", false).unwrap();
        let value: yaml_serde::Value = yaml_serde::from_str(&first).unwrap();
        let ctx = mapping_get(mapping_get(&value, "extensions").unwrap(), "ctx").unwrap();
        assert_eq!(mapping_get(ctx, "cmd").unwrap().as_str(), Some("ctx"));
        assert_eq!(status_goose(&first).unwrap(), ConfigStatus::Current);
        let second = upsert_goose(&first, false).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn continue_writer_adds_named_server_and_is_idempotent() {
        let first = upsert_continue("name: Local\nversion: 1.0.0\nschema: v1\n", false).unwrap();
        let value: yaml_serde::Value = yaml_serde::from_str(&first).unwrap();
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

    #[test]
    fn malformed_mixed_continue_args_are_conflicting_and_repaired_only_when_forced() {
        let malformed = "mcpServers:\n  - name: ctx\n    type: stdio\n    command: ctx\n    args: [mcp, 7, serve]\n";
        assert_eq!(status_continue(malformed).unwrap(), ConfigStatus::Conflict);
        assert!(upsert_continue(malformed, false).is_err());

        let repaired = upsert_continue(malformed, true).unwrap();
        assert_eq!(status_continue(&repaired).unwrap(), ConfigStatus::Current);
        let value: yaml_serde::Value = yaml_serde::from_str(&repaired).unwrap();
        let servers = mapping_get(&value, "mcpServers")
            .unwrap()
            .as_sequence()
            .unwrap();
        let args = mapping_get(continue_server_by_name(servers).unwrap(), "args")
            .unwrap()
            .as_sequence()
            .unwrap();
        assert_eq!(
            args,
            &server_command()
                .args()
                .iter()
                .map(|arg| yaml_serde::Value::String((*arg).to_owned()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn malformed_mixed_goose_args_are_conflicting_and_repaired_only_when_forced() {
        let malformed = "extensions:\n  ctx:\n    enabled: true\n    name: ctx\n    type: stdio\n    cmd: ctx\n    args: [mcp, false, serve]\n";
        assert_eq!(status_goose(malformed).unwrap(), ConfigStatus::Conflict);
        assert!(upsert_goose(malformed, false).is_err());

        let repaired = upsert_goose(malformed, true).unwrap();
        assert_eq!(status_goose(&repaired).unwrap(), ConfigStatus::Current);
        let value: yaml_serde::Value = yaml_serde::from_str(&repaired).unwrap();
        let args = mapping_get(
            mapping_get(mapping_get(&value, "extensions").unwrap(), "ctx").unwrap(),
            "args",
        )
        .unwrap()
        .as_sequence()
        .unwrap();
        assert_eq!(
            args,
            &server_command()
                .args()
                .iter()
                .map(|arg| yaml_serde::Value::String((*arg).to_owned()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn continue_remover_preserves_unrelated_entries_and_empty_sequence() {
        let current_only =
            "name: Local\nmcpServers:\n  - name: ctx\n    command: ctx\n    args: [mcp, serve]\n";
        let removed = remove_continue(current_only, false).unwrap();
        let value: yaml_serde::Value = yaml_serde::from_str(&removed).unwrap();
        assert_eq!(
            mapping_get(&value, "name").and_then(yaml_serde::Value::as_str),
            Some("Local")
        );
        assert!(mapping_get(&value, "mcpServers")
            .unwrap()
            .as_sequence()
            .unwrap()
            .is_empty());
        assert_eq!(remove_continue(&removed, false).unwrap(), removed);

        let with_other = "mcpServers:\n  - name: ctx\n    command: ctx\n    args: [mcp, serve]\n  - name: other\n    command: other\n    args: []\n";
        let removed = remove_continue(with_other, false).unwrap();
        assert!(removed.contains("name: other"));
        assert!(!removed.contains("name: ctx"));
    }

    #[test]
    fn continue_remover_requires_force_for_conflicts_and_rejects_invalid_yaml() {
        let conflict = "mcpServers:\n  - name: ctx\n    command: custom\n    args: []\n";
        assert!(remove_continue(conflict, false).is_err());
        assert_eq!(status_continue(conflict).unwrap(), ConfigStatus::Conflict);
        assert_eq!(
            status_continue(&remove_continue(conflict, true).unwrap()).unwrap(),
            ConfigStatus::Missing
        );
        assert!(remove_continue("mcpServers: [", true).is_err());
        assert!(status_continue("[]").is_err());
        assert!(status_continue("mcpServers: {}").is_err());
    }

    #[test]
    fn goose_remover_preserves_unrelated_values_and_empty_mapping() {
        let current_only =
            "GOOSE_MODEL: test\nextensions:\n  ctx:\n    cmd: ctx\n    args: [mcp, serve]\n";
        let removed = remove_goose(current_only, false).unwrap();
        let value: yaml_serde::Value = yaml_serde::from_str(&removed).unwrap();
        assert_eq!(
            mapping_get(&value, "GOOSE_MODEL").and_then(yaml_serde::Value::as_str),
            Some("test")
        );
        assert!(mapping_get(&value, "extensions")
            .unwrap()
            .as_mapping()
            .unwrap()
            .is_empty());
        assert_eq!(remove_goose(&removed, false).unwrap(), removed);

        let with_other = "extensions:\n  other:\n    cmd: other\n    args: []\n  ctx:\n    cmd: ctx\n    args: [mcp, serve]\n";
        let removed = remove_goose(with_other, false).unwrap();
        let value: yaml_serde::Value = yaml_serde::from_str(&removed).unwrap();
        let extensions = mapping_get(&value, "extensions").unwrap();
        assert!(mapping_get(extensions, "ctx").is_none());
        assert_eq!(
            mapping_get(mapping_get(extensions, "other").unwrap(), "cmd")
                .and_then(yaml_serde::Value::as_str),
            Some("other")
        );
    }

    #[test]
    fn goose_remover_requires_force_for_conflicts_and_rejects_invalid_yaml() {
        let conflict = "extensions:\n  ctx:\n    cmd: custom\n    args: []\n";
        assert!(remove_goose(conflict, false).is_err());
        assert_eq!(status_goose(conflict).unwrap(), ConfigStatus::Conflict);
        assert_eq!(
            status_goose(&remove_goose(conflict, true).unwrap()).unwrap(),
            ConfigStatus::Missing
        );
        assert!(remove_goose("extensions: [", true).is_err());
        assert!(status_goose("[]").is_err());
        assert!(status_goose("extensions: []").is_err());
    }
}
