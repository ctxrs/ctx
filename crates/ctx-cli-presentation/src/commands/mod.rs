pub mod doctor;
mod doctor_presentation;
mod history_health;
mod import_diagnostics;
pub mod index;
mod index_dashboard;
pub mod list;
pub mod locate;
pub mod search;
pub mod show;
pub mod sources;
pub mod stats;
pub mod status;
mod status_health;
mod status_presentation;
mod status_usage;

pub use doctor::DoctorArgs;
pub use doctor_presentation::{
    render_doctor_human, source_epoch_findings, DoctorRefreshFailure, DoctorSearchAvailability,
};
pub use import_diagnostics::{
    render_import_path_not_found, render_import_path_not_found_plain, render_partial_deprecation,
};
pub use index::IndexArgs;
pub use list::{ListArgs, ListEventsArgs, ListTarget};
pub use locate::{LocateArgs, LocateTarget};
pub use search::{CliRefreshArg, ContentScopeArg, SearchArgs, SearchBackendArg};
pub use setup::{render_setup_human, SetupArgs, SetupDaemonState};
pub use show::{ShowArgs, ShowEventArgs, ShowSessionArgs, ShowTarget};
pub use sources::SourcesArgs;
pub use stats::StatsArgs;
pub use status::{StatusArgs, UsageStatusMode};
pub use status_presentation::render_status_human;
pub use status_usage::{
    compact_usage_health_json, malformed_status_config_json, removed_cloud_config_json,
    render_malformed_status_config_failure, render_removed_cloud_config_failure,
    render_usage_action_human, render_usage_failure, usage_action_error_json, usage_action_json,
};
pub mod setup;
