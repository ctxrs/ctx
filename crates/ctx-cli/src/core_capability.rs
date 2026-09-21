//! Fixed helpers for signed installation transactions and hosted uninstall.
use anyhow::{anyhow, Result};
use ctx_companion_bridge::{
    verify_signed_managed_pair_envelope, ReleaseChannel, SignedManagedPairIdentity,
    SignedManagedPairTarget,
};
use ctx_upgrade_engine::{
    ManagedPairComponentIdentity, ManagedPairTarget, ManagedPairVerifier,
    VerifiedManagedPairIdentity,
};
use std::process::ExitCode;

mod managed_pair_apply;
#[cfg(unix)]
mod managed_pair_reconcile;

const MANAGED_PAIR_APPLY_INVOCATION: &str = "--ctx-core-managed-pair-apply-v1";
#[cfg(unix)]
const MANAGED_PAIR_RECONCILE_INVOCATION: &str = "--ctx-core-managed-pair-reconcile-integration-v1";
const HOSTED_UNINSTALL_POST_EXIT_INVOCATION: &str = "--ctx-core-hosted-uninstall-after-parent-v1";
const DISABLE_MAN_PAGES_INVOCATION: &str = "--ctx-core-disable-managed-man-pages-v1";

pub(crate) fn intercept(arguments: &[std::ffi::OsString]) -> Option<ExitCode> {
    if arguments.len() == 2
        && arguments
            .get(1)
            .is_some_and(|value| value == DISABLE_MAN_PAGES_INVOCATION)
    {
        return Some(if ctx_upgrade_engine::disable_current_man_pages().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    if arguments
        .get(1)
        .is_some_and(|value| value == MANAGED_PAIR_APPLY_INVOCATION)
    {
        let result =
            crate::output::with_stdout_writer(|writer| managed_pair_apply::run(arguments, writer));
        return Some(match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                crate::output::write_stderr_line(format_args!("{error:#}"));
                ExitCode::FAILURE
            }
        });
    }
    #[cfg(unix)]
    if arguments
        .get(1)
        .is_some_and(|value| value == MANAGED_PAIR_RECONCILE_INVOCATION)
    {
        let result = crate::output::with_stdout_writer(|writer| {
            managed_pair_reconcile::run(arguments, writer)
        });
        return Some(match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                crate::output::write_stderr_line(format_args!("{error:#}"));
                ExitCode::FAILURE
            }
        });
    }
    if arguments
        .get(1)
        .is_some_and(|value| value == HOSTED_UNINSTALL_POST_EXIT_INVOCATION)
    {
        return Some(match run_hosted_uninstall_post_exit(arguments) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                crate::output::write_stderr_line(format_args!("{error:#}"));
                ExitCode::FAILURE
            }
        });
    }
    None
}

fn run_hosted_uninstall_post_exit(arguments: &[std::ffi::OsString]) -> Result<()> {
    if arguments.len() != 3 {
        return Err(anyhow!("invalid hosted uninstall post-exit invocation"));
    }
    let parent_pid = arguments[2]
        .to_str()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 1)
        .ok_or_else(|| anyhow!("invalid hosted uninstall parent PID"))?;
    ctx_upgrade_engine::run_hosted_uninstall_after_parent_exit(parent_pid)
}

fn write_response_frame(mut writer: impl std::io::Write, bytes: &[u8]) -> Result<()> {
    writer.write_all(bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

struct CoreManagedPairVerifier {
    expectations: ctx_companion_bridge::ManagedPairExpectations,
}

impl CoreManagedPairVerifier {
    fn for_channel(channel: ReleaseChannel) -> Self {
        Self {
            expectations: ctx_companion_bridge::ManagedPairExpectations::new(channel),
        }
    }
}

impl ManagedPairVerifier for CoreManagedPairVerifier {
    fn verify_signed_envelope(
        &self,
        signed_envelope: &[u8],
    ) -> Result<VerifiedManagedPairIdentity> {
        let identity = verify_signed_managed_pair_envelope(&self.expectations, signed_envelope)
            .map_err(|error| anyhow!(error.to_string()))?;
        engine_identity(&identity)
    }
}

fn engine_identity(identity: &SignedManagedPairIdentity) -> Result<VerifiedManagedPairIdentity> {
    let target = match identity.target() {
        SignedManagedPairTarget::LinuxArm64 => ManagedPairTarget::LinuxArm64,
        SignedManagedPairTarget::LinuxX64 => ManagedPairTarget::LinuxX64,
        SignedManagedPairTarget::MacosArm64 => ManagedPairTarget::MacosArm64,
        SignedManagedPairTarget::MacosX64 => ManagedPairTarget::MacosX64,
        SignedManagedPairTarget::WindowsX64 => ManagedPairTarget::WindowsX64,
    };
    VerifiedManagedPairIdentity::new(
        identity.release_name(),
        target,
        identity.rollback_generation(),
        identity.manifest_sha256().to_hex(),
        ManagedPairComponentIdentity::new(
            identity.core().sha256().to_hex(),
            identity.core().size_bytes(),
        )?,
        ManagedPairComponentIdentity::new(
            identity.companion().sha256().to_hex(),
            identity.companion().size_bytes(),
        )?,
    )
}

#[cfg(test)]
#[path = "core_capability/contract_tests.rs"]
mod tests;
