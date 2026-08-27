use anyhow::Result;
use clap::Args;

use crate::{
    local_usage::{self, UsageReport},
    output::print_json,
    ui::{
        diagnostic, empty_state, fields, outcome, section, table, Action, Diagnostic,
        DiagnosticLevel, Document, EmptyState, Field, Outcome, OutcomeState, RenderContext, Table,
        Ui,
    },
};

#[derive(Debug, Args, Clone)]
pub struct StatsArgs {
    #[arg(long, help = "Show CLI/MCP operation and latency breakdowns")]
    pub detail: bool,
    #[arg(long, value_enum, default_value_t = crate::JsonOutputFormat::Text)]
    pub format: crate::JsonOutputFormat,
}

/// Read and render the aggregate-only local report.
///
/// Dispatch excludes this command before constructing its completion draft, so
/// the detached read-only snapshot can never count the report itself.
pub fn run(
    args: StatsArgs,
    storage: &local_usage::LocalUsageStorageAuthority,
    control: &local_usage::UsageControlSnapshot,
    ui: &mut Ui,
) -> Result<()> {
    let report = local_usage::read_report_authorized(storage, control, true);
    if args.format.is_json() {
        print_json(serde_json::to_value(report)?)
    } else {
        let document = render_stats_human(ui.stdout_context(), &report, args.detail);
        ui.write_stdout(&document)?;
        Ok(())
    }
}

pub fn malformed_config_failure(json_output: bool, ui: &mut Ui) -> Result<()> {
    let report = UsageReport::config_error();
    if json_output {
        let output = format!("{}\n", serde_json::to_string(&report)?);
        ui.write_stderr_bytes(output.as_bytes())?;
    } else {
        let document = render_stats_human(ui.stderr_context(), &report, false);
        ui.write_stderr(&document)?;
    }
    Err(crate::rendered_cli_error())
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
            let results = format!("{} results", summary.result_count);
            let output = format!("{} bytes", summary.delivered_output_bytes);
            let covered_context = format!("{} bytes", summary.delivered_context_bytes);
            let matched_history = format!("{} bytes", summary.matched_normalized_session_bytes);
            let coverage = format!(
                "{} complete · {} unavailable",
                summary.complete_context_eligible_calls, summary.unavailable_context_eligible_calls
            );
            let values = vec![
                Field::new("Period", &period),
                Field::new("ctx versions", &versions),
                Field::new("Calls", &calls),
                Field::new("Classified result sets", &result_sets),
                Field::new("No result-set classification", &unclassified),
                Field::new("Results", &results),
                Field::new("Measured delivered output", &output),
                Field::new("Covered context", &covered_context),
                Field::new("Matched history", &matched_history),
                Field::new("Search coverage", &coverage),
            ];
            document.push_blank();
            document.append(section(&heading, fields(context, &values)));

            if detailed && !definition.by_operation.is_empty() {
                let mut operations =
                    Table::new(["Operation", "Version", "Calls", "Result sets", "Context"]);
                for operation in &definition.by_operation {
                    let output = measured_operation_output(
                        &operation.surface,
                        &operation.operation,
                        operation.delivered_output_bytes,
                    );
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
                            "{} · {} covered · {} complete · {} unavailable",
                            output,
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

fn measured_operation_output(surface: &str, operation: &str, bytes: u64) -> String {
    if surface == "cli" && operation == "blame" {
        "output n/a".to_owned()
    } else {
        format!("{bytes} output")
    }
}

#[cfg(test)]
mod ui_tests {
    use std::{
        io::{self, Write as _},
        sync::{Arc, Mutex},
    };

    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn report(enabled: bool, state: &'static str) -> UsageReport {
        UsageReport {
            schema_version: 3,
            local_only: true,
            read_only: true,
            enabled,
            state,
            retention_days: 400,
            definition_version: 3,
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

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl SharedWriter {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl io::Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
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
        assert!(rendered.contains("Measured local facts · definition 3"));
        assert!(rendered.contains("1 active UTC day"));
        assert!(rendered.contains("Classified result sets"));
        assert!(rendered.contains("1 nonempty, 2 empty"));
        assert!(rendered.contains("No result-set classification"));
        assert!(rendered.contains("1 call"));
        assert!(rendered.contains("Results"));
        assert!(!rendered.contains("1 UTC days"));
        assert!(!rendered.contains("unclassified"));
    }

    #[test]
    fn stats_marks_only_unmeasured_cli_blame_output_as_unavailable() {
        assert_eq!(measured_operation_output("cli", "blame", 0), "output n/a");
        assert_eq!(measured_operation_output("mcp", "blame", 0), "0 output");
        assert_eq!(measured_operation_output("cli", "search", 0), "0 output");
    }

    #[test]
    fn stats_json_uses_replacement_schema_without_removed_aliases() {
        let report = serde_json::to_value(UsageReport::ui_test_ready()).unwrap();
        assert_eq!(report["schema_version"], 3);
        let summary = &report["definitions"][0]["summary"];
        assert!(summary.get("citation_count").is_none());
        assert!(summary.get("pro_blame").is_none());
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
    fn malformed_config_machine_error_is_exact_json_on_stderr_only() {
        let stdout = SharedWriter::default();
        let stdout_copy = stdout.clone();
        let stderr = SharedWriter::default();
        let stderr_copy = stderr.clone();
        let mut ui = Ui::with_writers(
            stdout,
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
            stderr,
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
        );

        let error = malformed_config_failure(true, &mut ui).unwrap_err();

        let value: serde_json::Value = serde_json::from_str(stderr_copy.text().trim()).unwrap();
        assert_eq!(value["state"], "error");
        assert_eq!(value["error"]["code"], "local_usage_config_unavailable");
        assert!(stderr_copy.text().ends_with('\n'));
        assert!(stdout_copy.text().is_empty());
        assert!(error.is::<crate::RenderedCliError>());
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
