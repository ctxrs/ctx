use ctx_history_read_application::HistoryHealthReport;
use serde_json::Value;

use crate::ui::{
    evidence_list, fields, hint, outcome, section, Action, Document, Evidence, Field, Hint,
    Outcome, OutcomeState, RenderContext, Span,
};

use super::history_health::history_partial_cause;

const MAX_REFRESH_ERROR_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorSearchAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct DoctorRefreshFailure {
    pub detail: String,
    pub search: DoctorSearchAvailability,
    pub partial: bool,
}

pub fn source_epoch_findings(report: &Value, semantic_required: bool) -> Vec<String> {
    let mut findings = Vec::new();
    if report
        .pointer("/daemon/heartbeat_stale")
        .and_then(Value::as_bool)
        == Some(true)
    {
        findings.push("daemon is running (heartbeat_stale)".to_owned());
    }
    for (name, required) in [
        ("history_epoch", true),
        ("lexical", true),
        ("catalog", true),
        ("refresh", true),
        ("semantic", semantic_required),
    ] {
        if !required {
            continue;
        }
        let component = &report[name];
        let status = component
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unavailable");
        if !matches!(status, "ready" | "disabled") {
            let reason = component
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            findings.push(format!("{name} is {status} ({reason})"));
        }
    }
    findings
}

pub fn render_doctor_human(
    context: &RenderContext,
    findings: &[String],
    coverage: Option<&HistoryHealthReport>,
    refresh_failure: Option<DoctorRefreshFailure>,
) -> Document {
    let refresh_failed = findings.iter().any(|finding| {
        finding.contains("(source_refresh_failed)") || finding.contains("(core_refresh_failed)")
    });
    let mut human_findings = findings
        .iter()
        .filter(|finding| !refresh_failed || !is_derivative_refresh_finding(finding))
        .filter(|finding| refresh_failure.is_none() || !finding.starts_with("refresh is "))
        .map(|finding| humanize_doctor_finding(finding))
        .collect::<Vec<_>>();
    if let Some(failure) = refresh_failure.as_ref() {
        human_findings.insert(
            0,
            HumanDoctorFinding {
                summary: if failure.partial {
                    "History refresh is partial"
                } else {
                    "History refresh failed"
                }
                .to_owned(),
                detail: Some(bounded_terminal_detail(&failure.detail)),
            },
        );
    } else if refresh_failed {
        human_findings.insert(
            0,
            HumanDoctorFinding {
                summary: "History refresh needs repair".to_owned(),
                detail: Some("A refresh did not complete; healthy prior history remains searchable when shown as available.".to_owned()),
            },
        );
    } else if let Some(cause) = history_partial_cause(coverage) {
        human_findings.insert(
            0,
            HumanDoctorFinding {
                summary: "History coverage is partial".to_owned(),
                detail: Some(format!(
                    "{cause}. Healthy prior history remains searchable."
                )),
            },
        );
    }
    let title = match human_findings.len() {
        0 => "No problems found".to_owned(),
        1 => "ctx found 1 issue".to_owned(),
        count => format!("ctx found {count} issues"),
    };
    let mut document = outcome(
        context,
        Outcome {
            state: if human_findings.is_empty() {
                OutcomeState::Success
            } else {
                OutcomeState::Warning
            },
            title: &title,
            detail: None,
        },
    );
    if let Some(failure) = refresh_failure.as_ref() {
        document.push_blank();
        document.append(section(
            "Search",
            fields(
                context,
                &[Field::new(
                    "Availability",
                    match failure.search {
                        DoctorSearchAvailability::Available => {
                            "Available — healthy prior history remains searchable"
                        }
                        DoctorSearchAvailability::Unavailable => {
                            "Unavailable — no verified history is searchable"
                        }
                    },
                )],
            ),
        ));
    }
    if human_findings.is_empty() {
        return document;
    }

    let references = (1..=human_findings.len())
        .map(|index| index.to_string())
        .collect::<Vec<_>>();
    let evidence = references
        .iter()
        .zip(&human_findings)
        .map(|(reference, finding)| Evidence {
            reference,
            summary: &finding.summary,
            detail: finding.detail.as_deref(),
        })
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Issues", evidence_list(context, &evidence)));
    document.push_blank();
    let coverage_has_root_issues = coverage
        .and_then(|coverage| coverage.provider_roots)
        .is_some_and(|roots| roots.partial > 0 || roots.excluded > 0 || roots.unknown > 0);
    let coverage_has_refresh_issues = coverage
        .is_some_and(|coverage| coverage.source_failures > 0 || coverage.rejected_records > 0);
    let (text, command) = if findings
        .iter()
        .any(|finding| finding.contains("(heartbeat_stale)"))
    {
        (
            "Inspect the daemon and retained work before retrying.",
            "ctx daemon status",
        )
    } else if refresh_failure.is_some() {
        (
            "Fix the refresh error above, then retry.",
            "ctx import --all",
        )
    } else if refresh_failed || coverage_has_refresh_issues {
        ("Re-run the bounded history refresh.", "ctx import --all")
    } else if coverage_has_root_issues {
        (
            "Inspect every provider location and its import status.",
            "ctx sources --all",
        )
    } else if findings
        .iter()
        .any(|finding| finding.contains("generation_not_published"))
    {
        ("Publish the first verified history index.", "ctx setup")
    } else if findings
        .iter()
        .any(|finding| finding.starts_with("semantic is unavailable"))
    {
        (
            "Inspect the semantic failure and its error detail.",
            "ctx semantic status",
        )
    } else if findings.iter().any(|finding| finding.contains(" pending ")) {
        ("Wait for history indexing to finish.", "ctx index watch")
    } else if findings.iter().any(|finding| finding.contains("upgrade")) {
        (
            "Inspect upgrade configuration and install health.",
            "ctx upgrade status",
        )
    } else {
        ("Re-run setup after resolving the issue above.", "ctx setup")
    };
    document.append(hint(context, Hint { text }, Some(Action { command })));
    document
}

fn is_derivative_refresh_finding(finding: &str) -> bool {
    [
        "history_epoch is unavailable (source_refresh_failed)",
        "history_epoch is unavailable (core_refresh_failed)",
        "lexical is unavailable (source_refresh_failed)",
        "lexical is unavailable (core_refresh_failed)",
        "lexical is unavailable (lexical_generation_unavailable)",
        "catalog is pending (catalog_publication_pending)",
        "catalog is pending (core_generation_pending)",
        "refresh is unavailable (core_refresh_failed)",
        "semantic is pending (lexical_generation_unavailable)",
    ]
    .contains(&finding)
}

pub(super) fn bounded_terminal_detail(detail: &str) -> String {
    let escaped = Span::text(detail).content().to_owned();
    if escaped.len() <= MAX_REFRESH_ERROR_BYTES {
        return escaped;
    }
    // Keep both the provider/context and the actionable cause at the tail of
    // an error chain. Long paths must not hide required/available resources.
    let mut end = (MAX_REFRESH_ERROR_BYTES - 3) / 2;
    while !escaped.is_char_boundary(end) {
        end -= 1;
    }
    let mut tail = escaped.len() - (MAX_REFRESH_ERROR_BYTES - 3 - end);
    while !escaped.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}...{}", &escaped[..end], &escaped[tail..])
}

struct HumanDoctorFinding {
    summary: String,
    detail: Option<String>,
}

fn humanize_doctor_finding(finding: &str) -> HumanDoctorFinding {
    let Some((component, state_and_reason)) = finding.split_once(" is ") else {
        return HumanDoctorFinding {
            summary: finding.to_owned(),
            detail: None,
        };
    };
    let Some((state, reason)) = state_and_reason
        .strip_suffix(')')
        .and_then(|value| value.rsplit_once(" ("))
    else {
        return HumanDoctorFinding {
            summary: finding.to_owned(),
            detail: None,
        };
    };
    let label = match component {
        "daemon" => "Daemon",
        "history_epoch" => "History",
        "lexical" => "Search index",
        "catalog" => "History source catalog",
        "refresh" => "History refresh",
        "semantic" => "Semantic search",
        _ => {
            return HumanDoctorFinding {
                summary: finding.to_owned(),
                detail: None,
            }
        }
    };
    let summary = match (state, reason) {
        (_, "heartbeat_stale") => "Daemon heartbeat is stale".to_owned(),
        ("pending", _) => format!("{label} is still preparing"),
        ("unavailable", _) => format!("{label} is unavailable"),
        (other, _) => format!("{label} is {}", other.replace('_', " ")),
    };
    let detail = match reason {
        "model_load_failed"
        | "model_acquisition_failed"
        | "model_integrity_failed"
        | "daemon_semantic_job_failed" => "The last semantic background operation failed.",
        "heartbeat_stale" => "The process is still running, but its heartbeat has not advanced. Work may be stalled; this does not prove the process is dead.",
        "catalog_publication_pending" => "Required local data is still being prepared.",
        "daemon_unavailable" => "The background history refresh service is not available.",
        "source_refresh_failed" | "core_refresh_failed" | "lexical_generation_unavailable" => {
            "Required local data is not available."
        }
        _ => "The component is not ready.",
    };
    HumanDoctorFinding {
        summary,
        detail: Some(detail.to_owned()),
    }
}

#[cfg(test)]
mod ui_tests {
    use ctx_history_read_application::{
        HistoryDataCoverage, HistoryHealthReport, HistoryRootCoverage,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn doctor_does_not_call_semantic_startup_failure_preparation() {
        let report = json!({
            "history_epoch": {"status": "ready"},
            "lexical": {"status": "ready"},
            "catalog": {"status": "ready"},
            "refresh": {"status": "ready"},
            "semantic": {"enabled": true, "status": "unavailable", "reason": "model_load_failed"},
        });
        let findings = source_epoch_findings(&report, true);
        let rendered = render_doctor_human(&context(120), &findings, None, None).render_plain();
        assert!(
            rendered.contains("Semantic search is unavailable"),
            "{rendered}"
        );
        assert!(
            rendered.contains("last semantic background operation failed"),
            "{rendered}"
        );
        assert!(rendered.contains("ctx semantic status"), "{rendered}");
        assert!(!rendered.contains("preparing"), "{rendered}");
    }
    use crate::{
        test_support::{assert_fits, strip_ansi},
        ui::{ColorMode, StreamKind, TestContext},
    };

    fn context_with_color(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn context(width: usize) -> RenderContext {
        context_with_color(width, ColorMode::Never)
    }

    fn partial_coverage() -> HistoryHealthReport {
        HistoryHealthReport {
            contributing_agent_histories: vec!["codex".to_owned()],
            provider_roots: Some(HistoryRootCoverage {
                included: 2,
                partial: 0,
                excluded: 0,
                unknown: 1,
            }),
            sessions: 3,
            messages: 100,
            tool_calls: 20,
            data: HistoryDataCoverage {
                processed: 1024,
                excluded: None,
            },
            source_failures: 0,
            rejected_records: 0,
        }
    }

    #[test]
    fn healthy_doctor_is_one_concise_outcome() {
        assert_eq!(
            render_doctor_human(&context(80), &[], None, None).render_plain(),
            "✓ No problems found\n"
        );
    }

    #[test]
    fn doctor_reports_stale_heartbeat_as_observation_not_process_death() {
        let report = json!({
            "history_epoch": {"status": "ready"},
            "lexical": {"status": "ready"},
            "catalog": {"status": "ready"},
            "refresh": {"status": "ready"},
            "daemon": {"running": true, "heartbeat_stale": true},
        });
        let findings = source_epoch_findings(&report, false);
        assert_eq!(findings, ["daemon is running (heartbeat_stale)"]);
        let rendered = render_doctor_human(&context(120), &findings, None, None).render_plain();
        assert!(rendered.contains("Daemon heartbeat is stale"), "{rendered}");
        assert!(rendered.contains("ctx daemon status"), "{rendered}");
        assert!(!rendered.contains("No problems found"), "{rendered}");
    }

    #[test]
    fn doctor_names_partial_coverage_without_repeating_status() {
        let coverage = partial_coverage();
        let rendered = render_doctor_human(&context(80), &[], Some(&coverage), None).render_plain();

        assert!(rendered.starts_with("! ctx found 1 issue\n"), "{rendered}");
        assert!(
            rendered.contains("History coverage is partial"),
            "{rendered}"
        );
        assert!(
            rendered.contains("1 provider root could not be assessed"),
            "{rendered}"
        );
        assert!(rendered.contains("ctx sources --all"), "{rendered}");
        assert!(!rendered.contains("Configuration"), "{rendered}");
        assert!(!rendered.contains("Sessions"), "{rendered}");
    }

    #[test]
    fn source_epoch_findings_keep_machine_component_diagnostics() {
        let base = json!({
            "history_epoch": {"status": "ready"},
            "lexical": {"status": "ready"},
            "catalog": {"status": "ready"},
            "semantic": {"status": "disabled"},
        });
        let mut rejections = base.clone();
        rejections["refresh"] = json!({"status": "ready"});
        assert!(source_epoch_findings(&rejections, false).is_empty());

        let mut failures = base;
        failures["refresh"] =
            json!({"status": "partial", "reason": "completed_with_source_failures"});
        assert_eq!(
            source_epoch_findings(&failures, false),
            vec!["refresh is partial (completed_with_source_failures)"],
        );
    }

    #[test]
    fn generic_findings_are_numbered_wrapped_and_do_not_loop_to_doctor() {
        let finding =
            "history configuration is unavailable; repair the local configuration".to_owned();
        for width in [32, 48, 80, 120] {
            let context = context(width);
            let document =
                render_doctor_human(&context, std::slice::from_ref(&finding), None, None);
            let rendered = document.render_plain();
            assert!(rendered.starts_with("! ctx found 1 issue\n"));
            assert!(rendered.contains("Issues\n[1]"));
            assert!(rendered.contains("ctx setup\n"));
            assert!(!rendered.contains("ctx doctor\n"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn refresh_component_cascade_collapses_to_one_actionable_issue() {
        let findings = [
            "history_epoch is unavailable (source_refresh_failed)",
            "lexical is unavailable (source_refresh_failed)",
            "catalog is pending (catalog_publication_pending)",
            "refresh is unavailable (core_refresh_failed)",
        ]
        .map(str::to_owned);

        for width in [32, 48, 80, 120] {
            let context = context(width);
            let document = render_doctor_human(&context, &findings, None, None);
            let rendered = document.render_plain();
            let flattened = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("! ctx found 1 issue\n"), "{rendered}");
            assert!(
                flattened.contains("History refresh needs repair"),
                "{rendered}"
            );
            assert!(
                flattened.contains("healthy prior history remains searchable"),
                "{rendered}"
            );
            assert!(rendered.contains("ctx import --all"), "{rendered}");
            assert!(!rendered.contains("source_refresh_failed"), "{rendered}");
            assert!(!rendered.contains("ctx status\n"), "{rendered}");
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn refresh_failure_is_one_root_issue_with_explicit_search_availability() {
        let findings = [
            "history_epoch is unavailable (source_refresh_failed)",
            "lexical is unavailable (source_refresh_failed)",
            "catalog is pending (catalog_publication_pending)",
            "refresh is unavailable (core_refresh_failed)",
        ]
        .map(str::to_owned);

        for (search, expected) in [
            (
                DoctorSearchAvailability::Available,
                "Available — healthy prior history remains searchable",
            ),
            (
                DoctorSearchAvailability::Unavailable,
                "Unavailable — no verified history is searchable",
            ),
        ] {
            let rendered = render_doctor_human(
                &context(80),
                &findings,
                None,
                Some(DoctorRefreshFailure {
                    detail: "Claude transcript repeats a stable event identity at lines 1 and 2"
                        .to_owned(),
                    search,
                    partial: false,
                }),
            )
            .render_plain();
            assert!(rendered.starts_with("! ctx found 1 issue\n"), "{rendered}");
            assert!(rendered.contains(expected), "{rendered}");
            assert_eq!(rendered.matches("History refresh failed").count(), 1);
            assert!(!rendered.contains("source catalog"), "{rendered}");
            assert!(rendered.contains("ctx import --all"), "{rendered}");
        }
    }

    #[test]
    fn refresh_failure_detail_is_control_escaped_and_byte_bounded() {
        let detail = format!("start\n\u{1b}[31m{}tail", "é".repeat(400));
        let bounded = bounded_terminal_detail(&detail);
        assert!(bounded.len() <= MAX_REFRESH_ERROR_BYTES);
        assert!(bounded.len() > MAX_REFRESH_ERROR_BYTES - 5);
        assert!(bounded.starts_with("start\\n\\x1b[31m"), "{bounded:?}");
        assert!(bounded.contains("..."), "{bounded:?}");
        assert!(bounded.ends_with("tail"), "{bounded:?}");
        assert!(!bounded.contains('\n'));
        assert!(!bounded.contains('\u{1b}'));
    }

    #[test]
    fn doctor_plain_output_equals_ansi_stripped_styled_output() {
        let coverage = partial_coverage();
        let context = context_with_color(80, ColorMode::Always);
        let document = render_doctor_human(&context, &[], Some(&coverage), None);
        assert_eq!(
            strip_ansi(&document.render(&context)),
            document.render_plain()
        );
    }
}
