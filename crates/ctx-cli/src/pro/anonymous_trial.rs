use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_pro_host_protocol::base64url;
use zeroize::Zeroize as _;

use super::{
    artifact_delivery::{fetch_latest, CommercialArtifactAuth},
    authorization::InstallationChallengeSigner,
    commercial_lifecycle::{vault_error, CommercialLifecycleService},
    credential_vault::{
        AnonymousTrialMaterial, CredentialRecord, CredentialRecordKind,
        VaultInstallationChallengeSigner,
    },
    lifecycle::{lifecycle_manifest::platform_target, ProSetupPlan},
};

const ENTITLEMENT_REFRESH_RETRY_SECONDS: i64 = 60 * 60;
const DEVICE_EVIDENCE_TIMEOUT_SECONDS: u64 = 15;
const MAX_DEVICE_EVIDENCE_BYTES: u64 = 32 * 1024;

pub(super) fn refresh_entitlement(service: &CommercialLifecycleService) -> Result<()> {
    let record = service
        .vault
        .load(CredentialRecordKind::AnonymousTrial)
        .map_err(vault_error)?;
    let CredentialRecord::AnonymousTrial(mut trial) = record else {
        bail!("key_store_unavailable: anonymous trial record mismatch");
    };
    let now = super::commercial_lifecycle::unix_time()?;
    if trial
        .refresh_not_before_unix()
        .is_some_and(|not_before| now < not_before)
    {
        bail!("service_unavailable: anonymous trial refresh is deferred");
    }
    let public_key = VaultInstallationChallengeSigner::new(&service.vault)
        .public_key()
        .context("key_store_unavailable: load installation public key")?;
    let refresh = service
        .api
        .refresh_trial(trial.access_token(), &base64url(&public_key))?;
    if refresh.trial_deadline_unix != trial.trial_deadline_unix() {
        bail!("invalid_response: anonymous trial refresh changed the authoritative deadline");
    }
    service.store_anonymous_entitlement(
        refresh.entitlement,
        &public_key,
        trial.trial_deadline_unix(),
    )?;
    trial = trial
        .with_access_token(refresh.trial_access_token)
        .map_err(vault_error)?;
    service
        .vault
        .store(&CredentialRecord::AnonymousTrial(trial))
        .map_err(vault_error)
}

pub(super) fn defer_refresh(service: &CommercialLifecycleService, now: i64) {
    let Ok(CredentialRecord::AnonymousTrial(trial)) =
        service.vault.load(CredentialRecordKind::AnonymousTrial)
    else {
        return;
    };
    let Ok(trial) = trial
        .with_refresh_not_before_unix(Some(now.saturating_add(ENTITLEMENT_REFRESH_RETRY_SECONDS)))
    else {
        return;
    };
    let _ = service
        .vault
        .store(&CredentialRecord::AnonymousTrial(trial));
}

pub(super) fn setup(
    service: &CommercialLifecycleService,
    data_root: &Path,
    installed_version: Option<&str>,
) -> Result<ProSetupPlan> {
    let public_key = service.installation_public_key()?;
    let encoded_public_key = base64url(&public_key);
    if let Ok(CredentialRecord::AnonymousTrial(mut trial)) =
        service.vault.load(CredentialRecordKind::AnonymousTrial)
    {
        let refresh = service
            .api
            .refresh_trial(trial.access_token(), &encoded_public_key)?;
        if refresh.trial_deadline_unix != trial.trial_deadline_unix() {
            bail!("invalid_response: anonymous trial refresh changed the authoritative deadline");
        }
        service.store_anonymous_entitlement(
            refresh.entitlement,
            &public_key,
            trial.trial_deadline_unix(),
        )?;
        trial = trial
            .with_access_token(refresh.trial_access_token)
            .map_err(vault_error)?;
        let authorization = format!("CtxTrial {}", trial.access_token());
        let artifact = fetch_latest(
            data_root,
            CommercialArtifactAuth {
                api_base_url: service.api.origin(),
                authorization: &authorization,
                release_trust: service.config.release_trust,
            },
            installed_version,
        )?;
        service
            .vault
            .store(&CredentialRecord::AnonymousTrial(trial))
            .map_err(vault_error)?;
        return Ok(ProSetupPlan {
            artifact: Some(artifact),
            account_state: "trial".to_owned(),
        });
    }

    let target = platform_target();
    let challenge = service.api.trial_challenge(
        service.config.release_trust.channel.wire_name(),
        &target,
        installed_version,
        ctx_pro_host_protocol::PROTOCOL_VERSION,
        ctx_pro_host_protocol::PROTOCOL_FINGERPRINT,
        &encoded_public_key,
    )?;
    let mut bootstrap_token = challenge.artifact_access_token;
    let authorization = format!("CtxTrial {bootstrap_token}");
    let artifact = fetch_latest(
        data_root,
        CommercialArtifactAuth {
            api_base_url: service.api.origin(),
            authorization: &authorization,
            release_trust: service.config.release_trust,
        },
        installed_version,
    )?;
    let evidence = collect_device_evidence(
        &artifact.artifact,
        &challenge.challenge_base64url,
        &encoded_public_key,
    )?;
    let activation = service.api.activate_trial(
        &bootstrap_token,
        &challenge.challenge_id,
        &encoded_public_key,
        &evidence,
    );
    bootstrap_token.zeroize();
    let activation = activation?;
    service.store_anonymous_entitlement(
        activation.entitlement,
        &public_key,
        activation.trial_deadline_unix,
    )?;
    let trial = AnonymousTrialMaterial::new(
        activation.trial_access_token,
        activation.trial_deadline_unix,
    )
    .map_err(vault_error)?;
    service
        .vault
        .store(&CredentialRecord::AnonymousTrial(trial))
        .map_err(vault_error)?;
    Ok(ProSetupPlan {
        artifact: Some(artifact),
        account_state: "trial".to_owned(),
    })
}

fn collect_device_evidence(
    helper: &Path,
    challenge_base64url: &str,
    installation_public_key_base64url: &str,
) -> Result<serde_json::Value> {
    let mut child = Command::new(helper)
        .arg("_activation-material")
        .arg(challenge_base64url)
        .arg(installation_public_key_base64url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("helper_crashed: start signed Pro activation helper")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("helper_crashed: activation helper stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("helper_crashed: activation helper stderr is unavailable"))?;
    let stdout_reader = thread::spawn(move || read_bounded_pipe(stdout));
    let stderr_reader = thread::spawn(move || read_bounded_pipe(stderr));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .context("helper_crashed: poll activation helper")?
        {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(DEVICE_EVIDENCE_TIMEOUT_SECONDS) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("helper_timeout: Pro activation helper timed out");
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow!("helper_crashed: activation helper output reader failed"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow!("helper_crashed: activation helper error reader failed"))??;
    drop(stderr);
    if !status.success() {
        bail!("helper_crashed: Pro activation evidence is unavailable");
    }
    serde_json::from_slice(&stdout)
        .context("invalid_response: Pro activation helper returned malformed evidence")
}

fn read_bounded_pipe(pipe: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.take(MAX_DEVICE_EVIDENCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("helper_crashed: read activation helper output")?;
    if bytes.len() as u64 > MAX_DEVICE_EVIDENCE_BYTES {
        bail!("invalid_response: Pro activation helper output exceeds maximum size");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn activation_output_reader_enforces_the_privacy_boundary() {
        assert_eq!(
            read_bounded_pipe(Cursor::new(b"{}".to_vec())).unwrap(),
            b"{}"
        );
        let oversized = vec![b'x'; MAX_DEVICE_EVIDENCE_BYTES as usize + 1];
        assert!(read_bounded_pipe(Cursor::new(oversized)).is_err());
    }
}
