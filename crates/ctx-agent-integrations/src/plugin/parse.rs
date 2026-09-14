use serde_json::{Map, Value};

use super::{
    PluginAgent, PluginInstallStatus, PluginMarketplaceStatus, PluginScope, LEGACY_PLUGIN_ID,
    MARKETPLACE_NAME, MARKETPLACE_SOURCE, PLUGIN_ID,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledPlugin {
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PluginInventory {
    pub current: Option<InstalledPlugin>,
    pub legacy: Option<InstalledPlugin>,
}

impl PluginInventory {
    pub fn status(&self) -> PluginInstallStatus {
        if self.current.is_some() {
            PluginInstallStatus::Installed
        } else if self.legacy.is_some() {
            PluginInstallStatus::LegacyInstalled
        } else {
            PluginInstallStatus::Missing
        }
    }
}

pub(crate) fn parse_marketplaces(
    agent: PluginAgent,
    value: &Value,
) -> Result<PluginMarketplaceStatus, ()> {
    let entries = match agent {
        PluginAgent::Codex => value
            .as_object()
            .and_then(|object| object.get("marketplaces"))
            .and_then(Value::as_array)
            .ok_or(())?,
        PluginAgent::ClaudeCode => value.as_array().ok_or(())?,
        PluginAgent::Cursor => return Err(()),
    };

    let mut found = false;
    for entry in entries {
        let object = entry.as_object().ok_or(())?;
        let name = object.get("name").and_then(Value::as_str).ok_or(())?;
        if name != MARKETPLACE_NAME {
            continue;
        }
        found = true;
        if !marketplace_is_ctx_source(agent, object)? {
            return Ok(PluginMarketplaceStatus::Conflict);
        }
    }
    Ok(if found {
        PluginMarketplaceStatus::Present
    } else {
        PluginMarketplaceStatus::Missing
    })
}

fn marketplace_is_ctx_source(agent: PluginAgent, object: &Map<String, Value>) -> Result<bool, ()> {
    match agent {
        PluginAgent::Codex => {
            let source = object
                .get("marketplaceSource")
                .and_then(Value::as_object)
                .ok_or(())?;
            let discriminator = source.get("type").and_then(Value::as_str).ok_or(())?;
            let identity = source.get("value").and_then(Value::as_str).ok_or(())?;
            Ok(discriminator == "github" && identity == MARKETPLACE_SOURCE)
        }
        PluginAgent::ClaudeCode => {
            let discriminator = object.get("source").and_then(Value::as_str).ok_or(())?;
            let identity = object.get("repo").and_then(Value::as_str).ok_or(())?;
            Ok(discriminator == "github" && identity == MARKETPLACE_SOURCE)
        }
        PluginAgent::Cursor => Err(()),
    }
}

pub(crate) fn parse_plugins(
    agent: PluginAgent,
    scope: PluginScope,
    value: &Value,
) -> Result<PluginInventory, ()> {
    let entries = match agent {
        PluginAgent::Codex => value
            .as_object()
            .and_then(|object| object.get("installed"))
            .and_then(Value::as_array)
            .ok_or(())?,
        PluginAgent::ClaudeCode => value.as_array().ok_or(())?,
        PluginAgent::Cursor => return Err(()),
    };
    let mut inventory = PluginInventory::default();
    let mut codex_current_seen = false;
    let mut codex_legacy_seen = false;
    for entry in entries {
        let object = entry.as_object().ok_or(())?;
        let (id, version) = match agent {
            PluginAgent::Codex => {
                let plugin = parse_codex_plugin(object)?;
                let seen = match plugin.id {
                    PLUGIN_ID => Some(&mut codex_current_seen),
                    LEGACY_PLUGIN_ID => Some(&mut codex_legacy_seen),
                    _ => None,
                };
                if let Some(seen) = seen {
                    if *seen {
                        return Err(());
                    }
                    *seen = true;
                }
                if !plugin.installed {
                    continue;
                }
                (plugin.id, plugin.version)
            }
            PluginAgent::ClaudeCode => {
                let Some(plugin) = parse_claude_plugin(object, scope)? else {
                    continue;
                };
                plugin
            }
            PluginAgent::Cursor => return Err(()),
        };
        if id != PLUGIN_ID && id != LEGACY_PLUGIN_ID {
            continue;
        }
        let slot = if id == PLUGIN_ID {
            &mut inventory.current
        } else {
            &mut inventory.legacy
        };
        if slot.is_some() {
            return Err(());
        }
        *slot = Some(InstalledPlugin { version });
    }
    Ok(inventory)
}

struct CodexPlugin<'a> {
    id: &'a str,
    installed: bool,
    version: Option<String>,
}

fn parse_codex_plugin(object: &Map<String, Value>) -> Result<CodexPlugin<'_>, ()> {
    let id = object.get("pluginId").and_then(Value::as_str).ok_or(())?;
    let installed = object.get("installed").and_then(Value::as_bool).ok_or(())?;
    let version = optional_version(object)?;
    let name = optional_string(object, "name")?;
    let marketplace_name = optional_string(object, "marketplaceName")?;

    let expected_name = match id {
        PLUGIN_ID => Some("ctx"),
        LEGACY_PLUGIN_ID => Some("ctx-agent-history-search"),
        _ => None,
    };
    if let Some(expected_name) = expected_name {
        if name != Some(expected_name) || marketplace_name != Some(MARKETPLACE_NAME) {
            return Err(());
        }
    } else if marketplace_name == Some(MARKETPLACE_NAME)
        && matches!(name, Some("ctx" | "ctx-agent-history-search"))
    {
        // A record may not claim either trusted target identity under a
        // different pluginId.
        return Err(());
    }
    Ok(CodexPlugin {
        id,
        installed,
        version,
    })
}

fn parse_claude_plugin(
    object: &Map<String, Value>,
    expected_scope: PluginScope,
) -> Result<Option<(&str, Option<String>)>, ()> {
    let id = object.get("id").and_then(Value::as_str).ok_or(())?;
    let scope = object.get("scope").and_then(Value::as_str).ok_or(())?;
    if !matches!(scope, "user" | "project" | "local") {
        return Err(());
    }
    let version = optional_version(object)?;
    Ok((scope == expected_scope.claude_scope()).then_some((id, version)))
}

fn optional_version(object: &Map<String, Value>) -> Result<Option<String>, ()> {
    match object.get("version") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(version)) => Ok(Some(version.clone())),
        Some(_) => Err(()),
    }
}

fn optional_string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<Option<&'a str>, ()> {
    match object.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(()),
    }
}
