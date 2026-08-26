use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::json;

use ctx_history_capture::{DiscoveryIssue, DiscoveryIssueKind, ProviderSourceStatus};
use ctx_history_core::CaptureProvider;

use crate::provider_sources::{
    configured_root_conflict_details, configured_root_for_issue, configured_root_for_source,
    sources_json_with_selection, ConfiguredRootConflictKind,
};
use crate::{
    discovery_report_issues_json_with_provider_roots, history_source_plugin_report,
    manual_path_guidance, plugin_manifest_failures_json, plugin_sources_json, provider_cli_name,
    CliSourceDiscoveryPort, HistorySourcePluginManifestFailure, HistorySourcePluginSource,
    OutputFormat, SourceInfo, SourcesRequest, DEFAULT_VISIBLE_SOURCE_PROVIDERS,
};
use ctx_terminal::{
    canonical_human_output_bytes, diagnostic, empty_state, hint, outcome, section, table, Action,
    Diagnostic, DiagnosticLevel, Document, EmptyState, Field, Hint, Outcome, OutcomeState,
    RenderContext, Table, Ui,
};

/// Complete sources-command facts for the final host to map into its owned
/// telemetry and local-usage delivery actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcesDiscoveryObservation {
    pub providers_detected: u64,
    pub providers_existing: u64,
    pub providers_importable: u64,
}

/// Complete successful sources-command facts for the final host to map into
/// its owned local-usage delivery action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcesExecutionObservation {
    pub result_count: usize,
    pub content_bytes: usize,
    pub output_bytes: usize,
}

/// Runs the transport-neutral sources application adapter from one request
/// snapshot. The final host resolves `home` once and owns all delivery actions.
pub fn run_sources<F>(
    request: SourcesRequest,
    data_root: &Path,
    home: Option<PathBuf>,
    automatic_provider_discovery: bool,
    provider_roots: Vec<ctx_history_capture::ProviderRootDefinition>,
    on_discovery: F,
    ui: &mut Ui,
) -> Result<SourcesExecutionObservation>
where
    F: FnOnce(SourcesDiscoveryObservation),
{
    let provider_filter = request.provider.map(|provider| provider.capture_provider());
    let discovery = CliSourceDiscoveryPort::new(home.clone(), data_root.to_path_buf())
        .with_automatic_provider_discovery(automatic_provider_discovery)
        .with_provider_roots(provider_roots.clone());
    let show_all_sources = request.all || request.show_missing || provider_filter.is_some();
    let listing = ctx_history_ingest_application::assemble_source_listing(
        &discovery,
        data_root,
        ctx_history_ingest_application::SourceListingRequest {
            provider_filter,
            show_all: show_all_sources,
            configured_provider_roots: provider_roots.clone(),
            default_visible_missing_providers: DEFAULT_VISIBLE_SOURCE_PROVIDERS.to_vec(),
        },
    )?;
    let discovery_report = listing.discovery;
    let sources = listing.visible_sources;
    let output_sources = sources
        .iter()
        .filter(|source| source_is_visible_for_output(source, show_all_sources, request.format))
        .cloned()
        .collect::<Vec<_>>();
    let plugin_sources = listing.plugins.sources;
    let plugin_failures = listing.plugins.failures;
    on_discovery(sources_discovery_observation(
        &sources,
        &plugin_sources,
        &plugin_failures,
    ));
    let hidden_missing_sources = listing.hidden_missing_sources;
    let mut canonical_entries = sources_json_with_selection(&output_sources, &provider_roots);
    canonical_entries.extend(plugin_sources_json(&plugin_sources));
    canonical_entries.extend(plugin_manifest_failures_json(&plugin_failures));
    let result_count = canonical_entries.len();
    let content_bytes = serde_json::to_vec(&canonical_entries)?.len();
    let output_bytes = if request.format == OutputFormat::Json {
        let (issues, issues_truncated) = discovery_report_issues_json_with_provider_roots(
            &discovery_report,
            &provider_roots,
            automatic_provider_discovery,
        );
        let value = json!({
            "schema_version": 1,
            "scope": if show_all_sources { "all" } else { "default" },
            "automatic_discovery": automatic_provider_discovery,
            "hidden_missing_sources": hidden_missing_sources,
            "sources": canonical_entries,
            "issues": issues,
            "issues_truncated": issues_truncated,
        });
        let output_bytes = serde_json::to_string_pretty(&value)?
            .len()
            .saturating_add(1);
        ctx_terminal::print_json(value)?;
        output_bytes
    } else {
        let render_input = SourcesHumanRenderInput {
            sources: &output_sources,
            issues: &discovery_report.issues,
            plugin_sources: &plugin_sources,
            plugin_failures: &plugin_failures,
            hidden_missing_sources,
            home: home.as_deref(),
            automatic_provider_discovery,
            provider_roots: &provider_roots,
        };
        let document = render_sources_human(ui.stdout_context(), render_input);
        let output_bytes =
            canonical_human_output_bytes(|context| render_sources_human(context, render_input));
        ui.write_stdout(&document)?;
        output_bytes
    };
    Ok(SourcesExecutionObservation {
        result_count,
        content_bytes,
        output_bytes,
    })
}

fn sources_discovery_observation(
    sources: &[SourceInfo],
    plugin_sources: &[HistorySourcePluginSource],
    plugin_failures: &[HistorySourcePluginManifestFailure],
) -> SourcesDiscoveryObservation {
    let existing = sources.iter().filter(|source| source.exists).count();
    let existing_plugin_sources = plugin_sources
        .iter()
        .filter(|source| history_source_plugin_report(source).is_importable())
        .count();
    let importable = sources
        .iter()
        .filter(|source| {
            source.exists
                && source.import_support.is_importable()
                && source.status == ProviderSourceStatus::Available
        })
        .count();
    SourcesDiscoveryObservation {
        providers_detected: sources
            .len()
            .saturating_add(plugin_sources.len())
            .saturating_add(plugin_failures.len()) as u64,
        providers_existing: existing.saturating_add(existing_plugin_sources) as u64,
        providers_importable: importable.saturating_add(existing_plugin_sources) as u64,
    }
}

fn source_is_visible_for_output(
    source: &SourceInfo,
    show_all_sources: bool,
    format: OutputFormat,
) -> bool {
    format == OutputFormat::Json
        || show_all_sources
        || source.status != ProviderSourceStatus::Empty
        || source.route_provenance.configured_root().is_some()
}

#[cfg(test)]
use ctx_history_ingest_application::{merge_sources, source_is_visible};

#[derive(Clone, Copy)]
struct SourcesHumanRenderInput<'a> {
    sources: &'a [SourceInfo],
    issues: &'a [DiscoveryIssue],
    plugin_sources: &'a [HistorySourcePluginSource],
    plugin_failures: &'a [HistorySourcePluginManifestFailure],
    hidden_missing_sources: usize,
    home: Option<&'a Path>,
    automatic_provider_discovery: bool,
    provider_roots: &'a [ctx_history_capture::ProviderRootDefinition],
}

#[cfg(test)]
impl<'a> SourcesHumanRenderInput<'a> {
    fn from_sources(sources: &'a [SourceInfo]) -> Self {
        Self {
            sources,
            issues: &[],
            plugin_sources: &[],
            plugin_failures: &[],
            hidden_missing_sources: 0,
            home: None,
            automatic_provider_discovery: true,
            provider_roots: &[],
        }
    }

    fn with_issues(mut self, issues: &'a [DiscoveryIssue]) -> Self {
        self.issues = issues;
        self
    }

    fn with_hidden_missing_sources(mut self, hidden_missing_sources: usize) -> Self {
        self.hidden_missing_sources = hidden_missing_sources;
        self
    }

    fn with_home(mut self, home: Option<&'a Path>) -> Self {
        self.home = home;
        self
    }

    fn with_automatic_provider_discovery(mut self, enabled: bool) -> Self {
        self.automatic_provider_discovery = enabled;
        self
    }

    fn with_provider_roots(
        mut self,
        provider_roots: &'a [ctx_history_capture::ProviderRootDefinition],
    ) -> Self {
        self.provider_roots = provider_roots;
        self
    }
}

fn render_sources_human(context: &RenderContext, input: SourcesHumanRenderInput<'_>) -> Document {
    let SourcesHumanRenderInput {
        sources,
        issues,
        plugin_sources,
        plugin_failures,
        hidden_missing_sources,
        home,
        automatic_provider_discovery,
        provider_roots,
    } = input;
    if sources.is_empty()
        && issues.is_empty()
        && plugin_sources.is_empty()
        && plugin_failures.is_empty()
    {
        return empty_state(
            context,
            EmptyState {
                title: "No history sources found",
                detail: if automatic_provider_discovery {
                    "Select a provider or inspect every known provider location."
                } else {
                    "Automatic discovery is disabled and no named roots are available."
                },
                action: Some(Action {
                    command: "ctx sources --all",
                }),
            },
        );
    }

    let importable = sources
        .iter()
        .filter(|source| {
            source.status == ProviderSourceStatus::Available
                && source.import_support.is_importable()
        })
        .count()
        .saturating_add(
            plugin_sources
                .iter()
                .filter(|source| history_source_plugin_report(source).is_importable())
                .count(),
        );
    let title = match importable {
        0 => "No importable history sources found".to_owned(),
        1 => "1 history source is ready".to_owned(),
        count => format!("{count} history sources are ready"),
    };
    let attention = sources
        .iter()
        .filter(|source| source.status == ProviderSourceStatus::Unsupported)
        .count()
        .saturating_add(
            plugin_sources
                .iter()
                .filter(|source| !history_source_plugin_report(source).is_importable())
                .count(),
        )
        .saturating_add(issues.len())
        .saturating_add(plugin_failures.len());
    let detail = (attention > 0).then(|| match attention {
        1 => "1 source needs attention.".to_owned(),
        count => format!("{count} sources need attention."),
    });
    let mut document = outcome(
        context,
        Outcome {
            state: if importable > 0 && attention == 0 {
                OutcomeState::Success
            } else {
                OutcomeState::Warning
            },
            title: &title,
            detail: detail.as_deref(),
        },
    );

    if !sources.is_empty() || !plugin_sources.is_empty() {
        let mut locations = Table::new(["Source", "Status", "Location", "Selection", "Format"])
            .keep_columns_intact([0, 1, 2, 3, 4]);
        for source in sources {
            let selection = configured_root_for_source(provider_roots, source).map_or_else(
                || "automatic".to_owned(),
                |root| match root.group.as_deref() {
                    Some(group) => format!("{} ({group})", root.id),
                    None => root.id.clone(),
                },
            );
            locations.push_row([
                source_provider_cli_name(source.provider).to_owned(),
                source.status.as_str().to_owned(),
                human_path(&source.path, home),
                selection,
                human_source_format(source.source_format),
            ]);
        }
        for source in plugin_sources {
            let report = history_source_plugin_report(source);
            locations.push_row([
                format!("custom/{}", source.label()),
                report.status.as_str().to_owned(),
                report.durable_path.map_or_else(
                    || "no durable provider path".to_owned(),
                    |path| human_path(path, home),
                ),
                "plugin".to_owned(),
                human_source_format(&source.source_format),
            ]);
        }
        document.push_blank();
        document.append(section("Locations", table(context, &locations)));
    }

    for source in sources
        .iter()
        .filter(|source| source.status == ProviderSourceStatus::Unsupported)
    {
        let provider = source_provider_cli_name(source.provider);
        let summary = format!("{provider} history cannot be imported automatically");
        let location = human_path(&source.path, home);
        let reason = source
            .unsupported_reason
            .unwrap_or("this source format is unsupported");
        let command = manual_path_guidance(source.provider);
        document.push_blank();
        document.append(diagnostic(
            context,
            Diagnostic {
                level: DiagnosticLevel::Warning,
                summary: &summary,
                detail: Some("Choose a supported disk-backed history location."),
                fields: &[
                    Field::new("Location", &location),
                    Field::new("Reason", reason),
                ],
                action: Some(Action { command: &command }),
            },
        ));
    }
    for source in plugin_sources {
        let report = history_source_plugin_report(source);
        if report.is_importable() {
            continue;
        }
        let summary = format!("custom/{} history cannot be imported", source.label());
        let manifest = human_path(&source.manifest_path, home);
        let location = report
            .durable_path
            .map_or_else(|| "not declared".to_owned(), |path| human_path(path, home));
        let reason = report
            .unsupported_reason
            .unwrap_or("this history source plugin is unsupported");
        document.push_blank();
        document.append(diagnostic(
            context,
            Diagnostic {
                level: DiagnosticLevel::Warning,
                summary: &summary,
                detail: Some("Declare a regular provider-owned ctx-history-jsonl-v2 path."),
                fields: &[
                    Field::new("Manifest", &manifest),
                    Field::new("Location", &location),
                    Field::new("Reason", reason),
                ],
                action: None,
            },
        ));
    }
    for issue in issues {
        document.push_blank();
        document.append(render_discovery_issue(
            context,
            issue,
            provider_roots,
            automatic_provider_discovery,
        ));
    }
    for failure in plugin_failures {
        let manifest = human_path(&failure.manifest_path, home);
        document.push_blank();
        document.append(diagnostic(
            context,
            Diagnostic {
                level: DiagnosticLevel::Warning,
                summary: "A custom history source manifest is invalid",
                detail: None,
                fields: &[
                    Field::new("Manifest", &manifest),
                    Field::new("Error", &failure.error),
                ],
                action: None,
            },
        ));
    }
    if hidden_missing_sources > 0 {
        let text = match hidden_missing_sources {
            1 => "1 missing provider location is hidden.".to_owned(),
            count => format!("{count} missing provider locations are hidden."),
        };
        document.push_blank();
        document.append(hint(
            context,
            Hint { text: &text },
            Some(Action {
                command: "ctx sources --all",
            }),
        ));
    }
    if !automatic_provider_discovery {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Automatic discovery is disabled; only named roots are active.",
            },
            None,
        ));
    }
    document
}

fn human_path(path: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return path.display().to_string();
    };
    let Ok(relative) = path.strip_prefix(home) else {
        return path.display().to_string();
    };
    if relative.as_os_str().is_empty() {
        "~".to_owned()
    } else {
        Path::new("~").join(relative).display().to_string()
    }
}

fn human_source_format(format: &str) -> String {
    if format == "ctx_history_jsonl_v2" {
        "ctx history".to_owned()
    } else if format.contains("sqlite") || format.contains("database") {
        "Session database".to_owned()
    } else if format.contains("transcript") || format.contains("trajectory") {
        "Agent transcripts".to_owned()
    } else if format.contains("history") && !format.contains("session") {
        "Prompt history".to_owned()
    } else if format.contains("event") {
        "Session events".to_owned()
    } else if format.contains("session") || format.contains("project") {
        "Session history".to_owned()
    } else {
        format.replace(['_', '-'], " ")
    }
}

fn render_discovery_issue(
    context: &RenderContext,
    issue: &DiscoveryIssue,
    provider_roots: &[ctx_history_capture::ProviderRootDefinition],
    automatic_provider_discovery: bool,
) -> Document {
    let provider = source_provider_cli_name(issue.provider);
    if issue.kind == DiscoveryIssueKind::ConfiguredRootConflict {
        let details =
            configured_root_conflict_details(issue, provider_roots, automatic_provider_discovery);
        let summary = match details.kind {
            Some(ConfiguredRootConflictKind::ConfiguredConfigured) => {
                format!("{provider} configured roots conflict")
            }
            Some(ConfiguredRootConflictKind::AutomaticConfigured) => {
                format!("{provider} automatic and configured roots conflict")
            }
            None => format!("{provider} configured roots conflict"),
        };
        let root_descriptions = details
            .roots
            .iter()
            .map(|root| format!("{} ({})", root.id, human_path(&root.path, None)))
            .collect::<Vec<_>>()
            .join(", ");
        let reported_path = issue.path.as_deref().map(|path| human_path(path, None));
        let conflict = details.kind.map(|kind| match kind {
            ConfiguredRootConflictKind::ConfiguredConfigured => "configured/configured",
            ConfiguredRootConflictKind::AutomaticConfigured => "automatic/configured",
        });
        let mut fields = Vec::new();
        if let Some(conflict) = conflict {
            fields.push(Field::new("Conflict", conflict));
        }
        if !root_descriptions.is_empty() {
            fields.push(Field::new("Configured roots", &root_descriptions));
        }
        if let Some(reported_path) = reported_path.as_deref() {
            fields.push(Field::new("Reported path", reported_path));
        }
        fields.push(Field::new("Reason", issue.reason));

        let repair_root = details.roots.last();
        let detail = match (details.kind, repair_root) {
            (Some(ConfiguredRootConflictKind::ConfiguredConfigured), Some(root)) => format!(
                "Remove one named root, or move `{}` persistently with `ctx sources add {} --provider {} --root <different-path> --replace`.",
                root.id,
                root.id,
                provider,
            ),
            (Some(ConfiguredRootConflictKind::AutomaticConfigured), Some(root)) => format!(
                "Remove or move `{}`; if named roots should replace automatic discovery, set `[sources] automatic=false`.",
                root.id,
            ),
            _ => format!(
                "Repair the persisted roots with `ctx sources remove <name>` or `ctx sources add <name> --provider {provider} --root <different-path> --replace`; use `[sources] automatic=false` when automatic discovery should be disabled."
            ),
        };
        let action_command = repair_root.map(|root| format!("ctx sources remove {}", root.id));
        return diagnostic(
            context,
            Diagnostic {
                level: DiagnosticLevel::Warning,
                summary: &summary,
                detail: Some(&detail),
                fields: &fields,
                action: action_command.as_deref().map(|command| Action { command }),
            },
        );
    }
    if issue.kind == DiscoveryIssueKind::ConfiguredRootMissing {
        let root = configured_root_for_issue(issue, provider_roots);
        let root_name = root.map_or("configured root", |root| root.id.as_str());
        let selection = root.map(|root| match root.group.as_deref() {
            Some(group) => format!("{} ({group})", root.id),
            None => root.id.clone(),
        });
        let location = issue.path.as_deref().map(|path| human_path(path, None));
        let mut fields = Vec::new();
        if let Some(selection) = selection.as_deref() {
            fields.push(Field::new("Selection", selection));
        }
        if let Some(location) = location.as_deref() {
            fields.push(Field::new("Location", location));
        }
        fields.push(Field::new("Reason", issue.reason));
        let detail = format!(
            "The named root remains configured, but its provider-owned state is absent. Restore it, replace its persisted path with `ctx sources add <name> --provider {provider} --root <replacement-path> --replace`, or remove `{root_name}` when it is no longer needed."
        );
        let action_command = root.map(|root| format!("ctx sources remove {}", root.id));
        return diagnostic(
            context,
            Diagnostic {
                level: DiagnosticLevel::Warning,
                summary: &format!("{provider} configured history root is missing"),
                detail: Some(&detail),
                fields: &fields,
                action: action_command.as_deref().map(|command| Action { command }),
            },
        );
    }
    let (summary, detail) = match issue.kind {
        DiscoveryIssueKind::NoDiskHistory => (
            format!("{provider} has no disk history selected"),
            issue.reason,
        ),
        DiscoveryIssueKind::SelectorUnreconstructible => (
            format!("{provider} history location could not be selected safely"),
            issue.reason,
        ),
        DiscoveryIssueKind::InsufficientOfficialEvidence => (
            format!("{provider} has no established automatic history location"),
            issue.reason,
        ),
        DiscoveryIssueKind::ConfiguredRootConflict => unreachable!(),
        DiscoveryIssueKind::ConfiguredRootMissing => unreachable!(),
    };
    let command = manual_path_guidance(issue.provider);
    diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Warning,
            summary: &summary,
            detail: Some(detail),
            fields: &[],
            action: Some(Action { command: &command }),
        },
    )
}

pub(crate) fn source_provider_cli_name(provider: CaptureProvider) -> &'static str {
    provider_cli_name(provider)
}

#[cfg(test)]
#[path = "sources_ui_tests.rs"]
mod ui_tests;
