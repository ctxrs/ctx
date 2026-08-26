use serde_json::Value;

use crate::ui::{
    evidence_list, fields, hint, outcome, section, Action, Document, Evidence, Field, Hint,
    Outcome, OutcomeState, RenderContext, Span,
};

const MAX_REFRESH_ERROR_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorSearchAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Copy)]
pub struct DoctorRefreshFailure<'a> {
    pub detail: &'a str,
    pub search: DoctorSearchAvailability,
}

pub fn source_epoch_findings(report: &Value, semantic_required: bool) -> Vec<String> {
    let mut findings = Vec::new();
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
    automatic_upgrades: &str,
    findings: &[String],
    rejected_records: u64,
    refresh_failure: Option<DoctorRefreshFailure<'_>>,
) -> Document {
    let mut human_findings = findings
        .iter()
        .filter(|finding| refresh_failure.is_none() || !is_derivative_refresh_finding(finding))
        .map(|finding| humanize_doctor_finding(finding))
        .collect::<Vec<_>>();
    if let Some(failure) = refresh_failure {
        human_findings.insert(
            0,
            HumanDoctorFinding {
                summary: "History refresh failed".to_owned(),
                detail: Some(bounded_terminal_detail(failure.detail)),
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
    document.push_blank();
    document.append(section(
        "Configuration",
        fields(
            context,
            &[Field::new("Automatic upgrades", automatic_upgrades)],
        ),
    ));
    if rejected_records > 0 || refresh_failure.is_some() {
        let rejected_records = rejected_records.to_string();
        let mut history_fields = Vec::new();
        if let Some(failure) = refresh_failure {
            history_fields.push(Field::new(
                "Search",
                match failure.search {
                    DoctorSearchAvailability::Available => {
                        "Available — last verified index remains searchable"
                    }
                    DoctorSearchAvailability::Unavailable => {
                        "Unavailable — no verified index is searchable"
                    }
                },
            ));
        }
        if rejected_records != "0" {
            history_fields.push(Field::new("Skipped records", &rejected_records));
        }
        document.push_blank();
        document.append(section("History", fields(context, &history_fields)));
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
    let refresh_failed = findings.iter().any(|finding| {
        finding.contains("(source_refresh_failed)") || finding.contains("(core_refresh_failed)")
    });
    document.append(hint(
        context,
        Hint {
            text: if refresh_failure.is_some() {
                "Fix the refresh error above, then retry."
            } else if refresh_failed {
                "Check the history refresh service."
            } else {
                "Resolve the issues above, then check again."
            },
        },
        Some(Action {
            command: if refresh_failure.is_some() {
                "ctx import --all"
            } else if refresh_failed {
                "ctx status"
            } else {
                "ctx doctor"
            },
        }),
    ));
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

fn bounded_terminal_detail(detail: &str) -> String {
    let escaped = Span::text(detail).content().to_owned();
    if escaped.len() <= MAX_REFRESH_ERROR_BYTES {
        return escaped;
    }
    let mut end = MAX_REFRESH_ERROR_BYTES - 3;
    while !escaped.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &escaped[..end])
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
    let summary = match state {
        "pending" => format!("{label} is still preparing"),
        "unavailable" => format!("{label} is unavailable"),
        other => format!("{label} is {}", other.replace('_', " ")),
    };
    let detail = match reason {
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
    use serde_json::json;
    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let width = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            assert!(line.width() <= width, "{line:?} exceeded {width} columns");
        }
    }

    #[test]
    fn healthy_doctor_is_concise_and_outcome_first() {
        let context = context(80);
        let rendered = render_doctor_human(&context, "apply", &[], 0, None).render_plain();
        assert_eq!(
            rendered,
            "✓ No problems found\n\nConfiguration\nAutomatic upgrades  apply\n"
        );
    }

    #[test]
    fn healthy_doctor_presents_record_rejections_as_skipped_records() {
        let rendered = render_doctor_human(&context(80), "apply", &[], 2, None).render_plain();

        assert!(rendered.starts_with("✓ No problems found\n"), "{rendered}");
        assert!(
            rendered.contains("History\nSkipped records  2\n"),
            "{rendered}"
        );
        assert!(!rendered.contains("issue"), "{rendered}");
    }

    #[test]
    fn doctor_ignores_record_diagnostics_but_flags_source_failures() {
        let base = json!({
            "history_epoch": {"status": "ready"},
            "lexical": {"status": "ready"},
            "catalog": {"status": "ready"},
            "semantic": {"status": "disabled"},
        });
        let mut rejections = base.clone();
        rejections["refresh"] = json!({
            "status": "ready",
            "outcome": "completed_with_rejections",
            "current": {"current_rejected_records": 1},
        });
        assert!(source_epoch_findings(&rejections, false).is_empty());

        for outcome in [
            "completed_with_source_failures",
            "completed_with_rejections_and_source_failures",
        ] {
            let mut failures = base.clone();
            failures["refresh"] = json!({"status": "partial", "reason": outcome});
            assert_eq!(
                source_epoch_findings(&failures, false),
                vec![format!("refresh is partial ({outcome})")],
            );
        }
    }

    #[test]
    fn findings_are_numbered_wrapped_and_actionable() {
        let finding = "history configuration is unavailable; repair the local configuration, then run `ctx doctor`".to_owned();
        for width in [32, 48, 80, 120] {
            let context = context(width);
            let document =
                render_doctor_human(&context, "off", std::slice::from_ref(&finding), 0, None);
            let rendered = document.render_plain();
            assert!(rendered.starts_with("! ctx found 1 issue\n"));
            assert!(rendered.contains("Issues\n[1]"));
            assert!(rendered.contains("ctx doctor\n"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn failed_source_findings_are_human_and_recover_through_daemon_status() {
        let findings = [
            "history_epoch is unavailable (source_refresh_failed)",
            "lexical is unavailable (source_refresh_failed)",
            "catalog is pending (catalog_publication_pending)",
            "refresh is unavailable (core_refresh_failed)",
        ]
        .map(str::to_owned);

        for width in [32, 48, 80, 120] {
            let context = context(width);
            let document = render_doctor_human(&context, "apply", &findings, 0, None);
            let rendered = document.render_plain();
            let flattened = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("! ctx found 4 issues\n"));
            for expected in [
                "History is unavailable",
                "Search index is unavailable",
                "History source catalog is still preparing",
                "History refresh is unavailable",
                "Check the history refresh service.",
                "ctx status",
            ] {
                assert!(
                    flattened.contains(expected),
                    "missing {expected:?}: {rendered}"
                );
            }
            for internal in [
                "history_epoch",
                "source_refresh_failed",
                "catalog_publication_pending",
                "lexical_generation_unavailable",
            ] {
                assert!(
                    !rendered.contains(internal),
                    "leaked {internal:?}: {rendered}"
                );
            }
            assert!(!rendered.contains("ctx doctor\n"), "{rendered}");
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
                "Available — last verified index remains searchable",
            ),
            (
                DoctorSearchAvailability::Unavailable,
                "Unavailable — no verified index is searchable",
            ),
        ] {
            let rendered = render_doctor_human(
                &context(80),
                "apply",
                &findings,
                0,
                Some(DoctorRefreshFailure {
                    detail: "Claude transcript repeats a stable event identity at lines 1 and 2",
                    search,
                }),
            )
            .render_plain();
            assert!(rendered.starts_with("! ctx found 1 issue\n"), "{rendered}");
            assert!(rendered.contains(expected), "{rendered}");
            assert_eq!(rendered.matches("History refresh failed").count(), 1);
            assert!(
                !rendered.contains("Search index is unavailable"),
                "{rendered}"
            );
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
        assert!(bounded.ends_with("..."), "{bounded:?}");
        assert!(!bounded.contains('\n'));
        assert!(!bounded.contains('\u{1b}'));
    }
}
