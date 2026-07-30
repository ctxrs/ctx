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

    let references = (1..=findings.len())
        .map(|index| index.to_string())
        .collect::<Vec<_>>();
    let evidence = references
        .iter()
        .zip(findings)
        .map(|(reference, finding)| Evidence {
            reference,
            summary: finding,
            detail: None,
        })
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Issues", evidence_list(context, &evidence)));
    document.push_blank();
    document.append(hint(
        context,
        Hint {
            text: "Resolve the issues above, then check again.",
        },
        Some(Action {
            command: "ctx doctor",
        }),
    ));
    document
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
}
