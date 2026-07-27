use std::{
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_pro_host_protocol::{
    base64url, installation_key_thumbprint, SignedEntitlement, INSTALLATION_PUBLIC_KEY_BYTES,
};
use zeroize::Zeroize as _;

use super::{
    artifact_delivery::{fetch_latest, CommercialArtifactAuth},
    authorization::InstallationChallengeSigner,
    commercial_api::{
        checkout_retry_after, is_retryable_checkout_failure, validate_https_url, CheckoutResult,
        CommercialApiClient, CommercialState,
    },
    commercial_config::CommercialConfig,
    credential_vault::{
        BoundedSignedEntitlement, CredentialRecord, CredentialRecordKind, CredentialVaultError,
        PlatformCredentialVault, VaultInstallationChallengeSigner, WorkOsSessionMaterial,
    },
    lifecycle::{ProLifecycleService, ProManagePlan, ProSetupPlan},
    workos_device::{WorkOsDeviceClient, WorkOsTokens},
};

pub(super) struct CommercialLifecycleService {
    config: CommercialConfig,
    workos: WorkOsDeviceClient,
    api: CommercialApiClient,
    vault: PlatformCredentialVault,
}

const ENTITLEMENT_REFRESH_RETRY_SECONDS: i64 = 60 * 60;
// Keep account checks responsive without busy-polling the service.
const CHECKOUT_POLL_INITIAL_SECONDS: u64 = 3;
const CHECKOUT_POLL_MAX_INTERVAL_SECONDS: u64 = 15;
const CHECKOUT_POLL_PROGRESS_SECONDS: u64 = 60;
const CHECKOUT_POLL_MAX_SECONDS: u64 = 30 * 60;

impl CommercialLifecycleService {
    pub(super) fn production(data_root: &Path) -> Result<Self> {
        let config = CommercialConfig::production()?;
        let workos = WorkOsDeviceClient::new(config.workos.clone())?;
        let api = CommercialApiClient::new(config.api.clone())?;
        let vault = PlatformCredentialVault::production(data_root, config.vault_namespace)
            .map_err(vault_error)?;
        Ok(Self {
            config,
            workos,
            api,
            vault,
        })
    }

    pub(super) fn refresh_entitlement_if_due(data_root: &Path) -> Result<()> {
        let service = Self::production(data_root)?;
        let current = match service.vault.load(CredentialRecordKind::SignedEntitlement) {
            Ok(CredentialRecord::SignedEntitlement(value)) => value.as_inner().clone(),
            Ok(_) => bail!("key_store_unavailable: entitlement record mismatch"),
            Err(CredentialVaultError::NotFound) => {
                bail!("entitlement_required: run `ctx pro`")
            }
            Err(error) => return Err(vault_error(error)),
        };
        let now = unix_time()?;
        if now < current.grant.refresh_after_unix {
            return Ok(());
        }
        if let Ok(CredentialRecord::WorkOsSession(session)) =
            service.vault.load(CredentialRecordKind::WorkOsSession)
        {
            if session
                .entitlement_refresh_not_before_unix()
                .is_some_and(|not_before| now < not_before)
            {
                return entitlement_still_usable(&current, now);
            }
        }
        match service.refresh_entitlement_noninteractive() {
            Ok(()) => Ok(()),
            Err(_) => {
                service.defer_entitlement_refresh(now);
                entitlement_still_usable(&current, now)
            }
        }
    }

    fn access_token(&self) -> Result<String> {
        match self.vault.load(CredentialRecordKind::WorkOsSession) {
            Ok(CredentialRecord::WorkOsSession(session)) => self.resume_session(session),
            Ok(_) => Err(anyhow!(
                "key_store_unavailable: WorkOS session record mismatch"
            )),
            Err(CredentialVaultError::NotFound) => self.device_sign_in(),
            Err(CredentialVaultError::Corrupt) => {
                self.delete_if_present(CredentialRecordKind::WorkOsSession)?;
                self.device_sign_in()
            }
            Err(error) => Err(vault_error(error)),
        }
    }

    fn resume_session(&self, session: WorkOsSessionMaterial) -> Result<String> {
        if session.access_expires_at_unix() > unix_time()?
            && self
                .workos
                .validate_access_token(session.access_token())
                .is_ok()
        {
            return Ok(session.access_token().to_owned());
        }
        let Some(refresh_token) = session.refresh_token() else {
            return self.device_sign_in();
        };
        match self.workos.refresh(refresh_token) {
            Ok(tokens) => self.persist_tokens(tokens),
            Err(error) if error.to_string().starts_with("authentication_failed:") => {
                self.device_sign_in()
            }
            Err(error) => Err(error),
        }
    }

    fn access_token_noninteractive(&self) -> Result<String> {
        let session = match self.vault.load(CredentialRecordKind::WorkOsSession) {
            Ok(CredentialRecord::WorkOsSession(session)) => session,
            Ok(_) => bail!("key_store_unavailable: WorkOS session record mismatch"),
            Err(CredentialVaultError::NotFound) => {
                bail!("authentication_required: run `ctx pro`")
            }
            Err(error) => return Err(vault_error(error)),
        };
        if session.access_expires_at_unix() > unix_time()?
            && self
                .workos
                .validate_access_token(session.access_token())
                .is_ok()
        {
            return Ok(session.access_token().to_owned());
        }
        let refresh_token = session
            .refresh_token()
            .ok_or_else(|| anyhow!("authentication_required: run `ctx pro`"))?;
        self.persist_tokens(self.workos.refresh(refresh_token)?)
    }

    fn device_sign_in(&self) -> Result<String> {
        let authorization = self.workos.begin()?;
        eprintln!("Sign in to ctx Pro at:");
        eprintln!("  {}", authorization.verification_uri);
        eprintln!("Enter code: {}", authorization.user_code);
        if open_browser(&authorization.verification_uri_complete).is_err() {
            eprintln!("A browser could not be opened; use the URL and code above.");
        }
        self.persist_tokens(self.workos.poll(&authorization)?)
    }

    fn persist_tokens(&self, tokens: WorkOsTokens) -> Result<String> {
        let expires_at = self.workos.access_token_expiration(&tokens.access_token)?;
        let access_token = tokens.access_token.clone();
        let session = WorkOsSessionMaterial::new(
            tokens.access_token.clone(),
            Some(tokens.refresh_token.clone()),
            expires_at,
        )
        .map_err(vault_error)?;
        self.vault
            .store(&CredentialRecord::WorkOsSession(session))
            .map_err(vault_error)?;
        Ok(access_token)
    }

    fn active_state(&self, access_token: &str) -> Result<CommercialState> {
        resolve_active_state(
            || self.api.account(access_token),
            || self.api.checkout(access_token),
            |state, checkout| self.await_checkout_access(access_token, state, checkout),
        )
    }

    fn await_checkout_access(
        &self,
        access_token: &str,
        initial_state: &CommercialState,
        checkout: CheckoutResult,
    ) -> Result<CommercialState> {
        let expires_at_unix = checkout
            .expires_at_unix
            .ok_or_else(|| anyhow!("invalid_response: Checkout result has no expiry"))?;
        if let Some(url) = checkout.url {
            validate_https_url(&url, "Checkout")?;
            eprintln!("Start your 14-day ctx Pro trial at:");
            eprintln!("  {url}");
            if open_browser(&url).is_err() {
                eprintln!("A browser could not be opened; use the URL above.");
            }
            eprintln!("Waiting for Checkout to complete...");
        } else {
            eprintln!("Checkout completed. Waiting for subscription state...");
        }
        let poll_started = Instant::now();
        let state = poll_checkout_access(
            expires_at_unix,
            unix_time,
            || poll_started.elapsed(),
            thread::sleep,
            || match self.api.account(access_token) {
                Ok(state)
                    if state.subject != initial_state.subject
                        || state.account_id != initial_state.account_id =>
                {
                    CheckoutPoll::Fatal(anyhow!(
                        "invalid_response: commercial account changed during Checkout"
                    ))
                }
                Ok(state) if state.grants_access() => CheckoutPoll::Granted(state),
                Ok(_) => CheckoutPoll::Pending,
                Err(error) if is_retryable_checkout_failure(&error) => {
                    CheckoutPoll::Retryable(checkout_retry_after(&error))
                }
                Err(error) => CheckoutPoll::Fatal(error),
            },
            |elapsed_seconds| {
                eprintln!(
                    "Still waiting for Checkout... {} minute(s) elapsed.",
                    elapsed_seconds / 60
                );
            },
        )?;
        eprintln!("Checkout complete. Finishing ctx Pro...");
        Ok(state)
    }

    fn installation_public_key(&self) -> Result<[u8; INSTALLATION_PUBLIC_KEY_BYTES]> {
        self.vault
            .load_or_create_installation_signing_key()
            .map_err(vault_error)?;
        VaultInstallationChallengeSigner::new(&self.vault)
            .public_key()
            .context("key_store_unavailable: load installation public key")
    }

    fn store_entitlement(
        &self,
        entitlement: SignedEntitlement,
        public_key: &[u8; INSTALLATION_PUBLIC_KEY_BYTES],
    ) -> Result<()> {
        self.config
            .entitlement_trust
            .validate_identity(&entitlement.grant.issuer, &entitlement.grant.key_id)?;
        if entitlement.grant.installation_key_thumbprint != installation_key_thumbprint(public_key)
        {
            bail!("invalid_response: entitlement is bound to another installation");
        }
        let entitlement = BoundedSignedEntitlement::new(entitlement).map_err(vault_error)?;
        self.vault
            .store(&CredentialRecord::SignedEntitlement(entitlement))
            .map_err(vault_error)
    }

    fn refresh_entitlement_noninteractive(&self) -> Result<()> {
        let mut access_token = self.access_token_noninteractive()?;
        let result = (|| {
            let state = self.api.account(&access_token)?;
            if !state.grants_access() {
                bail!("commercial_access_locked: an active trial or subscription is required");
            }
            let public_key = VaultInstallationChallengeSigner::new(&self.vault)
                .public_key()
                .context("key_store_unavailable: load installation public key")?;
            let entitlement = self
                .api
                .entitlement(&access_token, &base64url(&public_key))?;
            if entitlement.grant.subject != state.subject
                || entitlement.grant.account_id != state.account_id
            {
                bail!("invalid_response: entitlement identity does not match commercial account");
            }
            self.store_entitlement(entitlement, &public_key)?;
            self.clear_refresh_backoff();
            Ok(())
        })();
        access_token.zeroize();
        result
    }

    fn clear_refresh_backoff(&self) {
        let Ok(CredentialRecord::WorkOsSession(session)) =
            self.vault.load(CredentialRecordKind::WorkOsSession)
        else {
            return;
        };
        let Ok(session) = session.with_entitlement_refresh_not_before_unix(None) else {
            return;
        };
        let _ = self.vault.store(&CredentialRecord::WorkOsSession(session));
    }

    fn defer_entitlement_refresh(&self, now: i64) {
        let Ok(CredentialRecord::WorkOsSession(session)) =
            self.vault.load(CredentialRecordKind::WorkOsSession)
        else {
            return;
        };
        let Ok(session) = session.with_entitlement_refresh_not_before_unix(Some(
            now.saturating_add(ENTITLEMENT_REFRESH_RETRY_SECONDS),
        )) else {
            return;
        };
        let _ = self.vault.store(&CredentialRecord::WorkOsSession(session));
    }

    fn delete_if_present(&self, kind: CredentialRecordKind) -> Result<()> {
        match self.vault.delete(kind) {
            Ok(()) | Err(CredentialVaultError::NotFound) => Ok(()),
            Err(error) => Err(vault_error(error)),
        }
    }
}

impl ProLifecycleService for CommercialLifecycleService {
    fn release_trust(&self) -> Result<super::lifecycle::lifecycle_manifest::ReleaseTrust> {
        Ok(self.config.release_trust)
    }

    fn setup(&mut self, data_root: &Path, installed_version: Option<&str>) -> Result<ProSetupPlan> {
        let mut access_token = self.access_token()?;
        let result = (|| {
            let state = self.active_state(&access_token)?;
            let public_key = self.installation_public_key()?;
            let entitlement = self
                .api
                .entitlement(&access_token, &base64url(&public_key))?;
            if entitlement.grant.subject != state.subject
                || entitlement.grant.account_id != state.account_id
            {
                bail!("invalid_response: entitlement identity does not match commercial account");
            }
            self.store_entitlement(entitlement, &public_key)?;
            let artifact = fetch_latest(
                data_root,
                CommercialArtifactAuth {
                    api_base_url: self.api.origin(),
                    access_token: &access_token,
                    release_trust: self.config.release_trust,
                },
                installed_version,
            )?;
            Ok(ProSetupPlan {
                artifact: Some(artifact),
                account_state: state.access_state,
            })
        })();
        access_token.zeroize();
        result
    }

    fn manage(&mut self, _data_root: &Path) -> Result<ProManagePlan> {
        let mut access_token = self.access_token()?;
        let result = (|| {
            let state = self.api.account(&access_token)?;
            let portal = self.api.portal(&access_token)?;
            let (refresh_after_unix, access_deadline_unix, grace_deadline_unix) =
                self.local_entitlement_deadlines(&state)?;
            Ok(ProManagePlan {
                portal_url: portal.url,
                access_state: match state.access_state.as_str() {
                    "trial" => "trial",
                    "active" => "active",
                    "canceling_paid" => "canceling_paid",
                    "locked" | "none" => "locked",
                    _ => bail!("invalid_response: commercial access state is invalid"),
                }
                .to_owned(),
                refresh_after_unix,
                access_deadline_unix,
                grace_deadline_unix,
            })
        })();
        access_token.zeroize();
        result
    }
}

impl CommercialLifecycleService {
    fn local_entitlement_deadlines(
        &self,
        state: &CommercialState,
    ) -> Result<(Option<i64>, Option<i64>, Option<i64>)> {
        if !state.grants_access() {
            return Ok((None, None, None));
        }
        match self.vault.load(CredentialRecordKind::SignedEntitlement) {
            Ok(CredentialRecord::SignedEntitlement(entitlement)) => {
                let grant = &entitlement.as_inner().grant;
                if grant.subject != state.subject || grant.account_id != state.account_id {
                    bail!("key_store_unavailable: entitlement identity mismatch");
                }
                Ok((
                    Some(grant.refresh_after_unix),
                    Some(grant.access_deadline_unix),
                    Some(grant.grace_deadline_unix),
                ))
            }
            Ok(_) => bail!("key_store_unavailable: entitlement record mismatch"),
            Err(CredentialVaultError::NotFound) => Ok((None, state.access_deadline_unix, None)),
            Err(error) => Err(vault_error(error)),
        }
    }
}

fn resolve_active_state(
    mut account: impl FnMut() -> Result<CommercialState>,
    mut checkout: impl FnMut() -> Result<CheckoutResult>,
    mut await_checkout: impl FnMut(&CommercialState, CheckoutResult) -> Result<CommercialState>,
) -> Result<CommercialState> {
    let state = account()?;
    if state.grants_access() {
        return Ok(state);
    }
    let checkout = checkout()?;
    match checkout.kind.as_str() {
        "already_subscribed" => {
            let state = checkout.state.ok_or_else(|| {
                anyhow!("invalid_response: subscription result has no account state")
            })?;
            if state.grants_access() {
                return Ok(state);
            }
        }
        "checkout_created" | "checkout_pending" => return await_checkout(&state, checkout),
        _ => bail!("invalid_response: commercial API returned an invalid Checkout result"),
    }
    bail!("commercial_access_locked: an active trial or subscription is required")
}

enum CheckoutPoll<T> {
    Granted(T),
    Pending,
    Retryable(Option<Duration>),
    Fatal(anyhow::Error),
}

fn poll_checkout_access<T>(
    checkout_expires_at_unix: i64,
    mut now: impl FnMut() -> Result<i64>,
    mut elapsed: impl FnMut() -> Duration,
    mut sleep: impl FnMut(Duration),
    mut account: impl FnMut() -> CheckoutPoll<T>,
    mut progress: impl FnMut(u64),
) -> Result<T> {
    let maximum_elapsed = Duration::from_secs(CHECKOUT_POLL_MAX_SECONDS);
    let mut next_progress = Duration::from_secs(CHECKOUT_POLL_PROGRESS_SECONDS);
    let mut next_poll_eligible = Duration::ZERO;
    let mut poll_interval = Duration::from_secs(CHECKOUT_POLL_INITIAL_SECONDS);
    loop {
        let current_unix = now()?;
        if current_unix >= checkout_expires_at_unix {
            return Err(checkout_poll_ended(true));
        }
        let current_elapsed = elapsed();
        if current_elapsed >= maximum_elapsed {
            return Err(checkout_poll_ended(false));
        }
        if current_elapsed < next_poll_eligible {
            if current_elapsed >= next_progress {
                progress(current_elapsed.as_secs());
                next_progress = current_elapsed
                    .saturating_add(Duration::from_secs(CHECKOUT_POLL_PROGRESS_SECONDS));
            }
            let checkout_remaining = Duration::from_secs(
                u64::try_from(checkout_expires_at_unix - current_unix).unwrap_or(0),
            );
            let maximum_remaining = maximum_elapsed.saturating_sub(current_elapsed);
            let poll_remaining = next_poll_eligible.saturating_sub(current_elapsed);
            let progress_remaining = next_progress.saturating_sub(current_elapsed);
            sleep(
                poll_remaining
                    .min(progress_remaining)
                    .min(checkout_remaining)
                    .min(maximum_remaining),
            );
            continue;
        }
        let outcome = account();
        let current_unix = now()?;
        if current_unix >= checkout_expires_at_unix {
            return Err(checkout_poll_ended(true));
        }
        let current_elapsed = elapsed();
        if current_elapsed >= maximum_elapsed {
            return Err(checkout_poll_ended(false));
        }
        let retry_after = match outcome {
            CheckoutPoll::Granted(value) => return Ok(value),
            CheckoutPoll::Pending => None,
            CheckoutPoll::Retryable(retry_after) => retry_after,
            CheckoutPoll::Fatal(error) => return Err(error),
        };
        if current_elapsed >= next_progress {
            progress(current_elapsed.as_secs());
            next_progress =
                current_elapsed.saturating_add(Duration::from_secs(CHECKOUT_POLL_PROGRESS_SECONDS));
        }
        let requested_interval =
            retry_after.map_or(poll_interval, |retry_after| poll_interval.max(retry_after));
        next_poll_eligible = current_elapsed.saturating_add(requested_interval);
        poll_interval = poll_interval
            .saturating_mul(2)
            .min(Duration::from_secs(CHECKOUT_POLL_MAX_INTERVAL_SECONDS));
    }
}

fn checkout_poll_ended(expired: bool) -> anyhow::Error {
    if expired {
        anyhow!(
            "checkout_expired: Checkout expired before access was granted; rerun `ctx pro` to safely resume setup"
        )
    } else {
        anyhow!(
            "checkout_timeout: Checkout did not complete within 30 minutes; rerun `ctx pro` to safely resume setup"
        )
    }
}

fn open_browser(url: &str) -> Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(url);
        command
    } else if cfg!(windows) {
        let mut command = Command::new("rundll32.exe");
        command.args(["url.dll,FileProtocolHandler", url]);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("open browser")?;
    Ok(())
}

pub(super) fn vault_error(error: CredentialVaultError) -> anyhow::Error {
    let code = match error {
        CredentialVaultError::Locked => "key_store_locked",
        CredentialVaultError::Unavailable { .. }
        | CredentialVaultError::NotFound
        | CredentialVaultError::Corrupt
        | CredentialVaultError::Ambiguous
        | CredentialVaultError::InvalidRecordId
        | CredentialVaultError::InvalidDataRoot
        | CredentialVaultError::EntropyUnavailable
        | CredentialVaultError::SecretTooLarge { .. }
        | CredentialVaultError::Backend => "key_store_unavailable",
    };
    anyhow!("{code}: {error}")
}

fn entitlement_still_usable(entitlement: &SignedEntitlement, now: i64) -> Result<()> {
    if now <= entitlement.grant.grace_deadline_unix {
        Ok(())
    } else {
        bail!("entitlement_expired: run `ctx pro manage` to restore access")
    }
}

fn unix_time() -> Result<i64> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("invalid_request: system clock is before Unix epoch")?
            .as_secs(),
    )
    .context("invalid_request: system time is invalid")
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use crate::pro::commercial_api::BillingState;

    use super::*;

    fn elapsed_since(clock: &Cell<i64>, start: i64) -> Duration {
        Duration::from_secs(u64::try_from(clock.get() - start).unwrap())
    }

    fn commercial_state(access_state: &str, access_deadline_unix: Option<i64>) -> CommercialState {
        CommercialState {
            subject: "user_123".to_owned(),
            account_id: "org_123".to_owned(),
            access_state: access_state.to_owned(),
            access_deadline_unix,
            billing: BillingState {
                customer_associated: true,
                subscription_status: None,
                trial_end_unix: None,
                current_period_end_unix: None,
                cancel_at_period_end: false,
                canceled_at_unix: None,
                latest_invoice_status: None,
                latest_payment_state: "unknown".to_owned(),
            },
        }
    }

    #[test]
    fn already_subscribed_uses_embedded_state_without_a_second_account_request() {
        let account_calls = Cell::new(0_u32);
        let checkout_calls = Cell::new(0_u32);
        let await_calls = Cell::new(0_u32);
        let initial = commercial_state("none", None);
        let subscribed = commercial_state("active", Some(2_000));

        let state = resolve_active_state(
            || {
                account_calls.set(account_calls.get() + 1);
                if account_calls.get() > 1 {
                    bail!("unexpected second account request");
                }
                Ok(initial.clone())
            },
            || {
                checkout_calls.set(checkout_calls.get() + 1);
                Ok(CheckoutResult {
                    kind: "already_subscribed".to_owned(),
                    url: None,
                    expires_at_unix: None,
                    state: Some(subscribed.clone()),
                })
            },
            |_, _| {
                await_calls.set(await_calls.get() + 1);
                bail!("Checkout polling must not run for an existing subscription")
            },
        )
        .unwrap();

        assert_eq!(state.access_state, "active");
        assert_eq!(account_calls.get(), 1);
        assert_eq!(checkout_calls.get(), 1);
        assert_eq!(await_calls.get(), 0);
    }

    #[test]
    fn url_less_pending_checkout_uses_the_polling_path() {
        let await_calls = Cell::new(0_u32);
        let initial = commercial_state("none", None);
        let active = commercial_state("active", Some(2_000));

        let state = resolve_active_state(
            || Ok(initial.clone()),
            || {
                Ok(CheckoutResult {
                    kind: "checkout_pending".to_owned(),
                    url: None,
                    expires_at_unix: Some(1_500),
                    state: None,
                })
            },
            |observed, checkout| {
                await_calls.set(await_calls.get() + 1);
                assert_eq!(observed.access_state, "none");
                assert_eq!(checkout.kind, "checkout_pending");
                assert!(checkout.url.is_none());
                assert_eq!(checkout.expires_at_unix, Some(1_500));
                Ok(active.clone())
            },
        )
        .unwrap();

        assert_eq!(state.access_state, "active");
        assert_eq!(await_calls.get(), 1);
    }

    #[test]
    fn key_store_error_vocabulary_is_exact_and_does_not_expose_old_tokens() {
        let cases = [
            (
                CredentialVaultError::Unavailable { platform: "linux" },
                "key_store_unavailable:",
            ),
            (CredentialVaultError::Corrupt, "key_store_unavailable:"),
            (CredentialVaultError::Locked, "key_store_locked:"),
        ];
        for (error, expected) in cases {
            let rendered = vault_error(error).to_string();
            assert!(rendered.starts_with(expected));
            assert!(!rendered.contains("credential_vault_"));
        }
    }

    #[test]
    fn browser_urls_are_validated_before_launch() {
        assert!(validate_https_url("file:///tmp/session", "Checkout").is_err());
        assert!(validate_https_url("https://checkout.stripe.com/session", "Checkout").is_ok());
    }

    #[test]
    fn checkout_poll_uses_bounded_adaptive_backoff_without_real_sleep() {
        let clock = Cell::new(1_000_i64);
        let sleeps = RefCell::new(Vec::new());
        let attempts = Cell::new(0_u32);
        let result = poll_checkout_access(
            2_000,
            || Ok(clock.get()),
            || elapsed_since(&clock, 1_000),
            |duration| {
                sleeps.borrow_mut().push(duration);
                clock.set(clock.get() + i64::try_from(duration.as_secs()).unwrap());
            },
            || {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                match attempt {
                    1 => CheckoutPoll::Retryable(None),
                    2..=5 => CheckoutPoll::Pending,
                    _ => CheckoutPoll::Granted("active"),
                }
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(result, "active");
        assert_eq!(attempts.get(), 6);
        assert_eq!(
            sleeps.into_inner(),
            [
                Duration::from_secs(3),
                Duration::from_secs(6),
                Duration::from_secs(12),
                Duration::from_secs(15),
                Duration::from_secs(15),
            ]
        );
    }

    #[test]
    fn checkout_poll_respects_retry_after_across_progress_wakes() {
        let clock = Cell::new(1_000_i64);
        let sleeps = RefCell::new(Vec::new());
        let attempts = Cell::new(0_u32);
        let result = poll_checkout_access(
            2_000,
            || Ok(clock.get()),
            || elapsed_since(&clock, 1_000),
            |duration| {
                sleeps.borrow_mut().push(duration);
                clock.set(clock.get() + i64::try_from(duration.as_secs()).unwrap());
            },
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() == 1 {
                    CheckoutPoll::Retryable(Some(Duration::from_secs(120)))
                } else {
                    CheckoutPoll::Granted("active")
                }
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(result, "active");
        assert_eq!(attempts.get(), 2);
        assert_eq!(
            sleeps.into_inner(),
            [Duration::from_secs(60), Duration::from_secs(60)]
        );
    }

    #[test]
    fn checkout_poll_stops_at_expiry_or_thirty_minutes() {
        for (expires_at, expected_error, expected_elapsed) in [
            (1_005, "checkout_expired:", 5_i64),
            (
                5_000,
                "checkout_timeout:",
                i64::try_from(CHECKOUT_POLL_MAX_SECONDS).unwrap(),
            ),
        ] {
            let clock = Cell::new(1_000_i64);
            let attempts = Cell::new(0_u32);
            let error = poll_checkout_access::<()>(
                expires_at,
                || Ok(clock.get()),
                || elapsed_since(&clock, 1_000),
                |duration| {
                    clock.set(clock.get() + i64::try_from(duration.as_secs()).unwrap());
                },
                || {
                    attempts.set(attempts.get() + 1);
                    CheckoutPoll::Pending
                },
                |_| {},
            )
            .unwrap_err();
            assert!(error.to_string().starts_with(expected_error), "{error:#}");
            assert!(error.to_string().contains("rerun `ctx pro`"));
            assert_eq!(clock.get() - 1_000, expected_elapsed);
            assert!(attempts.get() > 0);
        }
    }

    #[test]
    fn checkout_poll_rejects_a_grant_returned_after_the_deadline() {
        let clock = Cell::new(1_000_i64);
        let error = poll_checkout_access(
            1_005,
            || Ok(clock.get()),
            || elapsed_since(&clock, 1_000),
            |_| {},
            || {
                clock.set(1_005);
                CheckoutPoll::Granted("late grant")
            },
            |_| {},
        )
        .unwrap_err();
        assert!(error.to_string().starts_with("checkout_expired:"));
    }

    #[test]
    fn checkout_poll_timeout_is_monotonic_when_the_wall_clock_moves_backward() {
        let wall_clock = Cell::new(1_000_i64);
        let elapsed = Cell::new(Duration::ZERO);
        let error = poll_checkout_access::<()>(
            5_000,
            || Ok(wall_clock.get()),
            || elapsed.get(),
            |duration| {
                elapsed.set(elapsed.get().saturating_add(duration));
                wall_clock.set(wall_clock.get().saturating_sub(1));
            },
            || CheckoutPoll::Pending,
            |_| {},
        )
        .unwrap_err();
        assert!(error.to_string().starts_with("checkout_timeout:"));
        assert_eq!(
            elapsed.get(),
            Duration::from_secs(CHECKOUT_POLL_MAX_SECONDS)
        );
    }

    #[test]
    fn checkout_poll_fails_closed_and_reports_bounded_progress() {
        let clock = Cell::new(1_000_i64);
        let progress = RefCell::new(Vec::new());
        let error = poll_checkout_access::<()>(
            2_000,
            || Ok(clock.get()),
            || elapsed_since(&clock, 1_000),
            |duration| {
                clock.set(clock.get() + i64::try_from(duration.as_secs()).unwrap());
            },
            || {
                if clock.get() < 1_063 {
                    CheckoutPoll::Pending
                } else {
                    CheckoutPoll::Fatal(anyhow!("authentication_required: sign in again"))
                }
            },
            |elapsed| progress.borrow_mut().push(elapsed),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "authentication_required: sign in again");
        assert_eq!(progress.into_inner(), [60]);
        assert_eq!(clock.get(), 1_066);
    }
}
