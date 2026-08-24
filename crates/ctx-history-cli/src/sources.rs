use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::json;

use ctx_history_capture::{DiscoveryIssue, DiscoveryIssueKind, ProviderSourceStatus};
use ctx_history_core::CaptureProvider;

use crate::provider_sources::{configured_root_for_source, sources_json_with_selection};
use crate::{
    discovery_report_issues_json, history_source_plugin_report, manual_path_guidance,
    plugin_manifest_failures_json, plugin_sources_json, provider_cli_name, CliSourceDiscoveryPort,
    HistorySourcePluginManifestFailure, HistorySourcePluginSource, OutputFormat, SourceInfo,
    SourcesRequest, DEFAULT_VISIBLE_SOURCE_PROVIDERS,
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
            default_visible_missing_providers: DEFAULT_VISIBLE_SOURCE_PROVIDERS.to_vec(),
        },
    )?;
    let discovery_report = listing.discovery;
    let sources = &listing.visible_sources;
    let plugin_sources = listing.plugins.sources;
    let plugin_failures = listing.plugins.failures;
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
    on_discovery(SourcesDiscoveryObservation {
        providers_detected: sources
            .len()
            .saturating_add(plugin_sources.len())
            .saturating_add(plugin_failures.len()) as u64,
        providers_existing: existing.saturating_add(existing_plugin_sources) as u64,
        providers_importable: importable.saturating_add(existing_plugin_sources) as u64,
    });
    let hidden_missing_sources = listing.hidden_missing_sources;
    let mut canonical_entries = sources_json_with_selection(sources, &provider_roots);
    canonical_entries.extend(plugin_sources_json(&plugin_sources));
    canonical_entries.extend(plugin_manifest_failures_json(&plugin_failures));
    let result_count = canonical_entries.len();
    let content_bytes = serde_json::to_vec(&canonical_entries)?.len();
    let output_bytes = if request.format == OutputFormat::Json {
        let (issues, issues_truncated) = discovery_report_issues_json(&discovery_report);
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
            sources,
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

#[cfg(test)]
use ctx_history_ingest_application::{merge_sources, source_identity, source_is_visible};

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
        document.append(render_discovery_issue(context, issue));
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

fn render_discovery_issue(context: &RenderContext, issue: &DiscoveryIssue) -> Document {
    let provider = source_provider_cli_name(issue.provider);
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
mod ui_tests {
    use std::{io::Write as _, path::PathBuf};

    use ctx_history_capture::{
        ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    };
    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use ctx_terminal::{ColorMode, StreamKind, TestContext};

    fn context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let width = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            let copyable_path = {
                let atom = line.trim_start();
                atom.starts_with("~/") || atom.starts_with('/')
            };
            assert!(
                line.width() <= width || copyable_path,
                "{line:?} exceeded {width} columns"
            );
        }
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    fn source(status: ProviderSourceStatus, path: &str) -> ProviderSource {
        ProviderSource {
            provider: CaptureProvider::Codex,
            path: PathBuf::from(path),
            exists: status != ProviderSourceStatus::Missing,
            source_format: "codex_session_jsonl_tree",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::Native,
            status,
            unsupported_reason: None,
        }
    }

    #[test]
    fn source_merge_is_stable_and_keeps_configured_missing_sources_visible() {
        let automatic = source(ProviderSourceStatus::Available, "/tmp/shared-history");
        let configured_duplicate = automatic.clone();
        let configured_missing = source(ProviderSourceStatus::Missing, "/tmp/configured-missing");
        let mut merged = vec![automatic];
        merge_sources(
            &mut merged,
            vec![configured_duplicate, configured_missing.clone()],
        );
        assert_eq!(
            merged
                .iter()
                .map(|source| source.path.as_path())
                .collect::<Vec<_>>(),
            [
                std::path::Path::new("/tmp/shared-history"),
                std::path::Path::new("/tmp/configured-missing"),
            ]
        );

        let configured = [source_identity(&configured_missing)].into_iter().collect();
        assert!(source_is_visible(
            &configured_missing,
            false,
            &configured,
            &[]
        ));
        let mut unknown_missing = source(ProviderSourceStatus::Missing, "/tmp/unknown-missing");
        unknown_missing.provider = CaptureProvider::Goose;
        assert!(!source_is_visible(
            &unknown_missing,
            false,
            &configured,
            &[]
        ));
    }

    #[test]
    fn configured_source_selection_is_visible_and_automatic_disable_is_explicit() {
        let mut configured_source = source(ProviderSourceStatus::Available, "/tmp/claude/projects");
        configured_source.provider = CaptureProvider::Claude;
        configured_source.source_format = "claude_projects_jsonl_tree";
        let root = ctx_history_capture::ProviderRootDefinition {
            id: "personal-claude".to_owned(),
            provider: CaptureProvider::Claude,
            path: PathBuf::from("/tmp/claude"),
            group: Some("personal".to_owned()),
        };
        assert_eq!(
            configured_root_for_source(std::slice::from_ref(&root), &configured_source)
                .map(|root| root.id.as_str()),
            Some("personal-claude")
        );

        let context = context(100, ColorMode::Never);
        let rendered = render_sources_human(
            &context,
            SourcesHumanRenderInput::from_sources(&[configured_source])
                .with_automatic_provider_discovery(false)
                .with_provider_roots(&[root]),
        )
        .render_plain();
        assert!(
            rendered.contains("personal-claude (personal)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Automatic discovery is disabled"),
            "{rendered}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn configured_source_selection_recognizes_physical_path_aliases() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let physical = temp.path().join("claude-physical");
        std::fs::create_dir_all(physical.join("projects")).unwrap();
        let alias = temp.path().join("claude-alias");
        symlink(&physical, &alias).unwrap();
        let mut source = source(
            ProviderSourceStatus::Available,
            &physical.join("projects").to_string_lossy(),
        );
        source.provider = CaptureProvider::Claude;
        source.source_format = "claude_projects_jsonl_tree";
        let root = ctx_history_capture::ProviderRootDefinition {
            id: "personal-claude".to_owned(),
            provider: CaptureProvider::Claude,
            path: alias,
            group: Some("personal".to_owned()),
        };

        assert_eq!(
            configured_root_for_source(std::slice::from_ref(&root), &source)
                .map(|root| root.id.as_str()),
            Some("personal-claude")
        );
    }

    #[test]
    fn sources_success_is_outcome_first_and_responsive() {
        let home = PathBuf::from("private-capture-root");
        let location = home.join(".codex/sessions/and/a/long/location");
        let sources = vec![source(
            ProviderSourceStatus::Available,
            &location.to_string_lossy(),
        )];
        let concise_prefix = Path::new("~").join(".codex").display().to_string();
        for width in [32, 48, 80, 100, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_sources_human(
                &context,
                SourcesHumanRenderInput::from_sources(&sources)
                    .with_hidden_missing_sources(2)
                    .with_home(Some(&home)),
            );
            let rendered = document.render_plain();
            assert!(rendered.starts_with("✓ 1 history source is ready\n"));
            assert!(rendered.contains("Locations\n"));
            assert!(rendered.contains(&concise_prefix), "{width}: {rendered}");
            assert!(
                !rendered.contains("private-capture-root"),
                "{width}: {rendered}"
            );
            assert!(rendered.contains("ctx sources --all"));
            for atom in ["codex", "available"] {
                assert_eq!(
                    rendered
                        .split_whitespace()
                        .filter(|token| *token == atom)
                        .count(),
                    1,
                    "{atom:?} did not remain intact at {width} columns: {rendered}"
                );
            }
            assert!(rendered.contains("Session history"), "{width}: {rendered}");
            assert!(!rendered.contains("jsonl"), "{width}: {rendered}");
            if width < 100 {
                assert!(
                    rendered.contains("Source\n  codex\nStatus\n  available\n"),
                    "{width}: {rendered}"
                );
            } else {
                assert!(
                    rendered.contains("Source  Status     Location"),
                    "{width}: {rendered}"
                );
            }
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn human_paths_only_abbreviate_complete_home_prefixes() {
        let home = PathBuf::from("test-home/example");
        assert_eq!(human_path(&home, Some(&home)), "~");
        let nested = home.join(".codex/sessions");
        assert_eq!(
            human_path(&nested, Some(&home)),
            Path::new("~").join(".codex/sessions").display().to_string()
        );
        let sibling = PathBuf::from("test-home/example-other/history");
        assert_eq!(
            human_path(&sibling, Some(&home)),
            sibling.display().to_string()
        );
    }

    #[test]
    fn sources_stack_when_fixed_columns_do_not_fit_and_keep_atoms_whole() {
        let home = PathBuf::from("test-home");
        let factory_path = home.join(".factory/sessions");
        let mut factory = source(
            ProviderSourceStatus::Available,
            &factory_path.to_string_lossy(),
        );
        factory.provider = CaptureProvider::FactoryAiDroid;
        factory.source_format = "factory_ai_droid_sessions_jsonl";
        let cursor_path = home.join(".cursor/projects/example/agent-transcripts");
        let mut cursor = source(
            ProviderSourceStatus::Available,
            &cursor_path.to_string_lossy(),
        );
        cursor.provider = CaptureProvider::Cursor;
        cursor.source_format = "cursor_agent_transcript_jsonl_tree";
        let sources = [factory, cursor];

        for width in [80, 100, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_sources_human(
                &context,
                SourcesHumanRenderInput::from_sources(&sources).with_home(Some(&home)),
            );
            let rendered = document.render_plain();
            for atom in ["factory-ai-droid", "available", "cursor"] {
                assert!(
                    rendered.split_whitespace().any(|token| token == atom),
                    "{atom:?} did not remain intact at {width} columns: {rendered}"
                );
            }
            assert!(rendered.contains("Session history"), "{width}: {rendered}");
            assert!(
                rendered.contains("Agent transcripts"),
                "{width}: {rendered}"
            );
            assert!(!rendered.contains("jsonl"), "{width}: {rendered}");
            if width < 120 {
                assert!(
                    rendered.contains("Source\n  factory-ai-droid\nStatus\n  available\n"),
                    "{rendered}"
                );
            } else {
                assert!(
                    rendered.contains("Source            Status     Location"),
                    "{width}: {rendered}"
                );
            }
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn sources_empty_state_is_actionable() {
        let context = context(48, ColorMode::Never);
        let rendered = render_sources_human(&context, SourcesHumanRenderInput::from_sources(&[]))
            .render_plain();
        assert!(rendered.starts_with("No history sources found\n"));
        assert!(rendered.contains("Next\n  ctx sources --all\n"));
    }

    #[test]
    fn sources_issue_is_safe_and_actionable() {
        let issue = DiscoveryIssue {
            provider: CaptureProvider::Codex,
            path: None,
            kind: DiscoveryIssueKind::SelectorUnreconstructible,
            reason: "selector contained \u{1b}[31mcontrol",
        };
        let context = context(48, ColorMode::Never);
        let document = render_sources_human(
            &context,
            SourcesHumanRenderInput::from_sources(&[]).with_issues(&[issue]),
        );
        let rendered = document.render_plain();
        assert!(rendered.contains("\\x1b[31mcontrol"));
        assert!(rendered.contains("ctx import --provider codex --path <path>"));
        assert!(!rendered.as_bytes().contains(&0x1b));
        assert_fits(&document, &context);
    }

    #[test]
    fn sources_plain_output_matches_ansi_stripped_output() {
        let sources = vec![source(ProviderSourceStatus::Available, "/tmp/codex")];
        let context = context(80, ColorMode::Always);
        let document =
            render_sources_human(&context, SourcesHumanRenderInput::from_sources(&sources));
        assert_eq!(
            strip_ansi(&document.render(&context)),
            document.render_plain()
        );
    }
}
