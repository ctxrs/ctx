use anstyle::{AnsiColor, Color, Style};

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
            Self::Text | Self::Command => Style::new(),
            Self::Heading => Style::new().bold(),
            Self::Label => Style::new().dimmed(),
            Self::Accent | Self::Reference => {
                Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan)))
            }
            Self::Success => Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))),
            Self::Warning => Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
            Self::Error => Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red))),
        }
    }
}
