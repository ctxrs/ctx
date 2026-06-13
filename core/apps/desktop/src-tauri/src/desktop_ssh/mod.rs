use super::*;

mod auth;
mod commands;
mod connect;
mod install;
mod jobs;
mod model;
mod planner;
mod probe;
mod transport;
mod tunnel;
mod update;

use auth::*;
use install::*;
use jobs::*;
use model::*;
use planner::*;
use probe::*;
use transport::*;
use tunnel::*;
use update::{
    begin_remote_update_drain, release_remote_update_drain, remote_update_target_key,
    run_remote_daemon_self_update, schedule_pending_remote_daemon_update,
};

pub(crate) use commands::{
    desktop_get_git_branch, desktop_kickoff_remote_prewarm, desktop_list_ssh_hosts,
    desktop_list_ssh_paths, desktop_test_ssh,
};
pub(crate) use connect::{desktop_connect_ssh, desktop_connect_ssh_begin};
pub(crate) use jobs::desktop_connect_ssh_poll;
#[cfg(test)]
pub(crate) use transport::normalized_ssh_config_override;
pub(crate) use transport::{new_ssh_command, remote_path_expr, shell_escape};
pub(crate) use update::desktop_update_remote_daemon;

#[cfg(test)]
mod tests;
