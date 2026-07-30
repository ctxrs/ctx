mod anonymous_trial;
pub(crate) mod artifact_delivery;
mod authorization;
mod client;
mod commercial_api;
mod commercial_config;
mod commercial_deletion;
mod commercial_lifecycle;
mod commercial_production_record;
mod credential_vault;
mod graph_key_deletion;
mod helper_command;
mod lifecycle;
mod local_deletion;
mod pending_materialization;
mod pricing;
#[cfg(any(test, ctx_pro_qualification))]
mod qualification_helper;
mod referral;
mod render;
mod request_identity;
mod setup_validation;
mod verified_executable;
mod workos_device;
pub(crate) use client::{
    blame, preflight_source_manifest_materialization, stable_error_code,
    sync_source_manifest_materialization,
};
pub(crate) use lifecycle::{lifecycle_status_json, run_lifecycle, ProArgs};
pub(crate) use pricing::PRO_MONTHLY_PRICE_DISPLAY;
#[cfg(test)]
pub(crate) use referral::parse_referral_codename;
pub(crate) use referral::{run as run_referral, show_cta_once, ReferralArgs};
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
            "unlock or repair the already selected secure key store, then run `ctx pro`; a fresh installation can select the owner-private local vault only when the native store is genuinely unavailable, and ctx never downgrades existing state"
        }
        _ => return error,
    };
    anyhow!("{code}: {guidance}")
}
