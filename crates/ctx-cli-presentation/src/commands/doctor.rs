use clap::Args;

use crate::output::JsonOutputFormat;

#[derive(Debug, Args, Clone)]
pub struct DoctorArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
    #[arg(
        long,
        help = "Repair history lease metadata while preserving verified history"
    )]
    pub repair: bool,
}
