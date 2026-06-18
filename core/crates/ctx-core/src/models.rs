mod agent_work;
mod attachments;
mod merge_queue;
mod mobile;
mod plugin;
mod policy;
mod run_archive;
mod runs;
mod sandbox;
mod session;
mod session_events;
mod workspace;
mod workspace_activity;
mod worktree_vcs;

pub use agent_work::*;
pub use attachments::*;
pub use merge_queue::*;
pub use mobile::*;
pub use plugin::*;
pub use policy::*;
pub use run_archive::*;
pub use runs::*;
pub use sandbox::*;
pub use session::*;
pub use session_events::*;
pub use workspace::*;
pub use workspace_activity::*;
pub use worktree_vcs::*;

pub(super) fn is_false(v: &bool) -> bool {
    !*v
}

pub(super) fn is_true(v: &bool) -> bool {
    *v
}

pub(super) fn default_true() -> bool {
    true
}
