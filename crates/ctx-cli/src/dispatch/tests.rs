use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use clap::Parser as _;

use super::*;
use crate::cli::Cli;
use crate::operation_descriptor::LocalUsageOperation;
use crate::ui::{ColorMode, RenderContext, StreamKind, TestContext};

fn daemon_autostart_trigger(args: &[&str]) -> Option<DaemonTriggerCommandArg> {
    let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
        .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
    command_daemon_autostart_trigger(&cli.command)
}

#[test]
fn setup_handoff_is_owned_by_setup_and_machine_import_does_not_autostart() {
    for args in [
        &["setup"][..],
        &["setup", "--format", "json"][..],
        &["setup", "--progress", "json"],
        &["import", "--format", "json"],
        &["import", "--progress", "json"],
    ] {
        assert!(daemon_autostart_trigger(args).is_none(), "{args:?}");
    }
}

#[test]
fn human_import_retains_post_command_daemon_autostart() {
    assert!(matches!(
        daemon_autostart_trigger(&["import"]),
        Some(DaemonTriggerCommandArg::Import)
    ));
}

#[test]
fn stats_is_excluded_from_remote_analytics() {
    for args in [
        &["stats"][..],
        &["stats", "--detail"][..],
        &["stats", "--format=json"][..],
    ] {
        let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
        assert!(
            ClientOperationDraft::from_descriptor(
                command_operation_descriptor(&cli.command),
                command_json_output(&cli.command),
            )
            .is_none(),
            "{args:?}"
        );
    }
}

#[test]
fn show_commands_are_typed_and_status_and_stats_are_excluded_from_local_usage() {
    for (args, expected) in [
        (&["show", "session", "abc"][..], "show_session"),
        (&["show", "event", "abc"][..], "show_event"),
        (&["list", "events"][..], "show_event"),
    ] {
        let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
        let descriptor = command_operation_descriptor(&cli.command);
        let local_operation = match &descriptor {
            OperationDescriptor::Cli(operation) => operation.local_usage_operation(),
            _ => None,
        };
        assert_eq!(
            local_operation.map(LocalUsageOperation::as_str),
            Some(expected)
        );
        assert!(
            local_usage::CliUsage::from_descriptor(&descriptor)
                .completed(true, std::time::Duration::ZERO)
                .is_some(),
            "{args:?}"
        );
    }

    for args in [&["status"][..], &["stats"][..]] {
        let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
        let descriptor = command_operation_descriptor(&cli.command);
        let local_operation = match &descriptor {
            OperationDescriptor::Cli(operation) => operation.local_usage_operation(),
            _ => None,
        };
        assert!(local_operation.is_none(), "{args:?}");
        assert!(
            local_usage::CliUsage::from_descriptor(&descriptor)
                .completed(true, std::time::Duration::ZERO)
                .is_none(),
            "{args:?}"
        );
    }
}

#[test]
fn query_authority_error_json_is_scoped_to_machine_search_show_and_locate() {
    for (args, expected) in [
        (&["search", "authority", "--format=json"][..], true),
        (&["show", "event", "bad", "--format=json"][..], true),
        (&["locate", "event", "bad", "--format=json"][..], true),
        (&["search", "authority"][..], false),
        (&["status", "--format=json"][..], false),
    ] {
        let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
        let json_output = command_json_output(&cli.command);
        assert_eq!(
            command_uses_query_authority_error_json(&cli.command, json_output),
            expected,
            "{args:?}"
        );
    }
}

#[derive(Clone, Default)]
struct SharedBytes(Arc<Mutex<Vec<u8>>>);

impl SharedBytes {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().map(|bytes| bytes.clone()).unwrap_or_default()
    }
}

impl Write for SharedBytes {
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

fn forced_color_test_ui(stderr: SharedBytes) -> Ui {
    Ui::with_writers(
        SharedBytes::default(),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Always)),
        stderr,
        RenderContext::for_test(TestContext::pipe(StreamKind::Stderr).color(ColorMode::Always)),
    )
}

#[test]
fn clap_value_errors_use_the_selected_stderr_stream_with_contextual_usage() {
    let arguments = ["ctx", "sources", "--provider", "unknown"];
    let mut error = Cli::try_parse_from(arguments).unwrap_err();
    let os_arguments = arguments
        .iter()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>();
    parse::attach_value_validation_usage(&mut error, &os_arguments);

    let stderr = SharedBytes::default();
    let stderr_copy = stderr.clone();
    let mut ui = forced_color_test_ui(stderr);
    write_clap_output(&error, &mut ui).unwrap();
    ui.flush().unwrap();

    let rendered = String::from_utf8(stderr_copy.bytes()).unwrap();
    assert!(rendered.contains('\u{1b}'));
    let mut stripped = anstream::StripStream::new(Vec::new());
    stripped.write_all(rendered.as_bytes()).unwrap();
    let plain = String::from_utf8(stripped.into_inner()).unwrap();
    assert!(plain.contains("unknown provider"));
    assert!(plain.contains("Usage: ctx sources [OPTIONS]"));
}

#[test]
fn forced_color_never_decorates_generic_machine_mode_errors() {
    for args in [
        &["show", "session", "bad", "--format", "jsonl"][..],
        &["show", "event", "bad", "--format", "markdown"][..],
        &["setup", "--progress", "json"][..],
        &["import", "--progress", "json"][..],
        &["mcp", "serve"][..],
        &["mcp", "--quiet", "serve"][..],
    ] {
        let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
            .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
        let json_output = command_json_output(&cli.command);
        assert!(
            command_machine_readable_output(&cli.command, json_output),
            "{args:?}"
        );

        let styled_stderr = SharedBytes::default();
        let styled_stderr_copy = styled_stderr.clone();
        let mut ui = forced_color_test_ui(styled_stderr);
        render_generic_command_error(
            &anyhow::anyhow!("representative command failure"),
            true,
            &mut ui,
        )
        .unwrap();
        ui.flush().unwrap();

        let machine_stderr = styled_stderr_copy.bytes();
        assert!(!machine_stderr.contains(&0x1b), "{args:?}");
        assert!(String::from_utf8_lossy(&machine_stderr)
            .starts_with("Error: representative command failure"));
    }
}

#[test]
fn forced_color_still_styles_generic_human_mode_errors() {
    let styled_stderr = SharedBytes::default();
    let styled_stderr_copy = styled_stderr.clone();
    let mut ui = forced_color_test_ui(styled_stderr);

    render_generic_command_error(&anyhow::anyhow!("human command failure"), false, &mut ui)
        .unwrap();
    ui.flush().unwrap();

    assert!(styled_stderr_copy.bytes().contains(&0x1b));
}

#[test]
fn generic_human_errors_include_the_actionable_cause_chain() {
    let stderr = SharedBytes::default();
    let stderr_copy = stderr.clone();
    let mut ui = Ui::with_writers(
        SharedBytes::default(),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Never)),
        stderr,
        RenderContext::for_test(TestContext::pipe(StreamKind::Stderr).color(ColorMode::Never)),
    );
    let error = anyhow::anyhow!("No such file or directory")
        .context("approve explicit source path /tmp/missing.jsonl");

    render_generic_command_error(&error, false, &mut ui).unwrap();
    ui.flush().unwrap();

    let rendered = String::from_utf8(stderr_copy.bytes()).unwrap();
    assert!(rendered.contains("approve explicit source path /tmp/missing.jsonl"));
    assert!(rendered.contains("No such file or directory"));
    assert!(!rendered.contains("Stack backtrace"));
}
