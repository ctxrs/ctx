use anstyle::{AnsiColor, Color, Style};

pub(crate) const CLAP_STYLES: clap::builder::styling::Styles =
    clap::builder::styling::Styles::styled()
        .header(Style::new().bold())
        .usage(Style::new().bold())
        .literal(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan))))
        .placeholder(Style::new().dimmed())
        .error(
            Style::new()
                .fg_color(Some(Color::Ansi(AnsiColor::Red)))
                .bold(),
        )
        .valid(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))))
        .invalid(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))))
        .context(Style::new().dimmed())
        .context_value(Style::new());

/// Removes visible whitespace at terminal line ends while preserving ANSI CSI
/// sequences that restore or establish the surrounding style.
pub(crate) fn trim_terminal_line_ends(rendered: &str) -> String {
    let mut trimmed = String::with_capacity(rendered.len());
    for line in rendered.split_inclusive('\n') {
        let (line, newline) = line
            .strip_suffix('\n')
            .map_or((line, ""), |line| (line, "\n"));
        let (line, carriage_return) = line
            .strip_suffix('\r')
            .map_or((line, ""), |line| (line, "\r"));
        trim_terminal_line_end(line, &mut trimmed);
        trimmed.push_str(carriage_return);
        trimmed.push_str(newline);
    }
    trimmed
}

fn trim_terminal_line_end(line: &str, trimmed: &mut String) {
    let mut cursor = 0;
    let mut pending = String::new();
    let mut pending_controls = String::new();

    while cursor < line.len() {
        if let Some(end) = csi_sequence_end(line.as_bytes(), cursor) {
            let control = &line[cursor..end];
            pending.push_str(control);
            pending_controls.push_str(control);
            cursor = end;
            continue;
        }

        let Some(character) = line[cursor..].chars().next() else {
            break;
        };
        cursor += character.len_utf8();
        if character.is_whitespace() {
            pending.push(character);
        } else {
            trimmed.push_str(&pending);
            pending.clear();
            pending_controls.clear();
            trimmed.push(character);
        }
    }

    trimmed.push_str(&pending_controls);
}

fn csi_sequence_end(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start..start + 2) != Some(b"\x1b[") {
        return None;
    }
    bytes[start + 2..]
        .iter()
        .position(|byte| (0x40..=0x7e).contains(byte))
        .map(|offset| start + 2 + offset + 1)
}

/// Semantic roles for human output.
///
/// State colors are reserved for markers and short state values. Ordinary
/// values keep the terminal's default foreground.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Token {
    #[default]
    Text,
    Heading,
    Label,
    Accent,
    Success,
    Warning,
    Error,
    Command,
    Reference,
}

impl Token {
    pub(crate) const fn style(self) -> Style {
        match self {
            Self::Text => Style::new(),
            Self::Heading => Style::new().bold(),
            Self::Label => Style::new().dimmed(),
            Self::Accent | Self::Command | Self::Reference => {
                Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan)))
            }
            Self::Success => Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))),
            Self::Warning => Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
            Self::Error => Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::trim_terminal_line_ends;

    #[test]
    fn terminal_line_trimming_preserves_style_controls_and_plain_content() {
        let styled = "value \x1b[2m\nkept \x1b[2mspace\x1b[0m \x1b[2m\r\n";
        let trimmed = trim_terminal_line_ends(styled);

        assert_eq!(trimmed, "value\x1b[2m\nkept \x1b[2mspace\x1b[0m\x1b[2m\r\n");
        assert_eq!(
            anstream::adapter::strip_str(&trimmed).to_string(),
            "value\nkept space\r\n"
        );
    }
}
