use clap::{Args, ValueEnum};

use crate::output::JsonOutputFormat;

#[derive(Debug, Args, Clone)]
pub struct StatusArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
    #[arg(
        long,
        value_enum,
        help = "Local usage control: enable, disable, or reset"
    )]
    pub usage: Option<UsageStatusMode>,
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = clap::value_parser!(u64).range(1..=60),
        conflicts_with = "usage",
        help = "Observe daemon activity for 1–60 seconds and print a shareable report"
    )]
    pub sample: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum UsageStatusMode {
    Enable,
    Disable,
    Reset,
}

impl UsageStatusMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Reset => "reset",
        }
    }
}
