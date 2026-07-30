use std::{collections::BTreeSet, path::PathBuf};

use anyhow::Result;
use serde_json::json;

use ctx_history_capture::{
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderSourceStatus,
};
use ctx_history_core::CaptureProvider;

use crate::analytics::{count_bucket, SourcesTelemetry};
use crate::commands::import::load_explicit_source_catalog_sources;
use crate::history_source_plugins::discover_history_source_plugins_with_diagnostics;
use crate::local_usage::{CliUsage, ResultObservationAction};
use crate::output::print_json;
use crate::provider_args::ProviderArg;
use crate::provider_sources::{
    discovered_sources_for_provider_report, discovered_sources_report,
    discovery_report_issues_json, manual_path_guidance, plugin_manifest_failures_json,
    plugin_sources_json, provider_cli_name, sources_json, SourceInfo,
};
use crate::ui::{
    canonical_human_output_bytes, diagnostic, empty_state, hint, outcome, section, table, Action,
    Diagnostic, DiagnosticLevel, Document, EmptyState, Field, Hint, Outcome, OutcomeState,
    RenderContext, Table, Ui,
};
use crate::{SourcesArgs, DEFAULT_VISIBLE_SOURCE_PROVIDERS};

pub(crate) fn run_sources(
    args: SourcesArgs,
    data_root: PathBuf,
    telemetry: &mut SourcesTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let provider_filter = args.provider.map(ProviderArg::capture_provider);
    let mut discovery_report = match provider_filter {
        Some(CaptureProvider::Custom) => DiscoveryReport::default(),
        Some(provider) => discovered_sources_for_provider_report(provider),
        None => discovered_sources_report(),
    };
    let configured_sources = load_explicit_source_catalog_sources(&data_root)?
        .into_iter()
        .filter(|source| provider_filter.is_none_or(|provider| source.provider == provider))
        .collect::<Vec<_>>();
    let configured_identities = configured_sources
        .iter()
        .map(source_identity)
        .collect::<BTreeSet<_>>();
    merge_sources(&mut discovery_report.sources, configured_sources);
    let sources = &discovery_report.sources;
    let plugin_discovery = discover_history_source_plugins_with_diagnostics(&data_root, &[])?;
    let (plugin_sources, plugin_failures) = if matches!(provider_filter, Some(provider) if provider != CaptureProvider::Custom)
    {
        (Vec::new(), Vec::new())
    } else {
        (plugin_discovery.sources, plugin_discovery.failures)
    };
    let existing = sources.iter().filter(|source| source.exists).count();
    let importable = sources
        .iter()
        .filter(|source| {
            source.exists
                && source.import_support.is_importable()
                && source.status == ProviderSourceStatus::Available
        })
        .count();
    telemetry.providers_detected = Some(count_bucket(
        sources
            .len()
            .saturating_add(plugin_sources.len())
            .saturating_add(plugin_failures.len()) as u64,
    ));
    telemetry.providers_existing = Some(count_bucket(
        existing.saturating_add(plugin_sources.len()) as u64,
    ));
    telemetry.providers_importable = Some(count_bucket(
        importable.saturating_add(plugin_sources.len()) as u64,
    ));
    let show_all_sources = args.all || args.show_missing || provider_filter.is_some();
    let visible_sources = sources
        .iter()
        .filter(|source| source_is_visible(source, show_all_sources, &configured_identities))
        .cloned()
        .collect::<Vec<_>>();
    let hidden_missing_sources = sources.len().saturating_sub(visible_sources.len());
    let mut canonical_entries = sources_json(&visible_sources);
    canonical_entries.extend(plugin_sources_json(&plugin_sources));
    canonical_entries.extend(plugin_manifest_failures_json(&plugin_failures));
    let result_count = canonical_entries.len();
    let content_bytes = serde_json::to_vec(&canonical_entries)?.len();
    let output_bytes = if args.format.is_json() {
        let (issues, issues_truncated) = discovery_report_issues_json(&discovery_report);
        let value = json!({
            "schema_version": 1,
            "scope": if show_all_sources { "all" } else { "default" },
            "hidden_missing_sources": hidden_missing_sources,
            "sources": canonical_entries,
            "issues": issues,
            "issues_truncated": issues_truncated,
        });
        let output_bytes = serde_json::to_string_pretty(&value)?
            .len()
            .saturating_add(1);
        print_json(value)?;
        output_bytes
    } else {
        let document = render_sources_human(
            ui.stdout_context(),
            &visible_sources,
            &discovery_report.issues,
            &plugin_sources,
            &plugin_failures,
            hidden_missing_sources,
        );
        let output_bytes = canonical_human_output_bytes(|context| {
            render_sources_human(
                context,
                &visible_sources,
                &discovery_report.issues,
                &plugin_sources,
                &plugin_failures,
                hidden_missing_sources,
            )
        });
        ui.write_stdout(&document)?;
        output_bytes
    };
    local_usage.set_result_observation(
        ResultObservationAction::Sources,
        result_count,
        0,
        content_bytes,
    );
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(())
}

type SourceIdentity = (String, PathBuf, String);

fn source_identity(source: &SourceInfo) -> SourceIdentity {
    (
        source.provider.as_str().to_owned(),
        source.path.clone(),
        source.source_format.to_owned(),
    )
}

fn merge_sources(discovered: &mut Vec<SourceInfo>, configured: Vec<SourceInfo>) {
    let mut seen = BTreeSet::new();
    discovered.retain(|source| seen.insert(source_identity(source)));
    discovered.extend(
        configured
            .into_iter()
            .filter(|source| seen.insert(source_identity(source))),
    );
}

fn source_is_visible(
    source: &SourceInfo,
    show_all_sources: bool,
    configured_identities: &BTreeSet<SourceIdentity>,
) -> bool {
    show_all_sources
        || configured_identities.contains(&source_identity(source))
        || source_visible_by_default(source)
}

fn render_sources_human(
    context: &RenderContext,
    sources: &[SourceInfo],
    issues: &[DiscoveryIssue],
    plugin_sources: &[crate::history_source_plugins::HistorySourcePluginSource],
    plugin_failures: &[crate::history_source_plugins::HistorySourcePluginManifestFailure],
    hidden_missing_sources: usize,
) -> Document {
    if sources.is_empty()
        && issues.is_empty()
        && plugin_sources.is_empty()
        && plugin_failures.is_empty()
    {
        return empty_state(
            context,
            EmptyState {
                title: "No history sources found",
                detail: "Select a provider or inspect every known provider location.",
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
        .saturating_add(plugin_sources.len());
    let title = match importable {
        0 => "No importable history sources found".to_owned(),
        1 => "1 history source is ready".to_owned(),
        count => format!("{count} history sources are ready"),
    };
    let attention = sources
        .iter()
        .filter(|source| source.status == ProviderSourceStatus::Unsupported)
        .count()
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
        let mut locations = Table::new(["Source", "Status", "Location", "Format"]);
        for source in sources {
            locations.push_row([
                source_provider_cli_name(source.provider).to_owned(),
                source.status.as_str().to_owned(),
                source.path.display().to_string(),
                source.source_format.to_owned(),
            ]);
        }
        for source in plugin_sources {
            locations.push_row([
                format!("custom/{}", source.label()),
                "available".to_owned(),
                "history-source-plugin".to_owned(),
                source.source_format.clone(),
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
        let location = source.path.display().to_string();
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
    for issue in issues {
        document.push_blank();
        document.append(render_discovery_issue(context, issue));
    }
    for failure in plugin_failures {
        let manifest = failure.manifest_path.display().to_string();
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
    document
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

pub(crate) fn source_visible_by_default(source: &SourceInfo) -> bool {
    source.exists
        || source.status != ProviderSourceStatus::Missing
        || DEFAULT_VISIBLE_SOURCE_PROVIDERS.contains(&source.provider)
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
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let width = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            assert!(line.width() <= width, "{line:?} exceeded {width} columns");
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
        assert!(source_is_visible(&configured_missing, false, &configured));
        let mut unknown_missing = source(ProviderSourceStatus::Missing, "/tmp/unknown-missing");
        unknown_missing.provider = CaptureProvider::Goose;
        assert!(!source_is_visible(&unknown_missing, false, &configured));
    }

    #[test]
    fn sources_success_is_outcome_first_and_responsive() {
        let sources = vec![source(
            ProviderSourceStatus::Available,
            "/tmp/history with spaces/and/a/long/location",
        )];
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_sources_human(&context, &sources, &[], &[], &[], 2);
            let rendered = document.render_plain();
            assert!(rendered.starts_with("✓ 1 history source is ready\n"));
            assert!(rendered.contains("Locations\n"));
            assert!(rendered.contains("/tmp/history"));
            assert!(rendered.contains("spaces/and/a/long/location"));
            assert!(rendered.contains("ctx sources --all"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn sources_empty_state_is_actionable() {
        let context = context(48, ColorMode::Never);
        let rendered = render_sources_human(&context, &[], &[], &[], &[], 0).render_plain();
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
        let document = render_sources_human(&context, &[], &[issue], &[], &[], 0);
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
        let document = render_sources_human(&context, &sources, &[], &[], &[], 0);
        assert_eq!(
            strip_ansi(&document.render(&context)),
            document.render_plain()
        );
    }
}
