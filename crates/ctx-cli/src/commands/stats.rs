use std::path::PathBuf;

use anyhow::Result;

use crate::{
    local_usage::{self, UsageReport},
    output::print_json,
    ui::{
        diagnostic, empty_state, fields, outcome, section, table, Action, Diagnostic,
        DiagnosticLevel, Document, EmptyState, Field, Outcome, OutcomeState, RenderContext, Table,
        Ui,
    },
    StatsArgs,
};

/// Read and render the aggregate-only local report.
///
/// Dispatch excludes this command before constructing its completion draft, so
/// the detached read-only snapshot can never count the report itself.
pub(crate) fn run(
    args: StatsArgs,
    data_root: PathBuf,
    local_usage_enabled: bool,
    ui: &mut Ui,
) -> Result<()> {
    let report = local_usage::read_report(&data_root, local_usage_enabled, true);
    if args.format.is_json() {
        print_json(serde_json::to_value(report)?)
    } else {
        let document = render_stats_human(ui.stdout_context(), &report, args.detail);
        ui.write_stdout(&document)?;
        Ok(())
    }
}

pub(crate) fn malformed_config_failure(json_output: bool, ui: &mut Ui) -> Result<()> {
    let report = UsageReport::config_error();
    if json_output {
        eprintln!("{}", serde_json::to_string(&report)?);
    } else {
        let document = render_stats_human(ui.stderr_context(), &report, false);
        ui.write_stderr(&document)?;
    }
    Err(crate::dispatch::rendered_cli_error())
}

fn render_stats_human(context: &RenderContext, report: &UsageReport, detailed: bool) -> Document {
    if let Some(error) = &report.error {
        return diagnostic(
            context,
            Diagnostic {
                level: DiagnosticLevel::Error,
                summary: "Local usage report is unavailable",
                detail: None,
                fields: &[
                    Field::new("Code", error.code),
                    Field::new("Detail", error.message),
                ],
                action: Some(Action {
                    command: "ctx stats",
                }),
            },
        );
    }
    if !report.enabled {
        return empty_state(
            context,
            EmptyState {
                title: "Local usage is disabled",
                detail: "Aggregate-only local usage measurement is not collecting new facts.",
                action: Some(Action {
                    command: "ctx status --usage enable",
                }),
            },
        );
    }
    if report.state == "empty"
        || report
            .definitions
            .as_ref()
            .is_some_and(|definitions| definitions.is_empty())
    {
        return empty_state(
            context,
            EmptyState {
                title: "No local usage recorded yet",
                detail:
                    "Use ctx normally, then run this command again for an aggregate-only report.",
                action: None,
            },
        );
    }

    let definition_count = report.definitions.as_ref().map_or(0, Vec::len);
    let title = match definition_count {
        0 | 1 => "Local usage summary".to_owned(),
        count => format!("Local usage across {count} measurement definitions"),
    };
    let retention = format!(
        "{} days of aggregate-only local facts",
        report.retention_days
    );
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Success,
            title: &title,
            detail: Some(&retention),
        },
    );

    if let Some(definitions) = &report.definitions {
        for definition in definitions {
            let summary = &definition.summary;
            let heading = format!(
                "Measured local facts · definition {}",
                definition.definition_version
            );
            let period = format!(
                "{} active UTC {} · {} through {}",
                definition.active_days,
                if definition.active_days == 1 {
                    "day"
                } else {
                    "days"
                },
                definition.first_day_utc,
                definition.last_day_utc
            );
            let versions = definition.ctx_versions.join(", ");
            let calls = format!(
                "{} total · {} succeeded · {} failed",
                summary.calls, summary.successful_calls, summary.failed_calls
            );
            let result_sets = format!(
                "{} nonempty, {} empty",
                summary.result_bearing_calls, summary.empty_calls
            );
            let unclassified = format!(
                "{} {}",
                summary.not_applicable_calls,
                if summary.not_applicable_calls == 1 {
                    "call"
                } else {
                    "calls"
                }
            );
            let results = format!(
                "{} results · {} unique blame citations",
                summary.result_count, summary.citation_count
            );
            let output = format!("{} bytes", summary.delivered_output_bytes);
            let covered_context = format!("{} bytes", summary.delivered_context_bytes);
            let matched_history = format!("{} bytes", summary.matched_normalized_session_bytes);
            let coverage = format!(
                "{} complete · {} unavailable",
                summary.complete_context_eligible_calls, summary.unavailable_context_eligible_calls
            );
            let mut values = vec![
                Field::new("Period", &period),
                Field::new("ctx versions", &versions),
                Field::new("Calls", &calls),
                Field::new("Classified result sets", &result_sets),
                Field::new("No result-set classification", &unclassified),
                Field::new("Results", &results),
                Field::new("Delivered output", &output),
                Field::new("Covered context", &covered_context),
                Field::new("Matched history", &matched_history),
                Field::new("Search coverage", &coverage),
            ];
            let blame = &summary.pro_blame;
            let blame_outcomes = (blame.requests > 0).then(|| {
                format!(
                    "{} produced-attribution · {} possible-only · {} none · {} error",
                    blame.produced_attribution_requests,
                    blame.possible_only_requests,
                    blame.none_requests,
                    blame.error_requests
                )
            });
            if let Some(blame_outcomes) = blame_outcomes.as_deref() {
                values.push(Field::new("Blame outcomes", blame_outcomes));
            }
            document.push_blank();
            document.append(section(&heading, fields(context, &values)));

            if detailed && !definition.by_operation.is_empty() {
                let mut operations =
                    Table::new(["Operation", "Version", "Calls", "Result sets", "Context"]);
                for operation in &definition.by_operation {
                    operations.push_row([
                        format!("{}/{}", operation.surface, operation.operation),
                        operation.ctx_version.clone(),
                        format!(
                            "{} · {} ok · {} failed",
                            operation.calls, operation.successful_calls, operation.failed_calls
                        ),
                        format!(
                            "{} nonempty · {} empty · {} n/a",
                            operation.result_bearing_calls,
                            operation.empty_calls,
                            operation.not_applicable_calls
                        ),
                        format!(
                            "{} output · {} covered · {} complete · {} unavailable",
                            operation.delivered_output_bytes,
                            operation.delivered_context_bytes,
                            operation.complete_context_eligible_calls,
                            operation.unavailable_context_eligible_calls
                        ),
                    ]);
                }
                document.push_blank();
                document.append(section("Operations", table(context, &operations)));
            }
            if detailed && !definition.duration_buckets.is_empty() {
                let mut durations = Table::new(["Duration", "Calls"]);
                for duration in &definition.duration_buckets {
                    durations
                        .push_row([duration.duration_bucket.clone(), duration.calls.to_string()]);
                }
                document.push_blank();
                document.append(section("Latency", table(context, &durations)));
            }
        }
    }

    if let Some(estimates) = &report.estimates {
        let tokens = estimates.approximate_context_tokens;
        let delivered = format!("{} bytes", tokens.delivered_context_bytes);
        let range = format!(
            "{} low · {} central · {} high",
            tokens.token_equivalents.low,
            tokens.token_equivalents.central,
            tokens.token_equivalents.high
        );
        document.push_blank();
        document.append(section(
            "Approximate token-equivalents",
            fields(
                context,
                &[
                    Field::new("Covered context", &delivered),
                    Field::new("Range", &range),
                    Field::new("Coefficient", tokens.coefficient_version),
                ],
            ),
        ));

        let reduction = estimates.estimated_context_reduction;
        let bytes = format!(
            "{} baseline · {} observed · {} estimated reduction",
            reduction.comparison_baseline_bytes,
            reduction.observed_delivered_context_bytes,
            reduction.estimated_avoided_context_bytes
        );
        let token_range = format!(
            "{} low · {} central · {} high",
            reduction.approximate_token_equivalents.low,
            reduction.approximate_token_equivalents.central,
            reduction.approximate_token_equivalents.high
        );
        let coverage = format!(
            "{} covered · {} unavailable",
            reduction.covered_calls, reduction.unavailable_calls
        );
        document.push_blank();
        document.append(section(
            "Estimated context reduction",
            fields(
                context,
                &[
                    Field::new("Bytes", &bytes),
                    Field::new("Token-equivalents", &token_range),
                    Field::new("Coverage", &coverage),
                    Field::new("Model", reduction.estimate_model_version),
                    Field::new("Coefficient", reduction.coefficient_version),
                ],
            ),
        ));
    }
    document
}

#[cfg(test)]
mod ui_tests {
    use std::io::Write as _;

    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn report(enabled: bool, state: &'static str) -> UsageReport {
        UsageReport {
            schema_version: 2,
            local_only: true,
            read_only: true,
            enabled,
            state,
            retention_days: 400,
            definition_version: 2,
            definitions: None,
            estimates: None,
            error: None,
        }
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

    #[test]
    fn stats_ready_output_is_outcome_first_and_responsive() {
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_stats_human(&context, &report(true, "ready"), false);
            assert!(document
                .render_plain()
                .starts_with("✓ Local usage summary\n"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn stats_uses_canonical_fact_wording_and_singular_grammar() {
        let context = context(120, ColorMode::Never);
        let rendered =
            render_stats_human(&context, &UsageReport::ui_test_ready(), false).render_plain();
        assert!(rendered.contains("Measured local facts · definition 2"));
        assert!(rendered.contains("1 active UTC day"));
        assert!(rendered.contains("Classified result sets"));
        assert!(rendered.contains("1 nonempty, 2 empty"));
        assert!(rendered.contains("No result-set classification"));
        assert!(rendered.contains("1 call"));
        assert!(rendered.contains("1 produced-attribution · 1 possible-only"));
        assert!(!rendered.contains("1 UTC days"));
        assert!(!rendered.contains("unclassified"));
    }

    #[test]
    fn stats_empty_and_disabled_states_are_clear() {
        let context = context(48, ColorMode::Never);
        let empty = render_stats_human(&context, &report(true, "empty"), false).render_plain();
        assert!(empty.starts_with("No local usage recorded yet\n"));

        let disabled =
            render_stats_human(&context, &report(false, "disabled"), false).render_plain();
        assert!(disabled.starts_with("Local usage is disabled\n"));
        assert!(disabled.contains("ctx status --usage enable"));
    }

    #[test]
    fn stats_error_is_structured_and_actionable() {
        let context = context(48, ColorMode::Never);
        let document = render_stats_human(&context, &UsageReport::config_error(), false);
        let rendered = document.render_plain();
        assert!(rendered.starts_with("✗ Local usage report is unavailable\n"));
        assert!(rendered.contains("local_usage_config_unavailable"));
        assert!(rendered.contains("Next\n  ctx stats\n"));
        assert_fits(&document, &context);
    }

    #[test]
    fn stats_plain_output_matches_ansi_stripped_output() {
        let context = context(80, ColorMode::Always);
        let document = render_stats_human(&context, &report(true, "ready"), false);
        assert_eq!(
            strip_ansi(&document.render(&context)),
            document.render_plain()
        );
    }
}
