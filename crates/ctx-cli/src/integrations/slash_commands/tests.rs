use std::{
    io::{self, Write as _},
    sync::{Arc, Mutex},
};

use unicode_width::UnicodeWidthStr as _;

use super::*;
use crate::ui::{ColorMode, StreamKind, TestContext};
use tempfile::tempdir;

fn render_context(width: usize, color: ColorMode) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
}

fn install_result(status: SlashCommandInstallStatus) -> InstallResult {
    InstallResult {
        agent: SlashCommandAgentArg::OpenCode,
        scope: Some(SlashCommandScope::Global),
        path: Some(PathBuf::from(
            "/tmp/config with spaces/opencode/commands/ctx-history.md",
        )),
        success: status != SlashCommandInstallStatus::Modified,
        previous_status: SlashCommandInstallStatus::Missing,
        status,
        already_installed: status == SlashCommandInstallStatus::Current,
        updated: status == SlashCommandInstallStatus::Stale,
        error: (status == SlashCommandInstallStatus::Modified)
            .then(|| "local command edits detected".to_owned()),
        note: None,
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
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn selected_agents_default_to_detected_file_based_targets() {
    let temp = tempdir().unwrap();
    let xdg = temp.path().join("xdg");
    fs::create_dir_all(xdg.join("opencode")).unwrap();
    fs::create_dir_all(xdg.join("mimocode")).unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned())
        .with_xdg_config_home(xdg);
    let args = SlashCommandInstallArgs {
        agent: Vec::new(),
        all_agents: false,
        project: false,
        format: JsonOutputFormat::Json,
        force: false,
    };

    assert_eq!(
        selected_agents(&args, &context),
        vec![
            SlashCommandAgentArg::OpenCode,
            SlashCommandAgentArg::MiMoCode
        ]
    );
}

#[test]
fn opencode_install_is_idempotent_and_refreshes_stale_owned_file() {
    let temp = tempdir().unwrap();
    let xdg = temp.path().join("xdg");
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned())
        .with_xdg_config_home(xdg.clone());
    let target = match SlashCommandAgentArg::OpenCode.install_plan(false, &context) {
        SlashCommandPlan::File(target) => target,
        _ => panic!("expected file target"),
    };

    let first = install_file_target(&target, false).unwrap();
    assert_eq!(first.previous_status, SlashCommandInstallStatus::Missing);
    assert!(!first.already_installed);
    assert!(xdg
        .join("opencode")
        .join("commands")
        .join("ctx-history.md")
        .exists());

    let second = install_file_target(&target, false).unwrap();
    assert_eq!(second.previous_status, SlashCommandInstallStatus::Current);
    assert!(second.already_installed);

    let old_body = "---\ndescription: old\n---\n\nold\n";
    fs::write(target.command_path(), old_body).unwrap();
    let mut metadata = SlashCommandMetadata::current(&target);
    metadata
        .files
        .insert(target.filename.clone(), sha256_hex(old_body.as_bytes()));
    fs::write(
        target.base_dir.join(METADATA_FILE),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();

    let refreshed = install_file_target(&target, false).unwrap();
    assert_eq!(refreshed.previous_status, SlashCommandInstallStatus::Stale);
    assert!(refreshed.updated);
    assert!(fs::read_to_string(target.command_path())
        .unwrap()
        .contains("Search local agent history with ctx"));
}

#[test]
fn modified_command_requires_force() {
    let temp = tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
    let target = match SlashCommandAgentArg::GeminiCli.install_plan(true, &context) {
        SlashCommandPlan::File(target) => target,
        _ => panic!("expected file target"),
    };
    fs::create_dir_all(&target.base_dir).unwrap();
    fs::write(target.command_path(), "prompt = 'local'\n").unwrap();

    let skipped = install_file_target(&target, false).unwrap();
    assert!(!skipped.success);
    assert_eq!(skipped.previous_status, SlashCommandInstallStatus::Modified);
    assert!(fs::read_to_string(target.command_path())
        .unwrap()
        .contains("local"));

    let forced = install_file_target(&target, true).unwrap();
    assert!(forced.success);
    assert_eq!(forced.previous_status, SlashCommandInstallStatus::Modified);
    assert!(fs::read_to_string(target.command_path())
        .unwrap()
        .contains("{{args}}"));
}

#[test]
fn skill_only_agents_do_not_write_codex_prompts() {
    let temp = tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
    let result = install_plan(
        SlashCommandAgentArg::Codex.install_plan(false, &context),
        false,
    )
    .unwrap();

    assert_eq!(result.status, SlashCommandInstallStatus::SkillOnly);
    assert!(!temp.path().join(".codex").join("prompts").exists());
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
        assert!(compact.contains("/tmp/configwithspaces/opencode/commands/ctx-history.md"));
        assert_fits(&document, &context);
    }
}

#[test]
fn no_detected_targets_is_actionable() {
    let context = render_context(48, ColorMode::Never);
    let rendered = render_install_results(&context, &[]).render_plain();
    assert!(rendered.starts_with("No separate slash-command targets detected\n"));
    assert!(rendered.contains("Next\n  ctx integrations install skills\n"));
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
        &[install_result(SlashCommandInstallStatus::Modified)],
    )
    .unwrap()
    .render_plain();
    let compact = diagnostic.split_whitespace().collect::<String>();
    assert!(diagnostic.contains("local command edits detected"));
    assert!(compact.contains("ctxintegrationsinstallslash-commands--agentopencode--force"));
    assert_fits(&document, &context);
}

#[test]
fn failed_target_details_and_recovery_are_written_to_stderr() {
    let temp = tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
    let target = match SlashCommandAgentArg::GeminiCli.install_plan(true, &context) {
        SlashCommandPlan::File(target) => target,
        _ => panic!("expected file target"),
    };
    fs::create_dir_all(&target.base_dir).unwrap();
    fs::write(target.command_path(), "prompt = 'local'\n").unwrap();

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
        &mut telemetry,
        &mut ui,
    );

    assert!(result.is_err());
    let stdout = stdout_copy.text();
    let stderr = stderr_copy.text();
    assert!(stdout.contains("Targets"));
    assert!(!stdout.contains("local command edits detected"));
    assert!(stderr.contains("local command edits detected"));
    assert!(stderr.contains("--agent gemini-cli --force"));
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
