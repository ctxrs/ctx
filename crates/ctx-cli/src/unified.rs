//! Dispatch independent local engines before opening history or its configuration.

use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct EngineArgs {
    /// Arguments for this command; use -- before a child program's arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(crate) arguments: Vec<OsString>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum UnifiedCommand {
    /// Index and navigate code and document relationships.
    Graph(Box<ctx_graph::GraphArgs>),
    /// Sift command output before it reaches an agent.
    #[command(disable_help_flag = true, disable_help_subcommand = true)]
    Sift(EngineArgs),
}

impl UnifiedCommand {
    pub(crate) fn is_graph(&self) -> bool {
        matches!(self, Self::Graph(_))
    }

    pub(crate) fn run(self) -> i32 {
        let args = match self {
            Self::Graph(args) => {
                return match ctx_graph::run_parsed(*args) {
                    Ok(()) => 0,
                    Err(error) => {
                        crate::output::write_stderr_line(format_args!(
                            "ctx graph: {}",
                            crate::ui::sanitize_untrusted_history_body_for_terminal(&format!(
                                "{error:#}"
                            ))
                        ));
                        1
                    }
                };
            }
            Self::Sift(args) => args,
        };
        let original = std::env::args_os().collect::<Vec<_>>();
        let arguments = preserved_output_arguments(&original, "sift").unwrap_or(args.arguments);
        run_sift(&arguments)
    }
}

/// Clap consumes an initial positional `--`. Forward the validated original
/// engine tail so literal dash-prefixed filenames and executables stay literal.
/// Root options are removed only before the trailing positional starts; after
/// its first token Clap treats the remaining tail as engine/child argv.
fn preserved_output_arguments(arguments: &[OsString], expected: &str) -> Option<Vec<OsString>> {
    let mut index = 1;
    while index < arguments.len() {
        match root_option_width(&arguments[index]) {
            Some(width) => index += width,
            None => break,
        }
    }
    if arguments.get(index)? != expected {
        return None;
    }
    index += 1;
    while index < arguments.len() {
        if let Some(width) = root_option_width(&arguments[index]) {
            index += width;
        } else {
            return Some(arguments[index..].to_vec());
        }
    }
    Some(Vec::new())
}

fn root_option_width(argument: &OsStr) -> Option<usize> {
    match argument.to_str()? {
        "--quiet" => Some(1),
        "--color" | "--data-root" => Some(2),
        value if value.starts_with("--color=") || value.starts_with("--data-root=") => Some(1),
        _ => None,
    }
}

const COMMANDS: &[&str] = &["graph", "sift"];

/// The common command path avoids parsing the history CLI and touching its state.
/// Root options still use the normal parser and the same engine dispatcher.
pub(crate) fn intercept(arguments: &[OsString]) -> Option<i32> {
    if arguments
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .any(|arg| {
            arg.to_str().is_some_and(|arg| {
                matches!(arg, "--data-root" | "--color" | "--quiet")
                    || arg.starts_with("--data-root=")
                    || arg.starts_with("--color=")
            })
        })
    {
        return None;
    }
    let name = arguments.get(1)?.to_str()?;
    if COMMANDS.contains(&name) {
        return Some(run_engine(name, &arguments[2..]));
    }
    if name == "help" {
        let command = arguments.get(2)?.to_str()?;
        if COMMANDS.contains(&command) {
            let mut forwarded = arguments[3..].to_vec();
            forwarded.push(OsStr::new("--help").to_owned());
            return Some(run_engine(command, &forwarded));
        }
    }
    None
}

fn run_engine(name: &str, arguments: &[OsString]) -> i32 {
    if name == "graph" {
        return ctx_graph::run(arguments.iter().cloned());
    }
    run_sift(arguments)
}

fn run_sift(arguments: &[OsString]) -> i32 {
    let Some(first) = arguments.first().and_then(|argument| argument.to_str()) else {
        return ctx_sift::run([OsString::from("--help")]);
    };
    if matches!(
        first,
        "--help"
            | "-h"
            | "--version"
            | "hook"
            | "filter"
            | "read"
            | "json"
            | "summary"
            | "err"
            | "test"
            | "gain"
            | "config"
            | "semantic"
            | "discover"
            | "ccusage"
            | "rewrite"
            | "run"
            | "proxy"
            | "compact"
            | "restore"
            | "recall"
    ) {
        return ctx_sift::run(arguments.iter().cloned());
    }
    let mut forwarded = vec![OsString::from("run"), OsString::from("--capture")];
    if first == "--" {
        forwarded.extend(arguments.iter().skip(1).cloned());
    } else if Path::new(first).is_file() {
        forwarded[0] = OsString::from("compact");
        forwarded.truncate(1);
        forwarded.extend(arguments.iter().cloned());
    } else {
        forwarded.extend(arguments.iter().cloned());
    }
    ctx_sift::run(forwarded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(argv: &[&str], command: &str) -> Vec<OsString> {
        preserved_output_arguments(
            &argv.iter().map(OsString::from).collect::<Vec<_>>(),
            command,
        )
        .unwrap()
    }

    #[test]
    fn root_controls_preserve_the_engine_separator_and_child_arguments() {
        assert_eq!(
            tail(&["ctx", "--quiet", "sift", "--", "--help"], "sift"),
            ["--", "--help"]
        );
        assert_eq!(
            tail(
                &[
                    "ctx",
                    "--data-root",
                    "sift",
                    "--color=never",
                    "sift",
                    "--raw",
                    "--",
                    "echo",
                    "--color",
                    "always",
                    "--quiet"
                ],
                "sift"
            ),
            ["--raw", "--", "echo", "--color", "always", "--quiet"]
        );
        assert_eq!(
            tail(&["ctx", "sift", "--quiet", "--", "-input"], "sift"),
            ["--", "-input"]
        );
        assert_eq!(
            tail(
                &["ctx", "--quiet", "sift", "--raw", "echo", "--color", "always", "--quiet"],
                "sift"
            ),
            ["--raw", "echo", "--color", "always", "--quiet"]
        );
    }
}
