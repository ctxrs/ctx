use std::{fmt::Write as _, path::Path};

use crate::ui::{diagnostic, Diagnostic, DiagnosticLevel, Document, Field, RenderContext, Token};

pub fn render_import_path_not_found(context: &RenderContext, path: &Path) -> Document {
    let path = render_os_path(path);
    let fields = [Field::new("Path", &path).with_value_token(Token::Reference)];
    diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Error,
            summary: "Import path does not exist",
            detail: None,
            fields: &fields,
            action: None,
        },
    )
}

/// Renders a concise stderr diagnostic for result-JSON mode without creating a
/// second JSON protocol or admitting terminal control bytes.
pub fn render_import_path_not_found_plain(path: &Path) -> String {
    format!("Import path does not exist: {}\n", render_os_path(path))
}

const ESCAPED_OS_PATH_PREFIX: &str = "os:\"";

/// Returns a control-safe, one-to-one terminal representation of an OS path.
///
/// Normal Unicode paths, including ordinary whitespace and Windows
/// separators, are retained directly. Paths that contain terminal controls,
/// invalid Unix bytes, or unpaired Windows UTF-16 units use a tagged quoted
/// form (`os:"…"`); within it, backslashes and quotes are escaped. Invalid
/// Unix bytes are `\\xNN` and unpaired Windows UTF-16 units are `\\u{NNNN}`.
#[cfg(unix)]
fn render_os_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;

    let mut remaining = path.as_os_str().as_bytes();
    if let Ok(text) = std::str::from_utf8(remaining) {
        if !requires_escaped_os_path_form(text) {
            return text.to_owned();
        }
    }

    let mut rendered = ESCAPED_OS_PATH_PREFIX.to_owned();
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(text) => {
                push_escaped_text(&mut rendered, text);
                break;
            }
            Err(error) => {
                let valid_len = error.valid_up_to();
                let valid = std::str::from_utf8(&remaining[..valid_len])
                    .expect("UTF-8 parser reported a valid prefix");
                push_escaped_text(&mut rendered, valid);
                let invalid_len = error
                    .error_len()
                    .unwrap_or_else(|| remaining.len().saturating_sub(valid_len));
                for &byte in &remaining[valid_len..valid_len + invalid_len] {
                    let _ = write!(rendered, "\\x{byte:02X}");
                }
                remaining = &remaining[valid_len + invalid_len..];
            }
        }
    }
    rendered.push('\"');
    rendered
}

#[cfg(windows)]
fn render_os_path(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt as _;

    let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if let Ok(text) = String::from_utf16(&units) {
        if !requires_escaped_os_path_form(&text) {
            return text;
        }
    }

    let mut rendered = ESCAPED_OS_PATH_PREFIX.to_owned();
    for character in char::decode_utf16(units) {
        match character {
            Ok(character) => push_escaped_character(&mut rendered, character),
            Err(error) => {
                let _ = write!(
                    rendered,
                    "\\u{{{:04X}}}",
                    u32::from(error.unpaired_surrogate())
                );
            }
        }
    }
    rendered.push('\"');
    rendered
}

#[cfg(not(any(unix, windows)))]
fn render_os_path(path: &Path) -> String {
    let text = path.as_os_str().to_string_lossy();
    if !requires_escaped_os_path_form(&text) {
        return text.into_owned();
    }

    let mut rendered = ESCAPED_OS_PATH_PREFIX.to_owned();
    push_escaped_text(&mut rendered, &text);
    rendered.push('\"');
    rendered
}

fn requires_escaped_os_path_form(text: &str) -> bool {
    text.starts_with(ESCAPED_OS_PATH_PREFIX)
        || text
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
}

fn push_escaped_text(rendered: &mut String, text: &str) {
    for character in text.chars() {
        push_escaped_character(rendered, character);
    }
}

fn push_escaped_character(rendered: &mut String, character: char) {
    match character {
        '\\' => rendered.push_str("\\\\"),
        '\"' => rendered.push_str("\\\""),
        '\n' => rendered.push_str("\\n"),
        '\r' => rendered.push_str("\\r"),
        '\t' => rendered.push_str("\\t"),
        '\u{1b}' => rendered.push_str("\\x1b"),
        character if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') => {
            let _ = write!(rendered, "\\u{{{:04x}}}", u32::from(character));
        }
        character => rendered.push(character),
    }
}

pub fn render_partial_deprecation(context: &RenderContext) -> Document {
    diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Warning,
            summary: "--partial is deprecated",
            detail: Some(
                "It no longer changes import behavior because tolerant import is always enabled.",
            ),
            fields: &[],
            action: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    #[test]
    fn partial_deprecation_is_warning_first_and_style_equivalent() {
        let context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stderr, 80).color(ColorMode::Always),
        );
        let document = render_partial_deprecation(&context);
        let plain = document.render_plain();
        assert_eq!(
            plain,
            "! --partial is deprecated\nIt no longer changes import behavior because tolerant import is always enabled.\n"
        );

        assert_eq!(strip_ansi(&document.render(&context)), plain);
    }

    #[test]
    fn missing_import_path_is_a_separate_exact_field_at_all_contract_widths() {
        let path = Path::new("  路径  missing\tfile\r\u{0001}\u{001b}\u{007f}\u{0085}\u{009f}  ");
        let visible = r#"os:"  路径  missing\tfile\r\u{0001}\x1b\u{007f}\u{0085}\u{009f}  ""#;

        for width in [32, 48, 80, 120] {
            let context = RenderContext::for_test(
                TestContext::tty(StreamKind::Stderr, width).color(ColorMode::Always),
            );
            let document = render_import_path_not_found(&context, path);
            let plain = document.render_plain();
            let aligned = format!("✗ Import path does not exist\n\nPath  {visible}\n");
            let stacked = format!("✗ Import path does not exist\n\nPath\n  {visible}\n");
            assert!(
                plain == aligned || plain == stacked,
                "width {width}: {plain:?}"
            );

            let styled = document.render(&context);
            assert!(styled.contains("\u{1b}["), "width {width}: {styled:?}");
            assert_eq!(strip_ansi(&styled), plain, "width {width}");
        }
    }

    #[test]
    fn missing_import_path_has_an_ascii_fallback_without_changing_the_path() {
        let context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stderr, 32)
                .color(ColorMode::Never)
                .unicode(false),
        );
        let plain =
            render_import_path_not_found(&context, Path::new("  missing  path  ")).render_plain();

        assert!(
            plain.starts_with("X Import path does not exist\n\n"),
            "{plain:?}"
        );
        assert!(plain.contains("  missing  path  \n"), "{plain:?}");
        assert!(!plain.contains('✗'), "{plain:?}");
    }

    #[test]
    fn missing_import_path_plain_diagnostic_is_control_safe() {
        let path = "  路径  \n\r\t\u{0001}\u{001b}\u{007f}\u{0085}\u{009f}  ";
        assert_eq!(
            render_import_path_not_found_plain(Path::new(path)),
            "Import path does not exist: os:\"  路径  \\n\\r\\t\\u{0001}\\x1b\\u{007f}\\u{0085}\\u{009f}  \"\n"
        );
    }

    #[test]
    fn missing_import_path_preserves_normal_windows_separators() {
        assert_eq!(
            render_import_path_not_found_plain(Path::new(r"C:\Users\ctx")),
            "Import path does not exist: C:\\Users\\ctx\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn missing_import_path_preserves_non_utf8_bytes_in_rich_and_plain_output() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt as _, path::PathBuf};

        let mut bytes = b"missing-\xFF-".to_vec();
        bytes.extend_from_slice("路径".as_bytes());
        let path = PathBuf::from(OsString::from_vec(bytes));
        let context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stderr, 80).color(ColorMode::Never),
        );

        assert!(render_import_path_not_found(&context, &path)
            .render_plain()
            .contains("Path  os:\"missing-\\xFF-路径\"\n"));
        assert_eq!(
            render_import_path_not_found_plain(&path),
            "Import path does not exist: os:\"missing-\\xFF-路径\"\n"
        );
    }

    #[cfg(windows)]
    #[test]
    fn missing_import_path_preserves_unpaired_utf16_in_plain_output() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt as _, path::PathBuf};

        let path = PathBuf::from(OsString::from_wide(&[b'm' as u16, 0xD800, b'x' as u16]));
        assert_eq!(
            render_import_path_not_found_plain(&path),
            "Import path does not exist: os:\"m\\u{D800}x\"\n"
        );
    }
}
