mod anonymous_trial;
pub(crate) mod artifact_delivery;
mod authorization;
mod client;
mod commercial_api;
mod commercial_config;
mod commercial_deletion;
mod commercial_lifecycle;
mod credential_vault;
mod graph_key_deletion;
mod lifecycle;
mod local_deletion;
mod pending_materialization;
mod render;
mod request_identity;
mod setup_validation;
mod verified_executable;
mod workos_device;
pub(crate) use client::ProOutputImport;
pub(crate) use client::{blame, stable_error_code};
pub(crate) use lifecycle::{lifecycle_status_json, run_lifecycle, ProArgs};
pub(crate) use pending_materialization::run_if_pending as run_pending_materialization;
pub(crate) use render::{blame_result_json, print_blame_result};

use anyhow::anyhow;
pub(crate) const DEFAULT_BLAME_LIMIT: u32 = 20;

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
