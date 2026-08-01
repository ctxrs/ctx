use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use crate::analytics::{count_bucket, DoctorTelemetry};
use crate::config::AppConfig;
use crate::output::print_json;
use crate::semantic::source_epoch_status_report;
use crate::ui::{
    evidence_list, fields, hint, outcome, section, Action, Document, Evidence, Field, Hint,
    Outcome, OutcomeState, RenderContext, Ui,
};
use crate::DoctorArgs;

pub(crate) fn run_doctor(
    args: DoctorArgs,
    data_root: PathBuf,
    telemetry: &mut DoctorTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let json_output = args.format.is_json();
    let mut findings = Vec::new();
    if !data_root.exists() {
        findings.push(format!("data root does not exist: {}", data_root.display()));
    }
    let config = AppConfig::load(&data_root)?;
    let source = source_epoch_status_report(&data_root, &config)?;
    let pro = crate::pro::lifecycle_status_json(&data_root);
    for (name, required) in [
        ("history_epoch", true),
        ("lexical", true),
        ("catalog", true),
        ("resolver", true),
        ("relational", true),
        ("semantic", config.semantic_search_enabled()),
        (
            "pro_projection",
            pro.get("installed").and_then(serde_json::Value::as_bool) == Some(true),
        ),
    ] {
        if !required {
            continue;
        }
        let component = &source.report[name];
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
    let daemon = source.report["daemon"].clone();
    let upgrade_diagnostics = crate::upgrade::upgrade_diagnostics(&config);
    findings.extend(upgrade_diagnostics.findings);
    let upgrade = upgrade_diagnostics.report;
    if pro["installed"].as_bool() == Some(true) {
        if let Some(code @ ("helper_upgrade_required" | "protocol_mismatch")) =
            pro["error_code"].as_str()
        {
            findings.push(format!(
                "ctx Pro helper is incompatible ({code}); run `ctx pro`"
            ));
        } else if let Some(code @ ("key_store_unavailable" | "key_store_locked")) =
            pro["error_code"].as_str()
        {
            findings.push(format!(
                "ctx Pro key store is unavailable ({code}); unlock or repair the already selected secure key store, then run `ctx pro`; a fresh installation can select the owner-private local vault only when the native store is genuinely unavailable, and ctx never downgrades existing state"
            ));
        } else if pro["error_code"].as_str() == Some("corrupt_graph") {
            findings.push(
                "ctx Pro graph needs repair; run `ctx pro` or reinstall with `ctx pro uninstall --delete-data`"
                    .to_owned(),
            );
        }
    }
    telemetry.finding_count = Some(count_bucket(findings.len() as u64));
    telemetry.healthy = Some(findings.is_empty());
    if json_output {
        print_json(json!({
            "schema_version": 1,
            "ok": findings.is_empty(),
            "findings": findings,
            "source_epoch": source.report,
            "daemon": daemon,
            "upgrade": upgrade,
            "pro": pro,
        }))?;
    } else {
        let document = render_doctor_human(
            ui.stdout_context(),
            config.auto_upgrade_mode().as_str(),
            &findings,
        );
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn render_doctor_human(
    context: &RenderContext,
    automatic_upgrades: &str,
    findings: &[String],
) -> Document {
    let title = match findings.len() {
        0 => "No problems found".to_owned(),
        1 => "ctx found 1 issue".to_owned(),
        count => format!("ctx found {count} issues"),
    };
    let mut document = outcome(
        context,
        Outcome {
            state: if findings.is_empty() {
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
    if findings.is_empty() {
        return document;
    }

    let human_findings = findings
        .iter()
        .map(|finding| humanize_doctor_finding(finding))
        .collect::<Vec<_>>();
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
        finding.contains("(source_refresh_failed)")
            || finding.starts_with("resolver is unavailable ")
    });
    document.append(hint(
        context,
        Hint {
            text: if refresh_failed {
                "Check the history refresh service."
            } else {
                "Resolve the issues above, then check again."
            },
        },
        Some(Action {
            command: if refresh_failed {
                "ctx daemon status"
            } else {
                "ctx doctor"
            },
        }),
    ));
    document
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
        "resolver" => "History refresh service",
        "relational" => "Session view",
        "semantic" => "Semantic search",
        "pro_projection" => "ctx Pro index",
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
        "daemon_unavailable" | "resolver_unavailable" => {
            "The background history refresh service is not available."
        }
        "source_refresh_failed" | "lexical_generation_unavailable" => {
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
        let rendered = render_doctor_human(&context, "apply", &[]).render_plain();
        assert_eq!(
            rendered,
            "✓ No problems found\n\nConfiguration\nAutomatic upgrades  apply\n"
        );
    }

    #[test]
    fn findings_are_numbered_wrapped_and_actionable() {
        let finding = "ctx Pro key store is unavailable; unlock or repair the already selected secure key store, then run `ctx pro`".to_owned();
        for width in [32, 48, 80, 120] {
            let context = context(width);
            let document = render_doctor_human(&context, "off", std::slice::from_ref(&finding));
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
            "resolver is unavailable (daemon_unavailable)",
            "relational is unavailable (lexical_generation_unavailable)",
        ]
        .map(str::to_owned);

        for width in [32, 48, 80, 120] {
            let context = context(width);
            let document = render_doctor_human(&context, "apply", &findings);
            let rendered = document.render_plain();
            let flattened = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("! ctx found 5 issues\n"));
            for expected in [
                "History is unavailable",
                "Search index is unavailable",
                "History source catalog is still preparing",
                "History refresh service is unavailable",
                "Session view is unavailable",
                "Check the history refresh service.",
                "ctx daemon status",
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
                "daemon_unavailable",
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
}
