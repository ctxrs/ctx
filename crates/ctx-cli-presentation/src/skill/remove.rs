use anyhow::{anyhow, Result};
use ctx_agent_application::{skill as application, ProductIdentity};
use ctx_agent_integrations::skill::SkillRemoveResult;
use serde_json::{json, Value};

use crate::{
    analytics::IntegrationTelemetry,
    ui::{
        diagnostic, fields, outcome, section, Action, Diagnostic, DiagnosticLevel, Document, Field,
        Outcome, OutcomeState, RenderContext, Ui,
    },
};

use super::{selection::remove_agent_selection, SkillRemoveArgs, BUNDLED_SKILL_NAME};

pub(super) fn run_remove(
    args: SkillRemoveArgs,
    context: &super::paths::PathContext,
    identity: ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let selection = remove_agent_selection(&args, context)?;
    let outcome = application::remove(selection, args.project, args.force, context)?;
    crate::integrations::apply_workflow_telemetry(outcome.telemetry, telemetry);
    let receipt = outcome.receipt;
    if args.format.is_json() {
        let output = json!({
            "skill": BUNDLED_SKILL_NAME,
            "scope": if receipt.project { "project" } else { "global" },
            "results": receipt.results.iter().map(remove_result_json).collect::<Vec<_>>(),
        });
        ui.write_stdout_bytes(format!("{output}\n").as_bytes())?;
    } else {
        ui.write_stdout(&render_remove_results(
            ui.stdout_context(),
            &receipt.results,
        ))?;
        if let Some(diagnostics) =
            render_remove_failures(ui.stderr_context(), identity, &receipt.results)
        {
            ui.write_stderr(&diagnostics)?;
        }
    }
    if receipt.failed == 0 {
        return Ok(());
    }
    if !args.format.is_json() {
        return Err(crate::rendered_cli_error());
    }
    Err(anyhow!(
        "failed to remove skill for {} target(s)",
        receipt.failed
    ))
}

fn remove_result_json(result: &SkillRemoveResult) -> Value {
    json!({
        "agent": result.target.agent.id(),
        "agent_display_name": result.target.agent.display_name(),
        "scope": result.target.scope.as_str(),
        "path": result.target.skill_dir,
        "success": result.success,
        "previous_status": result.previous_status.as_str(),
        "status": result.status.as_str(),
        "already_absent": result.already_absent,
        "removed": result.removed,
        "removed_current": result.removed_current,
        "removed_legacy": result.removed_legacy,
        "error": result.error,
    })
}

fn render_remove_results(context: &RenderContext, results: &[SkillRemoveResult]) -> Document {
    let all_absent = !results.is_empty() && results.iter().all(|result| result.already_absent);
    let all_success = !results.is_empty() && results.iter().all(|result| result.success);
    let title = if all_absent {
        "Agent skill is already absent"
    } else if all_success {
        "Agent skill removed"
    } else {
        "Agent skill needs attention"
    };
    let mut document = outcome(
        context,
        Outcome {
            state: if all_success {
                OutcomeState::Success
            } else {
                OutcomeState::Warning
            },
            title,
            detail: None,
        },
    );
    document.push_blank();
    document.append(fields(context, &[Field::new("Skill", BUNDLED_SKILL_NAME)]));

    let rows = results
        .iter()
        .map(|result| {
            let status = if result.already_absent {
                "absent"
            } else if result.success && result.removed {
                "removed"
            } else if result.success {
                "absent"
            } else {
                "skipped"
            };
            (
                status,
                format!(
                    "{} ({}) -> {}",
                    result.target.agent.display_name(),
                    result.target.scope.as_str(),
                    result.target.skill_dir.display()
                ),
            )
        })
        .collect::<Vec<_>>();
    let target_fields = rows
        .iter()
        .map(|(status, detail)| Field::new(status, detail))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Targets", fields(context, &target_fields)));
    document
}

fn render_remove_failures(
    context: &RenderContext,
    identity: ProductIdentity<'_>,
    results: &[SkillRemoveResult],
) -> Option<Document> {
    let mut document = Document::new();
    for result in results.iter().filter(|result| !result.success) {
        let summary = format!(
            "{} Agent Skill was not removed",
            result.target.agent.display_name()
        );
        let command = application::force_remove_command(identity, &result.target);
        if !document.is_empty() {
            document.push_blank();
        }
        document.append(diagnostic(
            context,
            Diagnostic {
                level: DiagnosticLevel::Warning,
                summary: &summary,
                detail: result.error.as_deref(),
                fields: &[],
                action: result
                    .force_required
                    .then_some(Action { command: &command }),
            },
        ));
    }
    (!document.is_empty()).then_some(document)
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write as _};

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext, Token};
    use ctx_agent_integrations::skill::{
        install_target, remove_target, single_target, PathContext, SkillAgentArg,
    };

    const PRODUCT: ProductIdentity<'static> = ProductIdentity {
        name: "ctx",
        version: "1.0.0-test",
    };

    fn render_context(kind: StreamKind, width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(kind, width).color(color))
    }

    fn semantic_command(document: &Document) -> String {
        document
            .lines()
            .iter()
            .flat_map(|line| line.spans())
            .filter(|span| span.token() == Token::Command)
            .map(|span| span.content())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    #[test]
    fn human_removed_and_absent_receipts_use_the_typed_ui() {
        let temp = tempfile::tempdir().unwrap();
        let path_context =
            PathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
        let target = single_target(SkillAgentArg::Universal, false, &path_context).unwrap();
        install_target(&target, false, true, PRODUCT.version).unwrap();
        let removed = remove_target(&target, false).unwrap();
        let absent = remove_target(&target, false).unwrap();

        for (result, expected) in [
            (removed, "Agent skill removed"),
            (absent, "Agent skill is already absent"),
        ] {
            let context = render_context(StreamKind::Stdout, 80, ColorMode::Never);
            let document = render_remove_results(&context, &[result]);
            assert!(document.render_plain().contains(expected));
            let styled_context = render_context(StreamKind::Stdout, 80, ColorMode::Always);
            let styled = document.render(&styled_context);
            assert_eq!(strip_ansi(&styled), document.render_plain());
        }
    }

    #[test]
    fn unowned_failure_offers_the_canonical_force_remove_action() {
        let temp = tempfile::tempdir().unwrap();
        let path_context =
            PathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
        let target = single_target(SkillAgentArg::Universal, true, &path_context).unwrap();
        fs::create_dir_all(&target.skill_dir).unwrap();
        fs::write(target.skill_dir.join("SKILL.md"), b"local edits\n").unwrap();
        let result = remove_target(&target, false).unwrap();

        for width in [32, 48, 80, 120] {
            let context = render_context(StreamKind::Stderr, width, ColorMode::Never);
            let document =
                render_remove_failures(&context, PRODUCT, std::slice::from_ref(&result)).unwrap();
            assert_eq!(
                semantic_command(&document),
                "ctx integrations remove skill --agent universal --project --force"
            );
            let normalized = document
                .render_plain()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(normalized.contains("Universal .agents Agent Skill was not removed"));
        }
    }
}
