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
    scan_color_mode(arguments)
        .unwrap_or(ColorMode::Auto)
        .as_anstream()
        .write_global();
}

pub(super) fn scan_color_mode<I>(arguments: I) -> Option<ColorMode>
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
