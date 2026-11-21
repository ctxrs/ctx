//! CLI composition aliases for terminal output.
//!
//! The argument enums live here because Clap is a composition concern; all
//! terminal writing and JSON helpers are implemented by `ctx-terminal`.

use clap::ValueEnum;

pub use ctx_terminal::output::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Text,
    Markdown,
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SessionOutputFormat {
    Text,
    Markdown,
    Json,
    Jsonl,
    Atif,
}

impl SessionOutputFormat {
    pub const fn is_json(self) -> bool {
        matches!(self, Self::Json | Self::Atif)
    }

    pub const fn is_machine_readable(self) -> bool {
        !matches!(self, Self::Text)
    }

    /// The shared transcript format, or `None` for the local-only ATIF export.
    pub const fn plain(self) -> Option<OutputFormat> {
        match self {
            Self::Text => Some(OutputFormat::Text),
            Self::Markdown => Some(OutputFormat::Markdown),
            Self::Json => Some(OutputFormat::Json),
            Self::Jsonl => Some(OutputFormat::Jsonl),
            Self::Atif => None,
        }
    }

    pub const fn render_format(self) -> crate::analytics::RenderFormat {
        match self {
            Self::Text => crate::analytics::RenderFormat::Text,
            Self::Markdown => crate::analytics::RenderFormat::Markdown,
            Self::Json => crate::analytics::RenderFormat::Json,
            Self::Jsonl => crate::analytics::RenderFormat::Jsonl,
            Self::Atif => crate::analytics::RenderFormat::Atif,
        }
    }
}

impl From<OutputFormat> for ctx_terminal::OutputFormat {
    fn from(value: OutputFormat) -> Self {
        match value {
            OutputFormat::Text => Self::Text,
            OutputFormat::Markdown => Self::Markdown,
            OutputFormat::Json => Self::Json,
            OutputFormat::Jsonl => Self::Jsonl,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum JsonOutputFormat {
    Text,
    Json,
}

impl From<JsonOutputFormat> for ctx_terminal::JsonOutputFormat {
    fn from(value: JsonOutputFormat) -> Self {
        match value {
            JsonOutputFormat::Text => Self::Text,
            JsonOutputFormat::Json => Self::Json,
        }
    }
}

impl JsonOutputFormat {
    pub const fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}
