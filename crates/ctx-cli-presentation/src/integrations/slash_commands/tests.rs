use std::{fs, path::PathBuf};

use super::*;
use crate::{
    test_support::{assert_fits, strip_ansi, SharedWriter},
    ui::{ColorMode, StreamKind, TestContext},
};
use tempfile::tempdir;

const PRODUCT: ProductIdentity<'static> = ProductIdentity {
    name: "ctx",
    version: "1.0.0-test",
};

fn render_context(width: usize, color: ColorMode) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
}

fn install_result(status: SlashCommandInstallStatus) -> InstallResult {
    InstallResult {
        agent: SlashCommandAgent::OpenCode,
        scope: Some(SlashCommandScope::Global),
        path: Some(PathBuf::from(
            "/tmp/config with spaces/opencode/commands/ctx.md",
        )),
        success: status != SlashCommandInstallStatus::Modified,
        previous_status: SlashCommandInstallStatus::Missing,
        status,
        already_installed: status == SlashCommandInstallStatus::Current,
        updated: status == SlashCommandInstallStatus::Stale,
        migrated: false,
        legacy_path: None,
        error: (status == SlashCommandInstallStatus::Modified)
            .then(|| "local command edits detected".to_owned()),
        note: None,
    }
}

#[test]
fn install_results_are_outcome_first_and_responsive() {
    let results = vec![install_result(SlashCommandInstallStatus::Current)];
    for width in [32, 48, 80, 120] {
        let context = render_context(width, ColorMode::Never);
        let document = render_install_results(&context, &results);
        let rendered = document.render_plain();
        let compact = rendered.split_whitespace().collect::<String>();
        let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(normalized.starts_with("✓ Slash-command integration is ready"));
        assert!(rendered.contains("Targets\n"));
        assert!(compact.contains("/tmp/configwithspaces/opencode/commands/ctx.md"));
        assert_fits(&document, &context);
    }
}

#[test]
fn no_detected_targets_is_actionable() {
    let context = render_context(48, ColorMode::Never);
    let rendered = render_install_results(&context, &[]).render_plain();
    assert!(rendered.starts_with("No separate slash-command targets detected\n"));
    assert!(rendered.contains("Next\n  ctx integrations install skill\n"));
}

#[test]
fn modified_target_has_a_force_recovery_command() {
    let context = render_context(48, ColorMode::Never);
    let document = render_install_results(
        &context,
        &[install_result(SlashCommandInstallStatus::Modified)],
    );
    let rendered = document.render_plain();
    assert!(rendered.starts_with("! 1 slash-command target needs attention\n"));
    assert!(!rendered.contains("local command edits detected"));

    let diagnostic = render_install_failures(
        &context,
        PRODUCT,
        &[install_result(SlashCommandInstallStatus::Modified)],
    )
    .unwrap()
    .render_plain();
    let compact = diagnostic.split_whitespace().collect::<String>();
    assert!(diagnostic.contains("local command edits detected"));
    assert!(compact.contains("ctxintegrationsinstallslash-command--agentopencode--force"));
    assert_fits(&document, &context);
}

#[test]
fn failed_target_details_and_recovery_are_written_to_stderr() {
    let temp = tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
    let command_path = temp
        .path()
        .join(".gemini")
        .join("commands")
        .join("ctx.toml");
    fs::create_dir_all(command_path.parent().unwrap()).unwrap();
    fs::write(command_path, "prompt = 'local'\n").unwrap();

    let stdout = SharedWriter::default();
    let stdout_copy = stdout.clone();
    let stderr = SharedWriter::default();
    let stderr_copy = stderr.clone();
    let stdout_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
    let stderr_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
    let mut ui = Ui::with_writers(stdout, stdout_context, stderr, stderr_context);
    let mut telemetry = IntegrationTelemetry::default();
    let result = run_install(
        SlashCommandInstallArgs {
            agent: vec![SlashCommandAgentArg::GeminiCli],
            all_agents: false,
            project: true,
            format: JsonOutputFormat::Text,
            force: false,
        },
        &context,
        PRODUCT,
        &mut telemetry,
        &mut ui,
    );

    assert!(result.is_err());
    let stdout = stdout_copy.text();
    let stderr = stderr_copy.text();
    assert!(stdout.contains("Targets"));
    assert!(!stdout.contains("local command edits detected"));
    assert!(stderr.contains("local command edits detected"));
    assert!(stderr.contains("--agent gemini-cli --project --force"));
}

#[test]
fn install_plain_output_matches_ansi_stripped_output() {
    let context = render_context(80, ColorMode::Always);
    let document = render_install_results(
        &context,
        &[install_result(SlashCommandInstallStatus::Current)],
    );
    assert_eq!(
        strip_ansi(&document.render(&context)),
        document.render_plain()
    );
}
