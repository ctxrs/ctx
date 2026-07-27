use std::{
    io::Read,
    path::Path,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_pro_host_protocol::base64url;
use zeroize::Zeroizing;

use super::{
    artifact_delivery::{acquire_latest, ArtifactDeliveryConfig},
    authorization::InstallationChallengeSigner,
    commercial_api::TrialChallengeRequest,
    commercial_lifecycle::{vault_error, CommercialLifecycleService},
    credential_vault::{
        AnonymousTrialMaterial, CredentialRecord, CredentialRecordKind,
        VaultInstallationChallengeSigner,
    },
    helper_command,
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
    let mut refresh = service
        .api
        .refresh_trial(trial.access_token(), &base64url(&public_key))?;
    if refresh.trial_deadline_unix != trial.trial_deadline_unix() {
        bail!("invalid_response: anonymous trial refresh changed the authoritative deadline");
    }
    let trial_deadline_unix = trial.trial_deadline_unix();
    trial = apply_trial_refresh(
        trial,
        std::mem::take(&mut refresh.trial_access_token),
        std::mem::take(&mut refresh.referral_claim_token),
    )?;
    service.store_anonymous_state(
        refresh.entitlement.clone(),
        &public_key,
        trial_deadline_unix,
        trial,
    )
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
    referral_codename: Option<&str>,
) -> Result<ProSetupPlan> {
    let public_key = service.installation_public_key()?;
    let encoded_public_key = base64url(&public_key);
    if let Ok(CredentialRecord::AnonymousTrial(mut trial)) =
        service.vault.load(CredentialRecordKind::AnonymousTrial)
    {
        if referral_codename.is_some() {
            bail!("invalid_request: a referral cannot be added after a Pro trial has started");
        }
        let mut refresh = service
            .api
            .refresh_trial(trial.access_token(), &encoded_public_key)?;
        if refresh.trial_deadline_unix != trial.trial_deadline_unix() {
            bail!("invalid_response: anonymous trial refresh changed the authoritative deadline");
        }
        let trial_deadline_unix = trial.trial_deadline_unix();
        trial = apply_trial_refresh(
            trial,
            std::mem::take(&mut refresh.trial_access_token),
            std::mem::take(&mut refresh.referral_claim_token),
        )?;
        let artifact = acquire_latest(
            data_root,
            installed_version,
            ArtifactDeliveryConfig::new(service.api.config(), service.config.release_trust),
        )?;
        service.store_anonymous_state(
            refresh.entitlement.clone(),
            &public_key,
            trial_deadline_unix,
            trial,
        )?;
        return Ok(ProSetupPlan {
            artifact: Some(artifact),
            account_state: "trial".to_owned(),
        });
    }

    let target = platform_target();
    let mut challenge = service.api.trial_challenge(TrialChallengeRequest {
        schema_version: 1,
        channel: service.config.release_trust.channel.wire_name(),
        target: &target,
        current_version: installed_version,
        protocol_version: ctx_pro_host_protocol::PROTOCOL_VERSION,
        protocol_fingerprint: ctx_pro_host_protocol::PROTOCOL_FINGERPRINT,
        installation_public_key_base64url: &encoded_public_key,
        referral_codename,
    })?;
    let activation_token = Zeroizing::new(std::mem::take(&mut challenge.trial_activation_token));
    let artifact = acquire_latest(
        data_root,
        installed_version,
        ArtifactDeliveryConfig::new(service.api.config(), service.config.release_trust),
    )?;
    let evidence = collect_device_evidence(
        data_root,
        artifact.verified_helper_path()?,
        &challenge.challenge_base64url,
        &encoded_public_key,
    )?;
    let mut activation = service.api.activate_trial(
        activation_token.as_str(),
        &challenge.challenge_id,
        &encoded_public_key,
        &evidence,
    )?;
    if referral_codename.is_some() != activation.referral_claim_token.is_some() {
        bail!("invalid_response: referral attribution result is inconsistent");
    }
    let trial = AnonymousTrialMaterial::new(
        std::mem::take(&mut activation.trial_access_token),
        activation.trial_deadline_unix,
    )
    .and_then(|trial| {
        trial.with_referral_claim_token(std::mem::take(&mut activation.referral_claim_token))
    })
    .map_err(vault_error)?;
    service.store_anonymous_state(
        activation.entitlement.clone(),
        &public_key,
        activation.trial_deadline_unix,
        trial,
    )?;
    Ok(ProSetupPlan {
        artifact: Some(artifact),
        account_state: "trial".to_owned(),
    })
}

fn apply_trial_refresh(
    trial: AnonymousTrialMaterial,
    access_token: String,
    refreshed_referral_claim_token: Option<String>,
) -> Result<AnonymousTrialMaterial> {
    if trial.referral_claim_token().is_none() && refreshed_referral_claim_token.is_some() {
        bail!("invalid_response: trial refresh cannot add referral attribution");
    }
    let trial = trial.with_access_token(access_token).map_err(vault_error)?;
    match refreshed_referral_claim_token {
        Some(token) => trial
            .with_referral_claim_token(Some(token))
            .map_err(vault_error),
        None => Ok(trial),
    }
}

fn collect_device_evidence(
    data_root: &Path,
    helper: &Path,
    challenge_base64url: &str,
    installation_public_key_base64url: &str,
) -> Result<serde_json::Value> {
    let mut command = helper_command::new(helper, data_root, None)?;
    command
        .arg("_activation-material")
        .arg(challenge_base64url)
        .arg(installation_public_key_base64url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
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

    #[test]
    fn refresh_preserves_or_rotates_an_existing_claim_but_never_attaches_one_late() {
        let referred = AnonymousTrialMaterial::new("a".repeat(32), 2_000)
            .unwrap()
            .with_referral_claim_token(Some("claim.original_123".to_owned()))
            .unwrap();
        let preserved = apply_trial_refresh(referred, "b".repeat(32), None).unwrap();
        assert_eq!(preserved.referral_claim_token(), Some("claim.original_123"));
        let rotated = apply_trial_refresh(
            preserved,
            "c".repeat(32),
            Some("claim.rotated_4567".to_owned()),
        )
        .unwrap();
        assert_eq!(rotated.referral_claim_token(), Some("claim.rotated_4567"));

        let nonreferred = AnonymousTrialMaterial::new("a".repeat(32), 2_000).unwrap();
        let error = apply_trial_refresh(
            nonreferred,
            "b".repeat(32),
            Some("claim.late_123456".to_owned()),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid_response: trial refresh cannot add referral attribution"
        );
    }

    #[cfg(unix)]
    fn write_activation_fixture(path: &Path, body: &str) {
        use std::{fs, os::unix::fs::PermissionsExt as _};

        fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }

    #[cfg(unix)]
    #[test]
    fn activation_process_receives_bounded_helper_environment_without_changing_its_contract() {
        let root = tempfile::tempdir().unwrap();
        let installation_id = crate::identity::installation_id(root.path()).unwrap();
        let pro_root = ctx_pro_host_protocol::ProFilesystemLayout::new(root.path()).pro_root();
        let helper = root.path().join("activation-fixture");
        let evidence = serde_json::json!({
            "schema_version": 1,
            "payload_type": "activation_material",
            "evidence": {"anchor": "opaque-fixture"},
        });
        let body = format!(
            r#"
expected_data_root={}
expected_pro_root={}
expected_installation_id={}
[ "${{CTX_DATA_ROOT-}}" = "$expected_data_root" ] || exit 21
[ "${{CTX_PRO_DATA_ROOT-}}" = "$expected_pro_root" ] || exit 22
[ "${{CTX_PRO_INSTALLATION_ID-}}" = "$expected_installation_id" ] || exit 23
[ "${{CTX_PRO_GIT_EXECUTABLE+x}}" != x ] || exit 26
[ "$#" -eq 3 ] || exit 31
[ "$1" = "_activation-material" ] || exit 32
[ "$2" = "challenge-base64url" ] || exit 33
[ "$3" = "installation-public-key-base64url" ] || exit 34
printf '%s' '{}'
"#,
            shell_quote(&root.path().to_string_lossy()),
            shell_quote(&pro_root.to_string_lossy()),
            shell_quote(&installation_id),
            evidence,
        );
        write_activation_fixture(&helper, &body);

        let actual = collect_device_evidence(
            root.path(),
            &helper,
            "challenge-base64url",
            "installation-public-key-base64url",
        )
        .unwrap();

        assert_eq!(actual, evidence);
    }

    #[cfg(unix)]
    #[test]
    fn activation_process_failure_does_not_expose_helper_output_or_invocation_material() {
        let root = tempfile::tempdir().unwrap();
        crate::identity::installation_id(root.path()).unwrap();
        let helper = root.path().join("failing-activation-fixture");
        write_activation_fixture(
            &helper,
            r#"
printf '%s' 'fixture-stdout-must-not-escape'
printf '%s' 'fixture-stderr-must-not-escape' >&2
exit 41
"#,
        );

        let error = collect_device_evidence(
            root.path(),
            &helper,
            "challenge-must-not-escape",
            "public-key-must-not-escape",
        )
        .unwrap_err();
        let message = error.to_string();

        assert_eq!(
            message,
            "helper_crashed: Pro activation evidence is unavailable"
        );
        for forbidden in [
            "fixture-stdout-must-not-escape",
            "fixture-stderr-must-not-escape",
            "challenge-must-not-escape",
            "public-key-must-not-escape",
            root.path().to_string_lossy().as_ref(),
        ] {
            assert!(
                !message.contains(forbidden),
                "{forbidden} escaped: {message}"
            );
        }
    }
}
