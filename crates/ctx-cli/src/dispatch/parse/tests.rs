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

        let invalid = human_output(&["/tmp/copied-ctx", "definitely-not-a-command"], width);
        assert!(invalid.contains("unrecognized subcommand"), "{invalid}");
        assert!(invalid.contains("definitely-not-a-command"), "{invalid}");
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
            (
                &["ctx", "daemon", "run", "--idle-exit-seconds", "0"][..],
                "daemon seconds must be between",
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
fn retired_once_has_bounded_human_recovery_without_force() {
    const RECOVERY: &str = "  ctx daemon run --idle-exit-seconds <SECONDS>";
    for width in [32, 80] {
        let output = human_output(&["ctx", "daemon", "run", "--once"], width);
        let normalized = normalized(&output);
        assert!(
            normalized.contains("The --once option has been retired"),
            "{output}"
        );
        assert!(normalized.contains("bounded foreground run"), "{output}");
        assert!(output.contains(RECOVERY), "{output}");
        assert!(!output.contains("--force"), "{output}");
        assert_width_safe(&output, width, &[RECOVERY]);
    }
}

#[test]
fn machine_parse_errors_remain_raw_clap_bytes() {
    for (arguments, expected_fragment) in [
        (
            &["ctx", "daemon", "run", "--once", "--format=json"][..],
            "--force",
        ),
        (
            &["ctx", "sources", "--provider", "unknown", "--format=json"][..],
            "run `ctx sources --all` to inspect every supported provider location",
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

#[test]
fn clap_help_keeps_the_terminal_design_width_ceiling() {
    let help = Cli::try_parse_from(["ctx", "sources", "--help"])
        .unwrap_err()
        .to_string();
    assert!(help.contains("Usage: ctx sources [OPTIONS]"));
    assert!(
        help.lines().all(|line| line.trim_end().width() <= 100),
        "{help}"
    );
}
