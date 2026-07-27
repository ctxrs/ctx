use std::{
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_pro_host_protocol::{
    base64url, installation_key_thumbprint, EntitlementAccessKind, SignedEntitlement,
    INSTALLATION_PUBLIC_KEY_BYTES,
};
use zeroize::Zeroize as _;

use super::{
    anonymous_trial,
    artifact_delivery::{fetch_latest, CommercialArtifactAuth},
    authorization::InstallationChallengeSigner,
    commercial_api::{
        checkout_retry_after, is_retryable_checkout_failure, validate_https_url, CheckoutResult,
        CommercialApiClient, CommercialState,
    },
    commercial_config::CommercialConfig,
    credential_vault::{
        AnonymousTrialMaterial, BoundedSignedEntitlement, CredentialRecord, CredentialRecordKind,
        CredentialVaultError, PlatformCredentialVault, VaultInstallationChallengeSigner,
        WorkOsSessionMaterial,
    },
    lifecycle::{ProLifecycleService, ProManagePlan, ProSetupPlan},
    workos_device::{WorkOsDeviceClient, WorkOsTokens},
};

pub(super) struct CommercialLifecycleService {
    pub(super) config: CommercialConfig,
    workos: WorkOsDeviceClient,
    pub(super) api: CommercialApiClient,
    pub(super) vault: PlatformCredentialVault,
}

const ENTITLEMENT_REFRESH_RETRY_SECONDS: i64 = 60 * 60;
// Keep account checks responsive without busy-polling the service.
const CHECKOUT_POLL_INITIAL_SECONDS: u64 = 3;
const CHECKOUT_POLL_MAX_INTERVAL_SECONDS: u64 = 15;
const CHECKOUT_POLL_PROGRESS_SECONDS: u64 = 60;
const CHECKOUT_POLL_MAX_SECONDS: u64 = 30 * 60;
const PAID_CHECKOUT_HEADING: &str = "Start ctx Pro for $20/month at:";

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
        if matches!(
            service.vault.load(CredentialRecordKind::AnonymousTrial),
            Ok(CredentialRecord::AnonymousTrial(_))
        ) {
            return match anonymous_trial::refresh_entitlement(&service) {
                Ok(()) => Ok(()),
                Err(_) => {
                    anonymous_trial::defer_refresh(&service, now);
                    entitlement_still_usable(&current, now)
                }
            };
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

    pub(super) fn access_token(&self) -> Result<String> {
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

    pub(super) fn access_token_noninteractive(&self) -> Result<String> {
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

    fn active_state(
        &self,
        access_token: &str,
        referral_claim_token: Option<&str>,
    ) -> Result<CommercialState> {
        resolve_active_state(
            || self.api.account(access_token),
            || self.api.checkout(access_token, referral_claim_token),
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
            eprintln!("{}", render_paid_checkout_prompt(&url));
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

    pub(super) fn installation_public_key(&self) -> Result<[u8; INSTALLATION_PUBLIC_KEY_BYTES]> {
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
        let entitlement = self.bound_entitlement(entitlement, public_key)?;
        self.vault
            .store(&CredentialRecord::SignedEntitlement(entitlement))
            .map_err(vault_error)
    }

    fn bound_entitlement(
        &self,
        entitlement: SignedEntitlement,
        public_key: &[u8; INSTALLATION_PUBLIC_KEY_BYTES],
    ) -> Result<BoundedSignedEntitlement> {
        self.config
            .entitlement_trust
            .validate_identity(&entitlement.grant.issuer, &entitlement.grant.key_id)?;
        if entitlement.grant.installation_key_thumbprint != installation_key_thumbprint(public_key)
        {
            bail!("invalid_response: entitlement is bound to another installation");
        }
        BoundedSignedEntitlement::new(entitlement).map_err(vault_error)
    }

    pub(super) fn store_anonymous_state(
        &self,
        entitlement: SignedEntitlement,
        public_key: &[u8; INSTALLATION_PUBLIC_KEY_BYTES],
        trial_deadline_unix: i64,
        trial: AnonymousTrialMaterial,
    ) -> Result<()> {
        if entitlement.grant.access_kind != EntitlementAccessKind::Trial
            || entitlement.grant.access_deadline_unix != trial_deadline_unix
            || entitlement.grant.grace_deadline_unix != trial_deadline_unix
            || entitlement.grant.expires_at_unix > trial_deadline_unix
        {
            bail!("invalid_response: anonymous entitlement exceeds the authoritative trial");
        }
        let entitlement = self.bound_entitlement(entitlement, public_key)?;
        self.vault
            .store_anonymous_trial_state(entitlement, trial)
            .map_err(|error| match error {
                CredentialVaultError::SecretTooLarge { .. } => {
                    anyhow!("invalid_response: anonymous trial state exceeds portable vault bounds")
                }
                error => vault_error(error),
            })
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

    fn setup(
        &mut self,
        data_root: &Path,
        installed_version: Option<&str>,
        trial_only: bool,
        referral_codename: Option<&str>,
    ) -> Result<ProSetupPlan> {
        // A WorkOS session proves identity for referral management; it does not
        // prove that this installation already consumed or converted its Pro
        // trial. The anonymous-trial authority remains the source of truth.
        let stored_access_kind = match self.vault.load(CredentialRecordKind::SignedEntitlement) {
            Ok(CredentialRecord::SignedEntitlement(entitlement)) => {
                Some(entitlement.as_inner().grant.access_kind)
            }
            Ok(_) => bail!("key_store_unavailable: entitlement record mismatch"),
            Err(CredentialVaultError::NotFound) => None,
            Err(error) => return Err(vault_error(error)),
        };
        setup_with_access_policy(
            trial_only,
            referral_codename.is_some(),
            stored_access_kind,
            || anonymous_trial::setup(self, data_root, installed_version, referral_codename),
            || self.setup_with_paid_access(data_root, installed_version),
        )
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

fn setup_with_access_policy<T>(
    trial_only: bool,
    referral_requested: bool,
    stored_access_kind: Option<EntitlementAccessKind>,
    anonymous_setup: impl FnOnce() -> Result<T>,
    paid_setup: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let anonymous_precedes_paid = anonymous_trial_precedes_paid_conversion(stored_access_kind);
    if referral_requested && !anonymous_precedes_paid {
        bail!("invalid_request: a referral can only start a new anonymous Pro trial");
    }
    if trial_only || referral_requested || anonymous_precedes_paid {
        match anonymous_setup() {
            Ok(plan) => return Ok(plan),
            Err(error)
                if !trial_only
                    && !referral_requested
                    && anonymous_trial_requires_conversion(&error) =>
            {
                eprintln!(
                    "The free Pro trial is unavailable for this device; sign in to continue with paid Pro."
                );
            }
            Err(error) => return Err(error),
        }
    }
    paid_setup()
}

impl CommercialLifecycleService {
    fn setup_with_paid_access(
        &self,
        data_root: &Path,
        installed_version: Option<&str>,
    ) -> Result<ProSetupPlan> {
        let mut access_token = self.access_token()?;
        let mut referral_claim_token = self.checkout_referral_claim_token()?;
        let result = (|| {
            let state = self.active_state(&access_token, referral_claim_token.as_deref())?;
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
                    authorization: &format!("Bearer {access_token}"),
                    release_trust: self.config.release_trust,
                },
                installed_version,
            )?;
            Ok(ProSetupPlan {
                artifact: Some(artifact),
                account_state: state.access_state,
            })
        })();
        referral_claim_token.zeroize();
        access_token.zeroize();
        result
    }
}

fn anonymous_trial_requires_conversion(error: &anyhow::Error) -> bool {
    matches!(
        crate::pro::stable_error_code(error),
        Some(
            "anonymous_trial_already_consumed"
                | "anonymous_trial_identity_ambiguous"
                | "anonymous_trial_installation_limit"
                | "commercial_access_locked"
        )
    )
}

fn anonymous_trial_precedes_paid_conversion(
    stored_access_kind: Option<EntitlementAccessKind>,
) -> bool {
    matches!(
        stored_access_kind,
        None | Some(EntitlementAccessKind::Trial)
    )
}

impl CommercialLifecycleService {
    fn checkout_referral_claim_token(&self) -> Result<Option<String>> {
        match self.vault.load(CredentialRecordKind::AnonymousTrial) {
            Ok(CredentialRecord::AnonymousTrial(trial)) => {
                Ok(trial.referral_claim_token().map(ToOwned::to_owned))
            }
            Ok(_) => bail!("key_store_unavailable: anonymous trial record mismatch"),
            Err(CredentialVaultError::NotFound) => Ok(None),
            Err(error) => Err(vault_error(error)),
        }
    }

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

pub(super) fn open_browser(url: &str) -> Result<()> {
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

pub(super) fn unix_time() -> Result<i64> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("invalid_request: system clock is before Unix epoch")?
            .as_secs(),
    )
    .context("invalid_request: system time is invalid")
}

fn render_paid_checkout_prompt(url: &str) -> String {
    format!("{PAID_CHECKOUT_HEADING}\n  {url}")
}

#[cfg(test)]
#[path = "commercial_lifecycle/tests.rs"]
mod tests;
