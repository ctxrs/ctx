use std::{error::Error as _, ffi::OsString};

use anyhow::{Context, Result};
use clap::{
    error::{ContextKind as ClapContextKind, ContextValue as ClapContextValue, ErrorKind},
    Command, CommandFactory, Parser,
};

use super::RenderedClapError;
use crate::{
    cli::Cli,
    ui::{
        diagnostic, scan_color_mode, scan_machine_output_hint, Action, ColorMode, Diagnostic,
        DiagnosticLevel, Document, Field, RenderContext, Ui,
    },
};

pub(super) fn parse_cli_from<I, T>(arguments: I) -> Result<Cli>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
    match Cli::try_parse_from(arguments.iter().cloned()) {
        Ok(cli) => Ok(cli),
        Err(mut error) => {
            attach_value_validation_usage(&mut error, &arguments);
            let exit_code = u8::try_from(error.exit_code()).unwrap_or(2);
            render_clap_output(&error, &arguments)?;
            Err(RenderedClapError(exit_code).into())
        }
    }
}

fn render_clap_output(error: &clap::Error, arguments: &[OsString]) -> Result<()> {
    let machine_output = scan_machine_output_hint(arguments);
    let mode = if machine_output {
        ColorMode::Never
    } else {
        scan_color_mode(arguments.iter().cloned()).unwrap_or(ColorMode::Auto)
    };
    let mut ui = Ui::stdio(mode);
    write_adapted_clap_output(error, arguments, machine_output, &mut ui)?;
    ui.flush().context("flush CLI parser output")
}

fn write_adapted_clap_output(
    error: &clap::Error,
    arguments: &[OsString],
    machine_output: bool,
    ui: &mut Ui,
) -> Result<()> {
    if !machine_output {
        let context = if error.use_stderr() {
            ui.stderr_context()
        } else {
            ui.stdout_context()
        };
        if let Some(document) = human_clap_document(error, arguments, context) {
            if error.use_stderr() {
                ui.write_stderr(&document)?;
            } else {
                ui.write_stdout(&document)?;
            }
            return Ok(());
        }
    }
    super::write_clap_output(error, ui)
}

fn human_clap_document(
    error: &clap::Error,
    arguments: &[OsString],
    context: &RenderContext,
) -> Option<Document> {
    if !error.use_stderr() {
        return None;
    }
    let leaf = leaf_bin_name(arguments);
    let (summary, detail, usage, action) = match error.kind() {
        ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            if leaf.as_deref() == Some("ctx") =>
        {
            (
                "A ctx command is required".to_owned(),
                None,
                Some("ctx [OPTIONS] <COMMAND>".to_owned()),
                Some("ctx --help"),
            )
        }
        ErrorKind::InvalidSubcommand if leaf.as_deref() == Some("ctx") => {
            let invalid = clap_context_text(error, ClapContextKind::InvalidSubcommand)?;
            (
                format!("unrecognized subcommand '{invalid}'"),
                None,
                Some("ctx [OPTIONS] <COMMAND>".to_owned()),
                Some("ctx --help"),
            )
        }
        ErrorKind::UnknownArgument
            if leaf.as_deref() == Some("ctx daemon run")
                && arguments.iter().any(|argument| argument == "--once") =>
        {
            (
                "The --once option has been retired".to_owned(),
                Some("Use a finite idle timeout for a bounded foreground run.".to_owned()),
                Some("ctx daemon run [OPTIONS]".to_owned()),
                Some("ctx daemon run --idle-exit-seconds <SECONDS>"),
            )
        }
        ErrorKind::ValueValidation => {
            let invalid_arg = clap_context_text(error, ClapContextKind::InvalidArg)?;
            let invalid_arg = invalid_arg
                .split_whitespace()
                .next()
                .unwrap_or(invalid_arg.as_str());
            let invalid_value = clap_context_text(error, ClapContextKind::InvalidValue)?;
            let (detail, action) = human_value_validation_recovery(
                invalid_arg,
                error.source().map(ToString::to_string),
            );
            (
                format!("invalid value '{invalid_value}' for '{invalid_arg}'"),
                detail,
                clap_usage(error),
                action,
            )
        }
        _ => return None,
    };
    let fields = usage
        .as_deref()
        .map(|usage| vec![Field::new("Usage", usage)])
        .unwrap_or_default();
    Some(diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Error,
            summary: &summary,
            detail: detail.as_deref(),
            fields: &fields,
            action: action.map(|command| Action { command }),
        },
    ))
}

fn human_value_validation_recovery(
    invalid_arg: &str,
    detail: Option<String>,
) -> (Option<String>, Option<&'static str>) {
    const PROVIDER_RECOVERY: &str =
        "; run `ctx sources --all` to inspect every supported provider location";
    if invalid_arg != "--provider" {
        return (detail, None);
    }
    let Some(detail) = detail else {
        return (None, None);
    };
    let Some(explanation) = detail.strip_suffix(PROVIDER_RECOVERY) else {
        return (Some(detail), None);
    };
    (
        Some(format!("{}.", explanation.trim_end_matches('.'))),
        Some("ctx sources --all"),
    )
}

fn clap_context_text(error: &clap::Error, kind: ClapContextKind) -> Option<String> {
    error
        .get(kind)
        .map(ToString::to_string)
        .filter(|value| !value.is_empty())
}

fn clap_usage(error: &clap::Error) -> Option<String> {
    clap_context_text(error, ClapContextKind::Usage).map(|usage| {
        usage
            .trim()
            .strip_prefix("Usage: ")
            .unwrap_or(usage.trim())
            .to_owned()
    })
}

pub(super) fn attach_value_validation_usage(error: &mut clap::Error, arguments: &[OsString]) {
    if error.kind() != ErrorKind::ValueValidation || error.get(ClapContextKind::Usage).is_some() {
        return;
    }
    let mut command = Cli::command();
    let leaf = leaf_command_for_arguments(&mut command, arguments);
    let usage = leaf.render_usage();
    error.insert(ClapContextKind::Usage, ClapContextValue::StyledStr(usage));
    *error = std::mem::replace(error, clap::Error::new(ErrorKind::ValueValidation)).with_cmd(leaf);
}

fn leaf_bin_name(arguments: &[OsString]) -> Option<String> {
    let mut command = Cli::command();
    leaf_command_for_arguments(&mut command, arguments)
        .get_bin_name()
        .map(ToOwned::to_owned)
}

fn leaf_command_for_arguments<'a>(
    command: &'a mut Command,
    arguments: &[OsString],
) -> &'a mut Command {
    let mut current = command;
    let mut command_path = vec!["ctx".to_owned()];
    let mut skip_global_value = false;
    for argument in arguments.iter().skip(1) {
        if skip_global_value {
            skip_global_value = false;
            continue;
        }
        let Some(argument) = argument.to_str() else {
            continue;
        };
        if matches!(argument, "--data-root" | "--color") {
            skip_global_value = true;
            continue;
        }
        if argument.starts_with('-') {
            continue;
        }
        let Some(index) = current
            .get_subcommands()
            .position(|subcommand| subcommand.get_name() == argument)
        else {
            continue;
        };
        current = current
            .get_subcommands_mut()
            .nth(index)
            .expect("subcommand index came from the same command");
        command_path.push(argument.to_owned());
    }
    current.set_bin_name(command_path.join(" "));
    current
}

#[cfg(test)]
#[path = "parse/tests.rs"]
mod tests;
