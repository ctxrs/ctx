use std::{
    ffi::OsString,
    io,
    sync::{Arc, Mutex},
};

use unicode_width::UnicodeWidthStr as _;

use super::*;
use crate::ui::{StreamKind, TestContext};

#[derive(Clone, Default)]
struct SharedBytes(Arc<Mutex<Vec<u8>>>);

impl SharedBytes {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}

impl io::Write for SharedBytes {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("shared test writer was poisoned"))?
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn error_and_arguments(arguments: &[&str]) -> (clap::Error, Vec<OsString>) {
    let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
    let mut error = Cli::try_parse_from(arguments.iter().cloned()).unwrap_err();
    attach_value_validation_usage(&mut error, &arguments);
    (error, arguments)
}

fn human_output(arguments: &[&str], width: usize) -> String {
    let (error, arguments) = error_and_arguments(arguments);
    let context = RenderContext::for_test(
        TestContext::tty(StreamKind::Stderr, width).color(ColorMode::Never),
    );
    human_clap_document(&error, &arguments, &context)
        .unwrap_or_else(|| {
            panic!(
                "assigned parse error should have human rendering: kind={:?} leaf={:?}",
                error.kind(),
                leaf_bin_name(&arguments)
            )
        })
        .render_plain()
}

fn normalized(output: &str) -> String {
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn assert_width_safe(output: &str, width: usize, allowed_overwidth: &[&str]) {
    for line in output.lines() {
        if allowed_overwidth.contains(&line) {
            continue;
        }
        assert!(
            line.width() < width,
            "line width {} is not below {width}: {line:?}\n{output}",
            line.width()
        );
    }
}

#[test]
fn root_errors_wrap_at_terminal_width_with_public_ctx_usage() {
    for width in [32, 80] {
        let missing = human_output(&["/tmp/copied-ctx"], width);
        assert!(missing.contains("A ctx command is required"), "{missing}");
        assert!(missing.contains("ctx [OPTIONS] <COMMAND>"), "{missing}");
        assert!(missing.contains("ctx --help"), "{missing}");
        assert!(!missing.contains("/tmp/copied-ctx"), "{missing}");
        assert!(!missing.contains("[subcommands:"), "{missing}");
        assert_width_safe(&missing, width, &[]);

        let invalid = human_output(&["/tmp/copied-ctx", "frobnicate"], width);
        let normalized = normalized(&invalid);
        assert!(invalid.contains("unrecognized subcommand"), "{invalid}");
        assert!(invalid.contains("frobnicate"), "{invalid}");
        assert!(
            normalized.contains("A similar subcommand exists: 'locate'."),
            "{invalid}"
        );
        assert!(invalid.contains("ctx [OPTIONS] <COMMAND>"), "{invalid}");
        assert!(!invalid.contains("/tmp/copied-ctx"), "{invalid}");
        assert_width_safe(&invalid, width, &[]);
    }
}

#[test]
fn value_validation_diagnostics_wrap_at_terminal_width() {
    for width in [32, 80] {
        for (arguments, expected) in [
            (
                &["ctx", "sources", "--provider", "definitely-not-a-provider"][..],
                "unknown provider",
            ),
            (
                &["ctx", "index", "watch", "--interval-seconds", "0"][..],
                "seconds must be between 1 and 86400",
            ),
            (
                &["ctx", "daemon", "run", "--loop-interval-seconds", "0"][..],
                "daemon loop interval seconds must be between 1 and 3600",
            ),
        ] {
            let output = human_output(arguments, width);
            let normalized = normalized(&output);
            assert!(
                normalized.contains("invalid value '0'")
                    || normalized.contains("invalid value 'definitely-not-a-provider'"),
                "{output}"
            );
            assert!(normalized.contains(expected), "{output}");
            assert!(output.contains("Usage"), "{output}");
            assert!(output.contains("ctx "), "{output}");
            assert_width_safe(&output, width, &[]);
        }
    }
}

#[test]
fn provider_validation_promotes_the_copyable_recovery_action() {
    let output = human_output(&["ctx", "sources", "--provider", "unknown"], 100);
    assert!(
        output.contains("unknown provider \"unknown\"; examples:"),
        "{output}"
    );
    assert!(!output.contains("run `ctx"), "{output}");
    assert_eq!(output.matches("ctx sources --all").count(), 1, "{output}");
    assert!(output.contains("Next\n  ctx sources --all\n"), "{output}");
    assert_width_safe(&output, 100, &[]);
}

#[test]
fn removed_idle_exit_has_persistent_foreground_recovery() {
    const RECOVERY: &str = "  ctx daemon run --help";
    for arguments in [
        &["ctx", "daemon", "run", "--idle-exit-seconds", "60"][..],
        &["ctx", "daemon", "run", "--idle-exit-seconds=60"][..],
    ] {
        for width in [32, 80] {
            let output = human_output(arguments, width);
            let normalized = normalized(&output);
            assert!(
                normalized.contains("The --idle-exit-seconds option has been removed"),
                "{output}"
            );
            assert!(
                normalized.contains("persistent foreground worker"),
                "{output}"
            );
            assert!(normalized.contains("has no idle timeout"), "{output}");
            assert!(output.contains(RECOVERY), "{output}");
            assert_width_safe(&output, width, &[]);
        }
    }
}

#[test]
fn removed_include_subagents_explains_the_all_agent_default() {
    const RECOVERY: &str = "  ctx search --help";
    for width in [32, 80] {
        let output = human_output(&["ctx", "search", "incident", "--include-subagents"], width);
        let normalized = normalized(&output);
        assert!(
            normalized.contains("The --include-subagents option has been removed"),
            "{output}"
        );
        assert!(
            normalized.contains("ctx search already includes primary and subagent sessions"),
            "{output}"
        );
        assert!(
            normalized.contains("Use --primary-only only when you want to exclude subagent work"),
            "{output}"
        );
        assert!(output.contains(RECOVERY), "{output}");
        assert!(!output.contains("--include-current-session"), "{output}");
        assert_width_safe(&output, width, &[]);
    }
}

#[test]
fn retired_once_recommends_foreground_import() {
    const RECOVERY: &str = "  ctx import --help";
    for width in [32, 80] {
        let output = human_output(&["ctx", "daemon", "run", "--once"], width);
        let normalized = normalized(&output);
        assert!(
            normalized.contains("The --once option has been retired"),
            "{output}"
        );
        assert!(
            normalized.contains("ctx import for foreground maintenance convergence"),
            "{output}"
        );
        assert!(output.contains(RECOVERY), "{output}");
        assert!(!output.contains("--force"), "{output}");
        assert!(!output.contains("--idle-exit-seconds"), "{output}");
        assert_width_safe(&output, width, &[]);
    }
}

#[test]
fn machine_parse_errors_remain_raw_clap_bytes() {
    for (arguments, expected_fragment) in [
        (
            &[
                "ctx",
                "search",
                "incident",
                "--include-subagents",
                "--format=json",
            ][..],
            "unexpected argument '--include-subagents'",
        ),
        (
            &["ctx", "daemon", "run", "--once", "--format=json"][..],
            "--force",
        ),
        (
            &[
                "ctx",
                "daemon",
                "run",
                "--idle-exit-seconds=60",
                "--format=json",
            ][..],
            "unexpected argument '--idle-exit-seconds'",
        ),
        (
            &["ctx", "sources", "--provider", "unknown", "--format=json"][..],
            "run `ctx sources --all` to inspect every supported provider location",
        ),
        (
            &["ctx", "frobnicate", "--format=json"][..],
            "unrecognized subcommand 'frobnicate'",
        ),
    ] {
        let (error, arguments) = error_and_arguments(arguments);
        let expected = error.to_string();
        assert!(expected.contains(expected_fragment), "{expected}");

        let stderr = SharedBytes::default();
        let stderr_copy = stderr.clone();
        let mut ui = Ui::with_writers(
            SharedBytes::default(),
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Never)),
            stderr,
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr).color(ColorMode::Never)),
        );
        write_adapted_clap_output(&error, &arguments, true, &mut ui).unwrap();
        ui.flush().unwrap();

        assert_eq!(String::from_utf8(stderr_copy.bytes()).unwrap(), expected);
    }
}

#[test]
fn human_parse_errors_use_the_selected_styled_stderr() {
    let (error, arguments) = error_and_arguments(&["ctx", "sources", "--provider", "unknown"]);
    let stderr = SharedBytes::default();
    let stderr_copy = stderr.clone();
    let mut ui = Ui::with_writers(
        SharedBytes::default(),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Always)),
        stderr,
        RenderContext::for_test(TestContext::pipe(StreamKind::Stderr).color(ColorMode::Always)),
    );
    write_adapted_clap_output(&error, &arguments, false, &mut ui).unwrap();
    ui.flush().unwrap();

    let rendered = String::from_utf8(stderr_copy.bytes()).unwrap();
    assert!(rendered.contains('\u{1b}'), "{rendered:?}");
    let mut stripped = anstream::StripStream::new(Vec::new());
    io::Write::write_all(&mut stripped, rendered.as_bytes()).unwrap();
    let plain = String::from_utf8(stripped.into_inner()).unwrap();
    assert!(plain.contains("unknown provider"), "{plain}");
    assert!(plain.contains("ctx sources [OPTIONS]"), "{plain}");
}

fn rendered_help(path: &[&str], width: usize) -> (String, String) {
    let mut root = Cli::command().term_width(width);
    root.build();
    let mut command = &mut root;
    for segment in path {
        command = command
            .find_subcommand_mut(segment)
            .unwrap_or_else(|| panic!("missing help path {path:?}"));
    }
    let rendered = command.render_long_help();
    let plain = crate::ui::trim_terminal_line_ends(&rendered.to_string());
    let styled = rendered.ansi().to_string();
    (plain, crate::ui::trim_terminal_line_ends(&styled))
}

#[test]
fn clap_help_has_no_authored_trailing_cells_at_supported_widths() {
    for width in [32, 80, 100, 120] {
        for path in [&[][..], &["search"][..], &["sources"][..]] {
            let (plain, styled) = rendered_help(path, width);
            assert!(plain
                .split_whitespace()
                .collect::<String>()
                .contains("Usage:ctx"));
            assert!(
                plain
                    .lines()
                    .all(|line| !line.ends_with([' ', '\t'])
                        && (width < 80 || line.width() <= width)),
                "help path {path:?} retained trailing cells or overflowed width {width}:\n{plain}"
            );
            assert!(
                styled.contains('\u{1b}'),
                "help path {path:?} was not styled"
            );
            assert_eq!(
                anstream::adapter::strip_str(&styled).to_string(),
                plain,
                "styled/plain mismatch for help path {path:?} at width {width}"
            );
        }
    }
}

#[test]
fn affected_help_paths_trim_line_ends_in_the_human_clap_pipeline() {
    let arguments = &["ctx", "sources", "--help"][..];
    let (error, os_arguments) = error_and_arguments(arguments);
    let stdout = SharedBytes::default();
    let stdout_copy = stdout.clone();
    let mut ui = Ui::with_writers(
        stdout,
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Always)),
        SharedBytes::default(),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stderr).color(ColorMode::Always)),
    );
    write_adapted_clap_output(&error, &os_arguments, false, &mut ui).unwrap();
    ui.flush().unwrap();

    let styled = String::from_utf8(stdout_copy.bytes()).unwrap();
    let plain = anstream::adapter::strip_str(&styled).to_string();
    assert!(styled.contains('\u{1b}'), "{arguments:?}");
    assert!(plain.contains("Usage: ctx"), "{arguments:?}:\n{plain}");
    assert!(
        plain
            .lines()
            .all(|line| !line.ends_with([' ', '\t']) && line.width() <= 100),
        "{arguments:?}:\n{plain}"
    );
}
