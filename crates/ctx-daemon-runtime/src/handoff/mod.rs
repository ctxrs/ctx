mod installation;
mod recovery;
mod state;
mod termination;

pub use installation::*;
pub use recovery::*;
pub use state::*;
#[cfg(windows)]
pub use termination::wait_for_released_residual_daemon;
pub use termination::{
    terminate_identity_verified_residual_daemon, terminate_identity_verified_residual_daemon_owner,
};

pub const DAEMON_UPGRADE_RESTART_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub const DAEMON_UPGRADE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
