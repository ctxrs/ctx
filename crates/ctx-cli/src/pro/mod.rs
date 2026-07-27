pub(crate) mod artifact_delivery;
mod authorization;
mod client;
mod commercial_api;
mod commercial_config;
mod commercial_lifecycle;
mod credential_vault;
mod graph_key_deletion;
mod lifecycle;
mod local_deletion;
mod render;
mod request_identity;
mod verified_executable;
mod workos_device;
pub(crate) use client::{query, stable_error_code};
pub(crate) use lifecycle::{lifecycle_status_json, run_lifecycle, ProArgs};
pub(crate) use render::{print_query_result, query_result_json};

use anyhow::anyhow;
use clap::ValueEnum;
use ctx_pro_host_protocol::{ResourceKind, ResourceSelector};
use serde_json::{json, Value};

pub(crate) const DEFAULT_QUERY_LIMIT: u32 = 100;

pub(crate) fn actionable_error(error: anyhow::Error) -> anyhow::Error {
    let Some(code) = stable_error_code(&error) else {
        return error;
    };
    let guidance = match code {
        "pro_not_installed" => "ctx Pro is not set up; run `ctx pro`",
        "entitlement_expired" => "ctx Pro is locked; run `ctx pro manage` to restore access",
        "helper_upgrade_required" | "protocol_mismatch" => {
            "the Pro helper needs repair; run `ctx pro`"
        }
        "not_materialized" | "needs_rebuild" | "partial" | "needs_resume" => {
            "the Pro graph needs repair; run `ctx pro`"
        }
        "key_store_unavailable" | "key_store_locked" => {
            "configure and unlock a persistent platform key store (not an ephemeral session collection), then run `ctx pro`; plaintext key fallback is not supported"
        }
        _ => return error,
    };
    anyhow!("{code}: {guidance}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ResourceKindArg {
    Repository,
    Checkout,
    Worktree,
    Branch,
    Commit,
    File,
    #[value(name = "pr", alias = "pull-request")]
    PullRequest,
    Issue,
    Remote,
    Release,
    Command,
    Check,
    Session,
    Agent,
    Run,
}

impl ResourceKindArg {
    pub(crate) const fn protocol(self) -> ResourceKind {
        match self {
            Self::Repository => ResourceKind::Repository,
            Self::Checkout => ResourceKind::Checkout,
            Self::Worktree => ResourceKind::Worktree,
            Self::Branch => ResourceKind::Branch,
            Self::Commit => ResourceKind::Commit,
            Self::File => ResourceKind::File,
            Self::PullRequest => ResourceKind::PullRequest,
            Self::Issue => ResourceKind::Issue,
            Self::Remote => ResourceKind::Remote,
            Self::Release => ResourceKind::Release,
            Self::Command => ResourceKind::Command,
            Self::Check => ResourceKind::Check,
            Self::Session => ResourceKind::Session,
            Self::Agent => ResourceKind::Agent,
            Self::Run => ResourceKind::Run,
        }
    }
}

pub(crate) fn selector_json(selector: &ResourceSelector) -> Value {
    json!({
        "kind": selector.kind.wire_name(),
        "value": selector.value,
        "repository": selector.repository,
        "line": selector.line,
    })
}
