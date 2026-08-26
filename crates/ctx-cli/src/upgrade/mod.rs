mod automatic;
mod command;
mod config;
mod diagnostics;
pub(crate) mod ports;

pub(crate) use automatic::{maybe_spawn_automatic, wait_for_invoking_parent};
pub use command::run;
pub use ctx_cli_presentation::upgrade::UpgradeArgs;
pub(crate) use diagnostics::upgrade_diagnostics;
