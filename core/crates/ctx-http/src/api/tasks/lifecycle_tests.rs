use super::*;
use ctx_core::models::{SandboxSubstrate, VcsKind};
use ctx_daemon::test_support::{
    branch_exists, managed_worktree_path, TaskLifecycleSandboxBindingSeed,
    TaskLifecycleSessionSeed, TaskLifecycleWorktreeSeed,
};

#[path = "lifecycle_tests/archive.rs"]
mod archive;
#[path = "lifecycle_tests/delete.rs"]
mod delete;
#[path = "lifecycle_tests/delete_cleanup.rs"]
mod delete_cleanup;
#[path = "lifecycle_tests/fixtures.rs"]
mod fixtures;
#[path = "lifecycle_tests/missing_resources.rs"]
mod missing_resources;
#[path = "lifecycle_tests/unarchive.rs"]
mod unarchive;
use fixtures::*;
