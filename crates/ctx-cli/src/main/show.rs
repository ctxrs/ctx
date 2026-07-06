#[allow(unused_imports)]
use super::*;

#[derive(Debug, Args)]
pub(crate) struct ShowArgs {
    #[command(subcommand)]
    pub(crate) target: ShowTarget,
}

pub(crate) struct ShowDto;
