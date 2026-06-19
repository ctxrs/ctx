use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const PLUGIN_MANIFEST_SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    #[serde(default = "default_plugin_manifest_schema_version")]
    pub schema_version: i64,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entrypoints: Vec<PluginEntrypoint>,
    #[serde(default, skip_serializing_if = "PluginContributions::is_empty")]
    pub contributes: PluginContributions,
    #[serde(default, skip_serializing_if = "PluginCompatibility::is_empty")]
    pub compatibility: PluginCompatibility,
}

impl PluginManifest {
    pub fn validate(&self) -> Result<(), PluginManifestValidationError> {
        validate_plugin_manifest(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginEntrypoint {
    pub id: String,
    #[serde(default)]
    pub kind: PluginEntrypointKind,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PluginEntrypointKind {
    #[default]
    Process,
    Worker,
    Webview,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct PluginContributions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<PluginProviderContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<PluginRuntimeContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<PluginCommandContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collectors: Vec<PluginCollectorContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observers: Vec<PluginObserverContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ui_surfaces: Vec<PluginUiSurfaceContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<PluginWorkbenchTemplateContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toolbar_actions: Vec<PluginWorkbenchToolbarActionContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_renderers: Vec<PluginArtifactRendererContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub card_renderers: Vec<PluginWorkbenchCardRendererContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail_sections: Vec<PluginWorkbenchSectionContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_sections: Vec<PluginWorkbenchSectionContribution>,
}

impl PluginContributions {
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
            && self.runtimes.is_empty()
            && self.commands.is_empty()
            && self.collectors.is_empty()
            && self.observers.is_empty()
            && self.ui_surfaces.is_empty()
            && self.templates.is_empty()
            && self.toolbar_actions.is_empty()
            && self.artifact_renderers.is_empty()
            && self.card_renderers.is_empty()
            && self.detail_sections.is_empty()
            && self.review_sections.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginProviderContribution {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginRuntimeContribution {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginCommandContribution {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginCollectorContribution {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginObserverContribution {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginUiSurfaceContribution {
    pub id: String,
    pub name: String,
    pub surface: PluginUiSurfaceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginUiSurfaceKind {
    Panel,
    Sidebar,
    StatusBar,
    CommandPalette,
    Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginDeclarativeWorkbenchContribution {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginWorkbenchTemplateContribution {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_sources: Vec<String>,
    pub title: String,
    pub template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginWorkbenchToolbarActionContribution {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_sources: Vec<String>,
    pub title: String,
    #[serde(
        default,
        deserialize_with = "deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub command: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub action: Option<ApprovedCtxActionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ApprovedCtxActionId {
    #[serde(rename = "work.focus")]
    WorkFocus,
    #[serde(rename = "task.start")]
    TaskStart,
    #[serde(rename = "ctx.command.run")]
    CtxCommandRun,
    #[serde(rename = "plugin.command.run")]
    PluginCommandRun,
    #[serde(rename = "work.export_redact")]
    WorkExportRedact,
    #[serde(rename = "artifact.attach")]
    ArtifactAttach,
    #[serde(rename = "note.attest")]
    NoteAttest,
    #[serde(rename = "gate.update")]
    GateUpdate,
    #[serde(rename = "provider.settings.open")]
    ProviderSettingsOpen,
    #[serde(rename = "provider.session.restart")]
    ProviderSessionRestart,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginArtifactRendererContribution {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_sources: Vec<String>,
    pub artifact_types: Vec<String>,
    pub renderer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginWorkbenchCardRendererContribution {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_sources: Vec<String>,
    pub card: String,
    pub renderer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginWorkbenchSectionContribution {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_sources: Vec<String>,
    pub section: String,
    pub renderer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct PluginCompatibility {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ctx_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

impl PluginCompatibility {
    pub fn is_empty(&self) -> bool {
        self.min_ctx_version.is_none() && self.capabilities.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginEnablement {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginLoadStatus {
    NotLoaded,
    Loaded,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginDiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginDiagnostic {
    pub severity: PluginDiagnosticSeverity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginInventoryItem {
    pub id: String,
    pub name: String,
    pub version: String,
    pub enabled: PluginEnablement,
    pub status: PluginLoadStatus,
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<PluginDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_loaded_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<PluginManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginContributionRegistration<T> {
    pub plugin_id: String,
    pub plugin_name: String,
    pub plugin_version: String,
    pub plugin_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_revision: Option<String>,
    pub contribution: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PluginExtensionRegistry {
    pub revision: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<PluginContributionRegistration<PluginProviderContribution>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<PluginContributionRegistration<PluginRuntimeContribution>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<PluginContributionRegistration<PluginCommandContribution>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collectors: Vec<PluginContributionRegistration<PluginCollectorContribution>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observers: Vec<PluginContributionRegistration<PluginObserverContribution>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ui_surfaces: Vec<PluginContributionRegistration<PluginUiSurfaceContribution>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<PluginContributionRegistration<PluginWorkbenchTemplateContribution>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toolbar_actions:
        Vec<PluginContributionRegistration<PluginWorkbenchToolbarActionContribution>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_renderers: Vec<PluginContributionRegistration<PluginArtifactRendererContribution>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub card_renderers:
        Vec<PluginContributionRegistration<PluginWorkbenchCardRendererContribution>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail_sections: Vec<PluginContributionRegistration<PluginWorkbenchSectionContribution>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_sections: Vec<PluginContributionRegistration<PluginWorkbenchSectionContribution>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginManifestValidationError {
    UnsupportedSchemaVersion {
        expected: i64,
        actual: i64,
    },
    EmptyField {
        field: &'static str,
    },
    EmptyPluginDefinition,
    DuplicateId {
        field: &'static str,
        id: String,
    },
    InvalidPluginQualifiedId {
        field: &'static str,
        id: String,
        plugin_id: String,
    },
    UnknownEntrypoint {
        field: &'static str,
        id: String,
    },
    UnknownCommand {
        field: &'static str,
        id: String,
    },
}

pub fn validate_plugin_manifest(
    manifest: &PluginManifest,
) -> Result<(), PluginManifestValidationError> {
    if manifest.schema_version != PLUGIN_MANIFEST_SCHEMA_VERSION {
        return Err(PluginManifestValidationError::UnsupportedSchemaVersion {
            expected: PLUGIN_MANIFEST_SCHEMA_VERSION,
            actual: manifest.schema_version,
        });
    }

    if manifest.id.trim().is_empty() {
        return Err(PluginManifestValidationError::EmptyField { field: "id" });
    }

    if manifest.name.trim().is_empty() {
        return Err(PluginManifestValidationError::EmptyField { field: "name" });
    }

    if manifest.version.trim().is_empty() {
        return Err(PluginManifestValidationError::EmptyField { field: "version" });
    }

    if manifest.entrypoints.is_empty() && manifest.contributes.is_empty() {
        return Err(PluginManifestValidationError::EmptyPluginDefinition);
    }

    let mut entrypoint_ids = BTreeSet::new();
    for entrypoint in &manifest.entrypoints {
        if entrypoint.id.trim().is_empty() {
            return Err(PluginManifestValidationError::EmptyField {
                field: "entrypoints.id",
            });
        }
        if entrypoint.command.trim().is_empty() {
            return Err(PluginManifestValidationError::EmptyField {
                field: "entrypoints.command",
            });
        }
        if !entrypoint_ids.insert(entrypoint.id.clone()) {
            return Err(PluginManifestValidationError::DuplicateId {
                field: "entrypoints.id",
                id: entrypoint.id.clone(),
            });
        }
    }

    let mut contribution_ids = BTreeSet::new();
    validate_named_contributions(
        "contributes.providers",
        &manifest.contributes.providers,
        &entrypoint_ids,
        &mut contribution_ids,
        |contribution| {
            (
                &contribution.id,
                &contribution.name,
                contribution.entrypoint.as_deref(),
            )
        },
    )?;
    validate_named_contributions(
        "contributes.runtimes",
        &manifest.contributes.runtimes,
        &entrypoint_ids,
        &mut contribution_ids,
        |contribution| {
            (
                &contribution.id,
                &contribution.name,
                contribution.entrypoint.as_deref(),
            )
        },
    )?;
    validate_named_contributions(
        "contributes.commands",
        &manifest.contributes.commands,
        &entrypoint_ids,
        &mut contribution_ids,
        |contribution| {
            (
                &contribution.id,
                &contribution.title,
                contribution.entrypoint.as_deref(),
            )
        },
    )?;
    let command_ids: BTreeSet<String> = manifest
        .contributes
        .commands
        .iter()
        .map(|command| command.id.clone())
        .collect();
    validate_named_contributions(
        "contributes.collectors",
        &manifest.contributes.collectors,
        &entrypoint_ids,
        &mut contribution_ids,
        |contribution| {
            (
                &contribution.id,
                &contribution.name,
                contribution.entrypoint.as_deref(),
            )
        },
    )?;
    validate_named_contributions(
        "contributes.observers",
        &manifest.contributes.observers,
        &entrypoint_ids,
        &mut contribution_ids,
        |contribution| {
            (
                &contribution.id,
                &contribution.name,
                contribution.entrypoint.as_deref(),
            )
        },
    )?;
    validate_named_contributions(
        "contributes.ui_surfaces",
        &manifest.contributes.ui_surfaces,
        &entrypoint_ids,
        &mut contribution_ids,
        |contribution| {
            (
                &contribution.id,
                &contribution.name,
                contribution.entrypoint.as_deref(),
            )
        },
    )?;
    validate_declarative_workbench_contributions(
        "contributes.templates",
        &manifest.contributes.templates,
        &manifest.id,
        &mut contribution_ids,
        |contribution| {
            (
                contribution.id.as_str(),
                contribution.name.as_str(),
                vec![contribution.title.as_str(), contribution.template.as_str()],
                Vec::new(),
            )
        },
    )?;
    validate_declarative_workbench_contributions(
        "contributes.toolbar_actions",
        &manifest.contributes.toolbar_actions,
        &manifest.id,
        &mut contribution_ids,
        |contribution| {
            (
                contribution.id.as_str(),
                contribution.name.as_str(),
                vec![contribution.title.as_str()],
                Vec::new(),
            )
        },
    )?;
    for contribution in &manifest.contributes.toolbar_actions {
        if let Some(command) = contribution.command.as_deref() {
            if command.trim().is_empty() {
                return Err(PluginManifestValidationError::EmptyField {
                    field: "contributes.toolbar_actions.command",
                });
            }
            if !command_ids.contains(command) {
                return Err(PluginManifestValidationError::UnknownCommand {
                    field: "contributes.toolbar_actions",
                    id: command.to_string(),
                });
            }
        }
        if contribution.command.is_none() && contribution.action.is_none() {
            return Err(PluginManifestValidationError::EmptyField {
                field: "contributes.toolbar_actions",
            });
        }
    }
    validate_declarative_workbench_contributions(
        "contributes.artifact_renderers",
        &manifest.contributes.artifact_renderers,
        &manifest.id,
        &mut contribution_ids,
        |contribution| {
            (
                contribution.id.as_str(),
                contribution.name.as_str(),
                vec![contribution.renderer.as_str()],
                vec![contribution.artifact_types.as_slice()],
            )
        },
    )?;
    validate_declarative_workbench_contributions(
        "contributes.card_renderers",
        &manifest.contributes.card_renderers,
        &manifest.id,
        &mut contribution_ids,
        |contribution| {
            (
                contribution.id.as_str(),
                contribution.name.as_str(),
                vec![contribution.card.as_str(), contribution.renderer.as_str()],
                Vec::new(),
            )
        },
    )?;
    validate_declarative_workbench_contributions(
        "contributes.detail_sections",
        &manifest.contributes.detail_sections,
        &manifest.id,
        &mut contribution_ids,
        |contribution| {
            (
                contribution.id.as_str(),
                contribution.name.as_str(),
                vec![
                    contribution.section.as_str(),
                    contribution.renderer.as_str(),
                ],
                Vec::new(),
            )
        },
    )?;
    validate_declarative_workbench_contributions(
        "contributes.review_sections",
        &manifest.contributes.review_sections,
        &manifest.id,
        &mut contribution_ids,
        |contribution| {
            (
                contribution.id.as_str(),
                contribution.name.as_str(),
                vec![
                    contribution.section.as_str(),
                    contribution.renderer.as_str(),
                ],
                Vec::new(),
            )
        },
    )?;

    Ok(())
}

fn deserialize_non_null_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_null() {
        return Err(serde::de::Error::custom(
            "expected omitted field or non-null value",
        ));
    }
    T::deserialize(value)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

fn validate_declarative_workbench_contributions<T>(
    field: &'static str,
    contributions: &[T],
    plugin_id: &str,
    contribution_ids: &mut BTreeSet<String>,
    fields: impl Fn(&T) -> (&str, &str, Vec<&str>, Vec<&[String]>),
) -> Result<(), PluginManifestValidationError> {
    for contribution in contributions {
        let (id, name, required_strings, required_arrays) = fields(contribution);
        if id.trim().is_empty() {
            return Err(PluginManifestValidationError::EmptyField {
                field: "contributes.id",
            });
        }
        if name.trim().is_empty() {
            return Err(PluginManifestValidationError::EmptyField {
                field: "contributes.name",
            });
        }
        if !id.starts_with(&format!("{plugin_id}.")) {
            return Err(PluginManifestValidationError::InvalidPluginQualifiedId {
                field,
                id: id.to_string(),
                plugin_id: plugin_id.to_string(),
            });
        }
        if !contribution_ids.insert(id.to_string()) {
            return Err(PluginManifestValidationError::DuplicateId {
                field: "contributes.id",
                id: id.to_string(),
            });
        }
        if required_strings.iter().any(|value| value.trim().is_empty()) {
            return Err(PluginManifestValidationError::EmptyField { field });
        }
        if required_arrays
            .iter()
            .any(|values| values.is_empty() || values.iter().any(|value| value.trim().is_empty()))
        {
            return Err(PluginManifestValidationError::EmptyField { field });
        }
    }
    Ok(())
}

fn validate_named_contributions<T>(
    field: &'static str,
    contributions: &[T],
    entrypoint_ids: &BTreeSet<String>,
    contribution_ids: &mut BTreeSet<String>,
    fields: impl Fn(&T) -> (&String, &String, Option<&str>),
) -> Result<(), PluginManifestValidationError> {
    for contribution in contributions {
        let (id, name, entrypoint) = fields(contribution);
        if id.trim().is_empty() {
            return Err(PluginManifestValidationError::EmptyField {
                field: "contributes.id",
            });
        }
        if name.trim().is_empty() {
            return Err(PluginManifestValidationError::EmptyField {
                field: "contributes.name",
            });
        }
        if !contribution_ids.insert(id.clone()) {
            return Err(PluginManifestValidationError::DuplicateId {
                field: "contributes.id",
                id: id.clone(),
            });
        }
        if let Some(entrypoint) = entrypoint {
            if entrypoint.trim().is_empty() {
                return Err(PluginManifestValidationError::EmptyField {
                    field: "contributes.entrypoint",
                });
            }
            if !entrypoint_ids.contains(entrypoint) {
                return Err(PluginManifestValidationError::UnknownEntrypoint {
                    field,
                    id: entrypoint.to_string(),
                });
            }
        }
    }
    Ok(())
}

fn default_plugin_manifest_schema_version() -> i64 {
    PLUGIN_MANIFEST_SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn minimal_plugin_manifest() -> PluginManifest {
        PluginManifest {
            schema_version: PLUGIN_MANIFEST_SCHEMA_VERSION,
            id: "example.agent-tools".into(),
            name: "Example Agent Tools".into(),
            version: "0.1.0".into(),
            description: Some("Example open-source plugin manifest.".into()),
            entrypoints: vec![PluginEntrypoint {
                id: "main".into(),
                kind: PluginEntrypointKind::Process,
                command: "node".into(),
                args: vec!["dist/index.js".into()],
                cwd: None,
                environment: BTreeMap::new(),
            }],
            contributes: PluginContributions {
                providers: vec![PluginProviderContribution {
                    id: "example-provider".into(),
                    name: "Example Provider".into(),
                    description: None,
                    entrypoint: Some("main".into()),
                    capabilities: vec!["agent.runtime".into()],
                }],
                runtimes: vec![PluginRuntimeContribution {
                    id: "example-runtime".into(),
                    name: "Example Runtime".into(),
                    description: None,
                    entrypoint: Some("main".into()),
                    capabilities: vec!["workspace.exec".into()],
                }],
                commands: vec![PluginCommandContribution {
                    id: "example.agent-tools.say_hello".into(),
                    title: "Say Hello".into(),
                    description: Some("Run the example command.".into()),
                    category: Some("Example".into()),
                    entrypoint: Some("main".into()),
                }],
                collectors: vec![PluginCollectorContribution {
                    id: "example.collector".into(),
                    name: "Example Collector".into(),
                    description: None,
                    entrypoint: Some("main".into()),
                    events: vec!["workspace.changed".into()],
                }],
                observers: vec![PluginObserverContribution {
                    id: "example.observer".into(),
                    name: "Example Observer".into(),
                    description: None,
                    entrypoint: Some("main".into()),
                    events: vec!["session.completed".into()],
                }],
                ui_surfaces: vec![PluginUiSurfaceContribution {
                    id: "example.panel".into(),
                    name: "Example Panel".into(),
                    surface: PluginUiSurfaceKind::Panel,
                    description: None,
                    entrypoint: Some("main".into()),
                    contexts: vec!["workspace".into()],
                }],
                templates: vec![PluginWorkbenchTemplateContribution {
                    id: "example.agent-tools.template".into(),
                    name: "Example Template".into(),
                    description: None,
                    contexts: vec!["workspace".into()],
                    data_sources: vec!["current Work summary".into()],
                    title: "Example Template".into(),
                    template: "host.example-template".into(),
                }],
                toolbar_actions: vec![PluginWorkbenchToolbarActionContribution {
                    id: "example.agent-tools.toolbar".into(),
                    name: "Example Toolbar Action".into(),
                    description: None,
                    contexts: vec!["workspace".into()],
                    data_sources: Vec::new(),
                    title: "Say Hello".into(),
                    command: Some("example.agent-tools.say_hello".into()),
                    action: None,
                    icon: Some("message-circle".into()),
                }],
                artifact_renderers: vec![PluginArtifactRendererContribution {
                    id: "example.agent-tools.artifact-renderer".into(),
                    name: "Example Artifact Renderer".into(),
                    description: None,
                    contexts: Vec::new(),
                    data_sources: Vec::new(),
                    artifact_types: vec!["text/plain".into()],
                    renderer: "host.text-artifact".into(),
                }],
                card_renderers: vec![PluginWorkbenchCardRendererContribution {
                    id: "example.agent-tools.card-renderer".into(),
                    name: "Example Card Renderer".into(),
                    description: None,
                    contexts: Vec::new(),
                    data_sources: Vec::new(),
                    card: "work.summary".into(),
                    renderer: "host.work-summary-card".into(),
                }],
                detail_sections: vec![PluginWorkbenchSectionContribution {
                    id: "example.agent-tools.detail-section".into(),
                    name: "Example Detail Section".into(),
                    description: None,
                    contexts: Vec::new(),
                    data_sources: Vec::new(),
                    section: "work-summary".into(),
                    renderer: "host.work-summary-section".into(),
                }],
                review_sections: vec![PluginWorkbenchSectionContribution {
                    id: "example.agent-tools.review-section".into(),
                    name: "Example Review Section".into(),
                    description: None,
                    contexts: Vec::new(),
                    data_sources: Vec::new(),
                    section: "gate-state".into(),
                    renderer: "host.gate-state-section".into(),
                }],
            },
            compatibility: PluginCompatibility {
                min_ctx_version: Some("0.1.0".into()),
                capabilities: vec!["plugins.manifest.v1".into()],
            },
        }
    }

    #[test]
    fn plugin_manifest_round_trips_public_shape() {
        let manifest = minimal_plugin_manifest();

        let value = serde_json::to_value(&manifest).unwrap();

        assert_eq!(value.get("schema_version"), Some(&json!(1)));
        assert_eq!(value.get("id"), Some(&json!("example.agent-tools")));
        assert_eq!(
            value.pointer("/entrypoints/0/kind"),
            Some(&json!("process"))
        );
        assert_eq!(
            value.pointer("/contributes/providers/0/capabilities/0"),
            Some(&json!("agent.runtime"))
        );
        assert_eq!(
            value.pointer("/contributes/ui_surfaces/0/surface"),
            Some(&json!("panel"))
        );
        assert_eq!(
            value.pointer("/contributes/templates/0/template"),
            Some(&json!("host.example-template"))
        );
        assert_eq!(
            value.pointer("/contributes/toolbar_actions/0/command"),
            Some(&json!("example.agent-tools.say_hello"))
        );
        assert_eq!(
            value.pointer("/contributes/artifact_renderers/0/artifact_types/0"),
            Some(&json!("text/plain"))
        );
        assert_eq!(
            value.pointer("/contributes/card_renderers/0/card"),
            Some(&json!("work.summary"))
        );
        assert_eq!(
            value.pointer("/contributes/detail_sections/0/section"),
            Some(&json!("work-summary"))
        );
        assert_eq!(
            value.pointer("/contributes/review_sections/0/renderer"),
            Some(&json!("host.gate-state-section"))
        );
        assert_eq!(
            value.pointer("/compatibility/min_ctx_version"),
            Some(&json!("0.1.0"))
        );

        let round_trip: PluginManifest = serde_json::from_value(value).unwrap();
        assert_eq!(round_trip, manifest);
        assert_eq!(round_trip.validate(), Ok(()));
    }

    #[test]
    fn plugin_manifest_rejects_null_toolbar_targets() {
        let null_command = serde_json::from_value::<PluginManifest>(json!({
            "id": "example.agent-tools",
            "name": "Example Agent Tools",
            "version": "0.1.0",
            "contributes": {
                "toolbar_actions": [
                    {
                        "id": "example.agent-tools.toolbar",
                        "name": "Toolbar",
                        "title": "Focus",
                        "command": null,
                        "action": "work.focus"
                    }
                ]
            }
        }))
        .unwrap_err()
        .to_string();
        assert!(null_command.contains("expected omitted field or non-null value"));

        let null_action = serde_json::from_value::<PluginManifest>(json!({
            "id": "example.agent-tools",
            "name": "Example Agent Tools",
            "version": "0.1.0",
            "contributes": {
                "toolbar_actions": [
                    {
                        "id": "example.agent-tools.toolbar",
                        "name": "Toolbar",
                        "title": "Focus",
                        "command": "example.agent-tools.focus",
                        "action": null
                    }
                ]
            }
        }))
        .unwrap_err()
        .to_string();
        assert!(null_action.contains("expected omitted field or non-null value"));
    }

    #[test]
    fn plugin_manifest_rejects_unknown_manifest_fields() {
        let top_level = serde_json::from_value::<PluginManifest>(json!({
            "id": "example.agent-tools",
            "name": "Example Agent Tools",
            "version": "0.1.0",
            "unexpected": true
        }))
        .unwrap_err()
        .to_string();
        assert!(top_level.contains("unknown field"));

        let processor_bucket = serde_json::from_value::<PluginManifest>(json!({
            "id": "example.agent-tools",
            "name": "Example Agent Tools",
            "version": "0.1.0",
            "contributes": {
                "redaction_processors": []
            }
        }))
        .unwrap_err()
        .to_string();
        assert!(processor_bucket.contains("unknown field"));

        let runtime_shaped_declarative = serde_json::from_value::<PluginManifest>(json!({
            "id": "example.agent-tools",
            "name": "Example Agent Tools",
            "version": "0.1.0",
            "contributes": {
                "templates": [
                    {
                        "id": "example.agent-tools.template",
                        "name": "Template",
                        "title": "Template",
                        "template": "host.template",
                        "entrypoint": "main"
                    }
                ]
            }
        }))
        .unwrap_err()
        .to_string();
        assert!(runtime_shaped_declarative.contains("unknown field"));
    }

    #[test]
    fn plugin_manifest_validation_rejects_unknown_toolbar_command() {
        let mut manifest = minimal_plugin_manifest();
        manifest.contributes.toolbar_actions[0].command =
            Some("example.agent-tools.missing".into());

        assert_eq!(
            manifest.validate(),
            Err(PluginManifestValidationError::UnknownCommand {
                field: "contributes.toolbar_actions",
                id: "example.agent-tools.missing".into(),
            })
        );
    }

    #[test]
    fn plugin_manifest_validation_rejects_invalid_schema_version() {
        let mut manifest = minimal_plugin_manifest();
        manifest.schema_version = 2;

        assert_eq!(
            manifest.validate(),
            Err(PluginManifestValidationError::UnsupportedSchemaVersion {
                expected: PLUGIN_MANIFEST_SCHEMA_VERSION,
                actual: 2,
            })
        );
    }

    #[test]
    fn plugin_manifest_validation_rejects_empty_id_and_name() {
        let mut manifest = minimal_plugin_manifest();
        manifest.id = "   ".into();

        assert_eq!(
            manifest.validate(),
            Err(PluginManifestValidationError::EmptyField { field: "id" })
        );

        manifest.id = "example.agent-tools".into();
        manifest.name.clear();

        assert_eq!(
            manifest.validate(),
            Err(PluginManifestValidationError::EmptyField { field: "name" })
        );
    }

    #[test]
    fn plugin_manifest_validation_rejects_empty_version_entrypoint_command_and_title() {
        let mut manifest = minimal_plugin_manifest();
        manifest.version = "  ".into();

        assert_eq!(
            manifest.validate(),
            Err(PluginManifestValidationError::EmptyField { field: "version" })
        );

        manifest.version = "0.1.0".into();
        manifest.entrypoints[0].command = " ".into();

        assert_eq!(
            manifest.validate(),
            Err(PluginManifestValidationError::EmptyField {
                field: "entrypoints.command",
            })
        );

        manifest.entrypoints[0].command = "node".into();
        manifest.contributes.commands[0].title = " ".into();

        assert_eq!(
            manifest.validate(),
            Err(PluginManifestValidationError::EmptyField {
                field: "contributes.name",
            })
        );
    }

    #[test]
    fn plugin_manifest_validation_rejects_duplicate_ids_and_unknown_entrypoints() {
        let mut manifest = minimal_plugin_manifest();
        manifest.entrypoints.push(PluginEntrypoint {
            id: "main".into(),
            kind: PluginEntrypointKind::Process,
            command: "node".into(),
            args: Vec::new(),
            cwd: None,
            environment: BTreeMap::new(),
        });

        assert_eq!(
            manifest.validate(),
            Err(PluginManifestValidationError::DuplicateId {
                field: "entrypoints.id",
                id: "main".into(),
            })
        );

        let mut manifest = minimal_plugin_manifest();
        manifest.contributes.commands[0].entrypoint = Some("missing".into());

        assert_eq!(
            manifest.validate(),
            Err(PluginManifestValidationError::UnknownEntrypoint {
                field: "contributes.commands",
                id: "missing".into(),
            })
        );

        let mut manifest = minimal_plugin_manifest();
        manifest.contributes.commands[0].id = "example-provider".into();

        assert_eq!(
            manifest.validate(),
            Err(PluginManifestValidationError::DuplicateId {
                field: "contributes.id",
                id: "example-provider".into(),
            })
        );
    }

    #[test]
    fn plugin_manifest_validation_rejects_empty_plugin_definition() {
        let mut manifest = minimal_plugin_manifest();
        manifest.entrypoints.clear();
        manifest.contributes = PluginContributions::default();

        assert_eq!(
            manifest.validate(),
            Err(PluginManifestValidationError::EmptyPluginDefinition)
        );
    }

    #[test]
    fn plugin_inventory_item_round_trips_status_and_diagnostics() {
        let item = PluginInventoryItem {
            id: "example.agent-tools".into(),
            name: "Example Agent Tools".into(),
            version: "0.1.0".into(),
            enabled: PluginEnablement::Enabled,
            status: PluginLoadStatus::Error,
            path: "/plugins/example".into(),
            diagnostics: vec![PluginDiagnostic {
                severity: PluginDiagnosticSeverity::Error,
                message: "Manifest entrypoint is missing.".into(),
                code: Some("entrypoint_missing".into()),
            }],
            last_loaded_at: Some("2026-06-17T12:00:00Z".parse().unwrap()),
            revision: Some("abc123".into()),
            manifest: Some(minimal_plugin_manifest()),
        };

        let value = serde_json::to_value(&item).unwrap();

        assert_eq!(value.get("enabled"), Some(&json!("enabled")));
        assert_eq!(value.get("status"), Some(&json!("error")));
        assert_eq!(
            value.pointer("/diagnostics/0/severity"),
            Some(&json!("error"))
        );
        assert_eq!(value.get("revision"), Some(&json!("abc123")));

        let round_trip: PluginInventoryItem = serde_json::from_value(value).unwrap();
        assert_eq!(round_trip, item);
    }

    #[test]
    fn plugin_extension_registry_round_trips_registered_contributions() {
        let manifest = minimal_plugin_manifest();
        let registry = PluginExtensionRegistry {
            revision: 7,
            providers: vec![PluginContributionRegistration {
                plugin_id: "example.agent-tools".into(),
                plugin_name: "Example Agent Tools".into(),
                plugin_version: "0.1.0".into(),
                plugin_path: "/plugins/example/ctx-plugin.json".into(),
                plugin_revision: Some("abc123".into()),
                contribution: manifest.contributes.providers[0].clone(),
            }],
            commands: vec![PluginContributionRegistration {
                plugin_id: "example.agent-tools".into(),
                plugin_name: "Example Agent Tools".into(),
                plugin_version: "0.1.0".into(),
                plugin_path: "/plugins/example/ctx-plugin.json".into(),
                plugin_revision: Some("abc123".into()),
                contribution: manifest.contributes.commands[0].clone(),
            }],
            templates: vec![PluginContributionRegistration {
                plugin_id: "example.agent-tools".into(),
                plugin_name: "Example Agent Tools".into(),
                plugin_version: "0.1.0".into(),
                plugin_path: "/plugins/example/ctx-plugin.json".into(),
                plugin_revision: Some("abc123".into()),
                contribution: manifest.contributes.templates[0].clone(),
            }],
            toolbar_actions: vec![PluginContributionRegistration {
                plugin_id: "example.agent-tools".into(),
                plugin_name: "Example Agent Tools".into(),
                plugin_version: "0.1.0".into(),
                plugin_path: "/plugins/example/ctx-plugin.json".into(),
                plugin_revision: Some("abc123".into()),
                contribution: manifest.contributes.toolbar_actions[0].clone(),
            }],
            artifact_renderers: vec![PluginContributionRegistration {
                plugin_id: "example.agent-tools".into(),
                plugin_name: "Example Agent Tools".into(),
                plugin_version: "0.1.0".into(),
                plugin_path: "/plugins/example/ctx-plugin.json".into(),
                plugin_revision: Some("abc123".into()),
                contribution: manifest.contributes.artifact_renderers[0].clone(),
            }],
            card_renderers: vec![PluginContributionRegistration {
                plugin_id: "example.agent-tools".into(),
                plugin_name: "Example Agent Tools".into(),
                plugin_version: "0.1.0".into(),
                plugin_path: "/plugins/example/ctx-plugin.json".into(),
                plugin_revision: Some("abc123".into()),
                contribution: manifest.contributes.card_renderers[0].clone(),
            }],
            detail_sections: vec![PluginContributionRegistration {
                plugin_id: "example.agent-tools".into(),
                plugin_name: "Example Agent Tools".into(),
                plugin_version: "0.1.0".into(),
                plugin_path: "/plugins/example/ctx-plugin.json".into(),
                plugin_revision: Some("abc123".into()),
                contribution: manifest.contributes.detail_sections[0].clone(),
            }],
            review_sections: vec![PluginContributionRegistration {
                plugin_id: "example.agent-tools".into(),
                plugin_name: "Example Agent Tools".into(),
                plugin_version: "0.1.0".into(),
                plugin_path: "/plugins/example/ctx-plugin.json".into(),
                plugin_revision: Some("abc123".into()),
                contribution: manifest.contributes.review_sections[0].clone(),
            }],
            ..PluginExtensionRegistry::default()
        };

        let value = serde_json::to_value(&registry).unwrap();

        assert_eq!(value.get("revision"), Some(&json!(7)));
        assert_eq!(
            value.pointer("/providers/0/plugin_id"),
            Some(&json!("example.agent-tools"))
        );
        assert_eq!(
            value.pointer("/commands/0/contribution/title"),
            Some(&json!("Say Hello"))
        );
        assert_eq!(
            value.pointer("/templates/0/contribution/template"),
            Some(&json!("host.example-template"))
        );
        assert_eq!(
            value.pointer("/toolbar_actions/0/contribution/action"),
            None
        );
        assert_eq!(
            value.pointer("/artifact_renderers/0/contribution/renderer"),
            Some(&json!("host.text-artifact"))
        );
        assert_eq!(
            value.pointer("/card_renderers/0/contribution/card"),
            Some(&json!("work.summary"))
        );
        assert_eq!(
            value.pointer("/detail_sections/0/contribution/section"),
            Some(&json!("work-summary"))
        );
        assert_eq!(
            value.pointer("/review_sections/0/contribution/renderer"),
            Some(&json!("host.gate-state-section"))
        );

        let round_trip: PluginExtensionRegistry = serde_json::from_value(value).unwrap();
        assert_eq!(round_trip, registry);
    }

    #[test]
    fn plugin_manifest_schema_version_defaults_to_public_v1() {
        let manifest: PluginManifest = serde_json::from_value(json!({
            "id": "example.agent-tools",
            "name": "Example Agent Tools",
            "version": "0.1.0",
            "entrypoints": [
                {
                    "id": "main",
                    "command": "node"
                }
            ]
        }))
        .unwrap();

        assert_eq!(manifest.schema_version, PLUGIN_MANIFEST_SCHEMA_VERSION);
        assert_eq!(manifest.entrypoints[0].kind, PluginEntrypointKind::Process);
        assert_eq!(manifest.validate(), Ok(()));
    }
}
