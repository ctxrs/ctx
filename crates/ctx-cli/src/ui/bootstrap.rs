use std::ffi::OsString;

use super::ColorMode;

/// Applies the color override before Clap can render help or a parse error.
///
/// This is intentionally not a second CLI parser. It recognizes only the two
/// spellings of the global color option and leaves all validation, duplicate
/// handling, and command semantics to Clap.
pub(crate) fn bootstrap_color_choice<I>(arguments: I)
where
    I: IntoIterator<Item = OsString>,
{
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    let mode = if scan_machine_output_hint(&arguments) {
        ColorMode::Never
    } else {
        scan_color_mode(arguments.iter().cloned()).unwrap_or(ColorMode::Auto)
    };
    mode.as_anstream().write_global();
}

pub(crate) fn scan_color_mode<I>(arguments: I) -> Option<ColorMode>
where
    I: IntoIterator<Item = OsString>,
{
    let arguments = arguments.into_iter().skip(1).collect::<Vec<_>>();
    let mut selected = None;

    for (index, argument) in arguments.iter().enumerate() {
        let Some(argument) = argument.to_str() else {
            continue;
        };
        if argument == "--" {
            break;
        }
        if let Some(value) = argument.strip_prefix("--color=") {
            if let Some(mode) = ColorMode::from_cli_value(value) {
                selected = Some(mode);
            }
            continue;
        }
        if argument == "--color" {
            if let Some(value) = arguments.get(index + 1).and_then(|value| value.to_str()) {
                if let Some(mode) = ColorMode::from_cli_value(value) {
                    selected = Some(mode);
                }
            }
        }
    }

    selected
}

/// Conservatively recognizes explicit machine-output spellings before Clap
/// renders a possible parse error. Dispatch remains the authoritative command
/// classifier after parsing.
pub(crate) fn scan_machine_output_hint(arguments: &[OsString]) -> bool {
    let arguments = arguments
        .iter()
        .skip(1)
        .filter_map(|argument| argument.to_str())
        .take_while(|argument| *argument != "--")
        .collect::<Vec<_>>();

    for (index, argument) in arguments.iter().enumerate() {
        if let Some(value) = argument.strip_prefix("--format=") {
            if machine_format(value) {
                return true;
            }
        } else if *argument == "--format" {
            if arguments
                .get(index + 1)
                .is_some_and(|value| machine_format(value))
            {
                return true;
            }
        } else if argument.strip_prefix("--progress=") == Some("json")
            || (*argument == "--progress" && arguments.get(index + 1).copied() == Some("json"))
        {
            return true;
        }
    }

    arguments
        .iter()
        .position(|argument| *argument == "mcp")
        .is_some_and(|position| arguments[position + 1..].contains(&"serve"))
}

fn machine_format(value: &str) -> bool {
    matches!(value, "json" | "jsonl" | "csv" | "raw" | "markdown")
}
