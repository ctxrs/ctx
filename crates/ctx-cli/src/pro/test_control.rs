use std::{
    env, fs,
    fs::{File, OpenOptions},
    io::{Read as _, Seek as _, Write as _},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_core::platform_security::{verify_private_directory, verify_private_file};
use serde::{Deserialize, Serialize};

use super::{
    artifact_delivery::SetupArtifactBundle,
    commercial_api::{ReferralCreateResult, ReferralPayoutResult, ReferralStatusResult},
    commercial_config::{self, test_control_release_trust, TEST_CONTROL_MANIFEST_ENV},
    lifecycle::{ProLifecycleService, ProManagePlan, ProSetupPlan},
    referral::{ReferralAuthMode, ReferralService},
};
use crate::ui::Ui;

mod validation;
use validation::*;

const MAX_MANIFEST_BYTES: u64 = 128 * 1024;
const MAX_FIXTURE_ID_BYTES: usize = 128;
const MAX_SCRIPT_EVENTS: usize = 8;
const MAX_BROWSER_CALLS: usize = 8;
const MAX_CLOCK_VALUES: usize = 32;
const MAX_ERROR_MESSAGE_BYTES: usize = 512;
const MAX_URL_BYTES: usize = 4096;
const MAX_HELPER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PAYOUT_LIFETIME_SECONDS: i64 = 7 * 24 * 60 * 60;

static SESSION: OnceLock<std::result::Result<Option<Arc<ControlSession>>, String>> =
    OnceLock::new();

pub(super) fn prepare() -> Result<()> {
    session().map(|_| ())
}

pub(super) fn is_active() -> Result<bool> {
    Ok(session()?.is_some())
}

pub(super) fn lifecycle_service() -> Result<Option<TestControlService>> {
    Ok(session()?.map(TestControlService::new))
}

pub(super) fn referral_service() -> Result<Option<TestControlService>> {
    Ok(session()?.map(TestControlService::new))
}

pub(super) fn finish<T>(result: Result<T>) -> Result<T> {
    let Some(session) = session()? else {
        return result;
    };
    let command_outcome = if result.is_ok() { "success" } else { "error" };
    match session.finish(command_outcome) {
        Ok(()) => result,
        Err(error) => Err(error),
    }
}

pub(super) fn browser_result_if_active(url: &str) -> Option<Result<()>> {
    match session() {
        Ok(Some(session)) => Some(session.open_browser(url)),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}

pub(super) fn unix_time_if_active() -> Option<Result<i64>> {
    match session() {
        Ok(Some(session)) => Some(session.unix_time()),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}

pub(super) fn helper_path() -> Result<Option<PathBuf>> {
    session()?
        .map(|session| {
            session
                .helper
                .as_ref()
                .map(TestControlHelperBundle::verified_path)
                .transpose()
        })
        .transpose()
        .map(Option::flatten)
}

fn session() -> Result<Option<Arc<ControlSession>>> {
    match SESSION.get_or_init(|| load_session().map_err(|error| format!("{error:#}"))) {
        Ok(session) => Ok(session.clone()),
        Err(message) => Err(anyhow!(message.clone())),
    }
}

fn load_session() -> Result<Option<Arc<ControlSession>>> {
    let Some(path) = env::var_os(TEST_CONTROL_MANIFEST_ENV) else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    let (root, manifest) = read_manifest(&path)?;
    let expected_operation = manifest.validate(&root)?;
    let helper = manifest
        .helper
        .as_ref()
        .map(|helper| TestControlHelperBundle::new(&root, helper))
        .transpose()?;
    let writer = ReceiptWriter::create(
        &root,
        &manifest.browser.receipt,
        ControlReceipt::new(&manifest),
    )?;
    Ok(Some(Arc::new(ControlSession {
        manifest,
        expected_operation,
        helper,
        writer: Mutex::new(writer),
    })))
}

fn read_manifest(path: &Path) -> Result<(PathBuf, ControlManifest)> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("invalid_request: {TEST_CONTROL_MANIFEST_ENV} must be a normalized absolute path");
    }
    let metadata =
        fs::symlink_metadata(path).context("invalid_request: inspect Pro test control manifest")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("invalid_request: Pro test control manifest must be a regular non-symlink file");
    }
    if metadata.len() == 0 || metadata.len() > MAX_MANIFEST_BYTES {
        bail!("invalid_request: Pro test control manifest size is outside allowed bounds");
    }
    verify_private_file(path)
        .context("invalid_request: Pro test control manifest permissions are unsafe")?;
    let canonical = path
        .canonicalize()
        .context("invalid_request: canonicalize Pro test control manifest")?;
    if canonical != path {
        bail!("invalid_request: Pro test control manifest path is not canonical");
    }
    let root = path
        .parent()
        .ok_or_else(|| anyhow!("invalid_request: Pro test control manifest has no observer root"))?
        .to_path_buf();
    let canonical_root = root
        .canonicalize()
        .context("invalid_request: canonicalize Pro test observer root")?;
    if canonical_root != root {
        bail!("invalid_request: Pro test observer root path is not canonical");
    }
    let root_metadata =
        fs::symlink_metadata(&root).context("invalid_request: inspect Pro test observer root")?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        bail!("invalid_request: Pro test observer root must be a non-symlink directory");
    }
    verify_private_directory(&root)
        .context("invalid_request: Pro test observer root permissions are unsafe")?;

    let mut file = File::open(path).context("invalid_request: open Pro test control manifest")?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    std::io::Read::by_ref(&mut file)
        .take(MAX_MANIFEST_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .context("invalid_request: read Pro test control manifest")?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!("invalid_request: Pro test control manifest changed while being read");
    }
    let manifest: ControlManifest = serde_json::from_slice(&bytes)
        .context("invalid_request: parse Pro test control manifest")?;
    let value = serde_json::from_slice(&bytes)
        .context("invalid_request: parse canonical Pro test control JSON")?;
    let mut canonical_bytes = serde_json::to_vec(&sort_json(value))
        .context("invalid_request: encode canonical Pro test control JSON")?;
    canonical_bytes.push(b'\n');
    if bytes != canonical_bytes {
        bail!("invalid_request: Pro test control manifest must use canonical JSON");
    }
    Ok((root, manifest))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ControlManifest {
    schema_version: u16,
    fixture_id: String,
    entitlement_trust: EntitlementTrustControl,
    vault: VaultControl,
    clock: ClockControl,
    browser: BrowserControl,
    helper: Option<HelperControl>,
    lifecycle: LifecycleControl,
    referral: ReferralControl,
}

impl ControlManifest {
    fn validate(&self, root: &Path) -> Result<&'static str> {
        if self.schema_version != 1 {
            bail!("invalid_request: unsupported Pro test control manifest schema");
        }
        validate_identifier(&self.fixture_id, MAX_FIXTURE_ID_BYTES, "fixture id")?;
        commercial_config::EntitlementTrust::validate_test_control_identity(
            &self.entitlement_trust.issuer,
            &self.entitlement_trust.key_id,
        )?;
        if self.clock.unix_seconds.is_empty()
            || self.clock.unix_seconds.len() > MAX_CLOCK_VALUES
            || self.clock.unix_seconds.iter().any(|value| *value <= 0)
        {
            bail!("invalid_request: Pro test clock values are outside allowed bounds");
        }
        if self.browser.outcomes.len() > MAX_BROWSER_CALLS {
            bail!("invalid_request: Pro test browser script exceeds its call bound");
        }
        validate_receipt_name(&self.browser.receipt)?;
        if let Some(helper) = &self.helper {
            TestControlHelperBundle::new(root, helper)?;
        }

        let mut operations = Vec::new();
        if let Some(script) = &self.lifecycle.setup {
            script.validate()?;
            if script
                .success_value()
                .is_some_and(|value| value.helper_artifact)
                && self.helper.is_none()
            {
                bail!("invalid_request: scripted Pro setup requires a digest-bound helper");
            }
            operations.push("lifecycle.setup");
        }
        if let Some(script) = &self.lifecycle.manage {
            script.validate()?;
            if let Some(value) = script.success_value() {
                validate_fixture_url(&value.portal_url, "management URL")?;
            }
            operations.push("lifecycle.manage");
        }
        if let Some(script) = &self.referral.create {
            script.validate()?;
            operations.push("referral.create");
        }
        if let Some(script) = &self.referral.status {
            script.validate()?;
            operations.push("referral.status");
        }
        if let Some(script) = &self.referral.payout {
            script.validate()?;
            if let Some(value) = script.success_value() {
                validate_fixture_url(&value.url, "payout URL")?;
            }
            operations.push("referral.payout");
        }
        if operations.len() != 1 {
            bail!("invalid_request: Pro test control manifest must script exactly one operation");
        }
        Ok(operations[0])
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EntitlementTrustControl {
    issuer: String,
    key_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct VaultControl {
    state: VaultState,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum VaultState {
    InstallationIdentityOnly,
    CommercialCredentialsActive,
    Locked,
    Unavailable,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ClockControl {
    unix_seconds: Vec<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BrowserControl {
    outcomes: Vec<BrowserOutcome>,
    receipt: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BrowserOutcome {
    Success,
    Failure,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HelperControl {
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LifecycleControl {
    setup: Option<Script<SetupValue>>,
    manage: Option<Script<ManageValue>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReferralControl {
    create: Option<Script<ReferralCreateValue>>,
    status: Option<Script<ReferralStatusValue>>,
    payout: Option<Script<ReferralPayoutValue>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Script<T> {
    events: Vec<CommercialEvent>,
    outcome: ScriptOutcome<T>,
}

impl<T> Script<T> {
    fn validate(&self) -> Result<()> {
        if self.events.len() > MAX_SCRIPT_EVENTS {
            bail!("invalid_request: Pro test script exceeds its event bound");
        }
        for event in &self.events {
            event.validate()?;
        }
        if let ScriptOutcome::Error { code, message } = &self.outcome {
            validate_error(code, message)?;
        }
        Ok(())
    }

    fn success_value(&self) -> Option<&T> {
        match &self.outcome {
            ScriptOutcome::Success { value } => Some(value),
            ScriptOutcome::Error { .. } => None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ScriptOutcome<T> {
    Success { value: T },
    Error { code: String, message: String },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CommercialEvent {
    DeviceSignIn {
        verification_uri: String,
        browser_uri: String,
        user_code: String,
    },
    TrialConversion,
    Checkout {
        url: String,
        waiting: bool,
    },
    CheckoutActive,
}

impl CommercialEvent {
    fn validate(&self) -> Result<()> {
        match self {
            Self::DeviceSignIn {
                verification_uri,
                browser_uri,
                user_code,
            } => {
                validate_fixture_url(verification_uri, "device sign-in URL")?;
                validate_fixture_url(browser_uri, "device browser URL")?;
                if !(4..=32).contains(&user_code.len())
                    || !user_code.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-'
                    })
                {
                    bail!("invalid_request: Pro test device code is invalid");
                }
            }
            Self::Checkout { url, .. } => validate_fixture_url(url, "checkout URL")?,
            Self::TrialConversion | Self::CheckoutActive => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SetupValue {
    account_state: String,
    helper_artifact: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManageValue {
    portal_url: String,
    access_state: String,
    refresh_after_unix: Option<i64>,
    access_deadline_unix: Option<i64>,
    grace_deadline_unix: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReferralCreateValue {
    codename: String,
    disposition: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReferralStatusValue {
    codename: String,
    attributed: u64,
    subscribed: u64,
    earned_cents: u64,
    pending_cents: u64,
    manual_review_cents: u64,
    payable_cents: u64,
    processing_cents: u64,
    paid_cents: u64,
    debt_cents: u64,
    currency: String,
    payout_state: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReferralPayoutValue {
    kind: String,
    payout_state: String,
    url: String,
    expires_at_unix: i64,
}

pub(super) struct TestControlService {
    session: Arc<ControlSession>,
}

impl TestControlService {
    fn new(session: Arc<ControlSession>) -> Self {
        Self { session }
    }

    fn emit_events(
        &self,
        events: &[CommercialEvent],
        ui: &mut Ui,
        human_output: bool,
        browser_enabled: bool,
    ) -> Result<()> {
        for event in events {
            match event {
                CommercialEvent::DeviceSignIn {
                    verification_uri,
                    browser_uri,
                    user_code,
                } => {
                    if human_output {
                        ui.write_stderr(&super::commercial_lifecycle::test_device_sign_in(
                            ui.stderr_context(),
                            verification_uri,
                            user_code,
                        ))?;
                    }
                    if browser_enabled {
                        let opened = self.session.open_browser(browser_uri).is_ok();
                        ui.write_stderr(&super::commercial_lifecycle::test_browser_notice(
                            ui.stderr_context(),
                            opened,
                            "ctx Pro sign-in",
                        ))?;
                    }
                }
                CommercialEvent::TrialConversion => {
                    if human_output {
                        ui.write_stderr(&super::commercial_lifecycle::test_trial_conversion(
                            ui.stderr_context(),
                        ))?;
                    }
                }
                CommercialEvent::Checkout { url, waiting } => {
                    if human_output {
                        ui.write_stderr(&super::commercial_lifecycle::test_paid_checkout_prompt(
                            ui.stderr_context(),
                            url,
                        ))?;
                    }
                    if browser_enabled {
                        let opened = self.session.open_browser(url).is_ok();
                        ui.write_stderr(&super::commercial_lifecycle::test_browser_notice(
                            ui.stderr_context(),
                            opened,
                            "ctx Pro checkout",
                        ))?;
                    }
                    if human_output && *waiting {
                        ui.write_stderr(&super::commercial_lifecycle::test_checkout_progress(
                            ui.stderr_context(),
                            "Waiting for checkout",
                            None,
                        ))?;
                    }
                }
                CommercialEvent::CheckoutActive => {
                    if human_output {
                        ui.write_stderr(&super::commercial_lifecycle::test_checkout_progress(
                            ui.stderr_context(),
                            "Subscription access is active",
                            Some("Finishing ctx Pro setup."),
                        ))?;
                    }
                }
            }
        }
        Ok(())
    }

    fn authorize_referral(
        &self,
        auth_mode: ReferralAuthMode,
        events: &[CommercialEvent],
    ) -> Result<()> {
        match self.session.manifest.vault.state {
            VaultState::Locked => bail!("key_store_locked: scripted process vault is locked"),
            VaultState::Unavailable => {
                bail!("key_store_unavailable: scripted process vault is unavailable")
            }
            VaultState::CommercialCredentialsActive => Ok(()),
            VaultState::InstallationIdentityOnly => match auth_mode {
                ReferralAuthMode::CachedOnly => bail!(
                    "authentication_required: rerun the referral command without --format json to sign in"
                ),
                ReferralAuthMode::Interactive { .. }
                    if events
                        .iter()
                        .any(|event| matches!(event, CommercialEvent::DeviceSignIn { .. })) =>
                {
                    Ok(())
                }
                ReferralAuthMode::Interactive { .. } => {
                    bail!("authentication_required: run `ctx referral status` to sign in")
                }
            },
        }
    }
}

impl ProLifecycleService for TestControlService {
    fn release_trust(&self) -> Result<super::lifecycle::lifecycle_manifest::ReleaseTrust> {
        test_control_release_trust()
    }

    fn setup(
        &mut self,
        _data_root: &Path,
        _installed_version: Option<&str>,
        _trial_only: bool,
        _referral_codename: Option<&str>,
        ui: &mut Ui,
        human_output: bool,
        browser_enabled: bool,
    ) -> Result<ProSetupPlan> {
        self.session.begin_operation("lifecycle.setup")?;
        let script = self
            .session
            .manifest
            .lifecycle
            .setup
            .as_ref()
            .ok_or_else(|| anyhow!("invalid_request: lifecycle setup was not scripted"))?;
        self.emit_events(&script.events, ui, human_output, browser_enabled)?;
        let value = scripted_value(&script.outcome)?;
        let artifact = if value.helper_artifact {
            Some(SetupArtifactBundle::TestControl(
                self.session
                    .helper
                    .clone()
                    .ok_or_else(|| anyhow!("invalid_request: scripted helper is missing"))?,
            ))
        } else {
            None
        };
        Ok(ProSetupPlan {
            artifact,
            account_state: value.account_state,
        })
    }

    fn manage(
        &mut self,
        _data_root: &Path,
        ui: &mut Ui,
        human_output: bool,
        browser_enabled: bool,
    ) -> Result<ProManagePlan> {
        self.session.begin_operation("lifecycle.manage")?;
        let script = self
            .session
            .manifest
            .lifecycle
            .manage
            .as_ref()
            .ok_or_else(|| anyhow!("invalid_request: lifecycle manage was not scripted"))?;
        self.emit_events(&script.events, ui, human_output, browser_enabled)?;
        let value = scripted_value(&script.outcome)?;
        Ok(ProManagePlan {
            portal_url: value.portal_url,
            access_state: value.access_state,
            refresh_after_unix: value.refresh_after_unix,
            access_deadline_unix: value.access_deadline_unix,
            grace_deadline_unix: value.grace_deadline_unix,
        })
    }
}

impl ReferralService for TestControlService {
    fn create(
        &mut self,
        codename: &str,
        auth_mode: ReferralAuthMode,
        ui: &mut Ui,
    ) -> Result<ReferralCreateResult> {
        self.session.begin_operation("referral.create")?;
        let script = self
            .session
            .manifest
            .referral
            .create
            .as_ref()
            .ok_or_else(|| anyhow!("invalid_request: referral create was not scripted"))?;
        self.authorize_referral(auth_mode, &script.events)?;
        let (human_output, browser_enabled) = auth_output(auth_mode);
        self.emit_events(&script.events, ui, human_output, browser_enabled)?;
        let value = scripted_value(&script.outcome)?;
        let result = ReferralCreateResult {
            codename: value.codename,
            disposition: value.disposition,
        };
        result.validate(codename)?;
        Ok(result)
    }

    fn status(&mut self, auth_mode: ReferralAuthMode, ui: &mut Ui) -> Result<ReferralStatusResult> {
        self.session.begin_operation("referral.status")?;
        let script = self
            .session
            .manifest
            .referral
            .status
            .as_ref()
            .ok_or_else(|| anyhow!("invalid_request: referral status was not scripted"))?;
        self.authorize_referral(auth_mode, &script.events)?;
        let (human_output, browser_enabled) = auth_output(auth_mode);
        self.emit_events(&script.events, ui, human_output, browser_enabled)?;
        let value = scripted_value(&script.outcome)?;
        let result = ReferralStatusResult {
            codename: value.codename,
            attributed: value.attributed,
            subscribed: value.subscribed,
            earned_cents: value.earned_cents,
            pending_cents: value.pending_cents,
            manual_review_cents: value.manual_review_cents,
            payable_cents: value.payable_cents,
            processing_cents: value.processing_cents,
            paid_cents: value.paid_cents,
            debt_cents: value.debt_cents,
            currency: value.currency,
            payout_state: value.payout_state,
        };
        result.validate()?;
        Ok(result)
    }

    fn payout(
        &mut self,
        _country: Option<&str>,
        _entity_type: Option<&str>,
        auth_mode: ReferralAuthMode,
        ui: &mut Ui,
    ) -> Result<ReferralPayoutResult> {
        self.session.begin_operation("referral.payout")?;
        let script = self
            .session
            .manifest
            .referral
            .payout
            .as_ref()
            .ok_or_else(|| anyhow!("invalid_request: referral payout was not scripted"))?;
        self.authorize_referral(auth_mode, &script.events)?;
        let (human_output, browser_enabled) = auth_output(auth_mode);
        self.emit_events(&script.events, ui, human_output, browser_enabled)?;
        let value = scripted_value(&script.outcome)?;
        let result = ReferralPayoutResult {
            kind: value.kind,
            payout_state: value.payout_state,
            url: value.url,
            expires_at_unix: value.expires_at_unix,
        };
        validate_test_payout(&result, self.session.unix_time()?)?;
        Ok(result)
    }
}

fn auth_output(auth_mode: ReferralAuthMode) -> (bool, bool) {
    match auth_mode {
        ReferralAuthMode::Interactive { browser_enabled } => (true, browser_enabled),
        ReferralAuthMode::CachedOnly => (false, false),
    }
}

fn scripted_value<T: Clone>(outcome: &ScriptOutcome<T>) -> Result<T> {
    match outcome {
        ScriptOutcome::Success { value } => Ok(value.clone()),
        ScriptOutcome::Error { code, message } if message.is_empty() => Err(anyhow!(code.clone())),
        ScriptOutcome::Error { code, message } => Err(anyhow!("{code}: {message}")),
    }
}

fn validate_test_payout(result: &ReferralPayoutResult, now: i64) -> Result<()> {
    if result.kind != "payout_onboarding_created" || result.payout_state != "onboarding_pending" {
        bail!("invalid_response: referral payout result is invalid");
    }
    validate_fixture_url(&result.url, "payout URL")?;
    if result.expires_at_unix <= now
        || result.expires_at_unix > now.saturating_add(MAX_PAYOUT_LIFETIME_SECONDS)
    {
        bail!("invalid_response: referral payout URL expiry is outside allowed bounds");
    }
    Ok(())
}

struct ControlSession {
    manifest: ControlManifest,
    expected_operation: &'static str,
    helper: Option<TestControlHelperBundle>,
    writer: Mutex<ReceiptWriter>,
}

impl ControlSession {
    fn begin_operation(&self, operation: &'static str) -> Result<()> {
        if operation != self.expected_operation {
            bail!(
                "invalid_request: Pro test control expected {}, observed {operation}",
                self.expected_operation
            );
        }
        self.update_receipt(|receipt| {
            if !receipt.service_calls.is_empty() {
                bail!("invalid_request: Pro test control operation was called more than once");
            }
            receipt.service_calls.push(operation.to_owned());
            Ok(())
        })
    }

    fn unix_time(&self) -> Result<i64> {
        self.update_receipt(|receipt| {
            let index = receipt
                .clock_calls
                .len()
                .min(self.manifest.clock.unix_seconds.len().saturating_sub(1));
            let value = self.manifest.clock.unix_seconds[index];
            receipt.clock_calls.push(value);
            Ok(value)
        })
    }

    fn open_browser(&self, url: &str) -> Result<()> {
        validate_fixture_url(url, "browser URL")?;
        self.update_receipt(|receipt| {
            let index = receipt.browser.calls.len();
            let Some(expected) = self.manifest.browser.outcomes.get(index).copied() else {
                receipt.browser.calls.push(BrowserCallReceipt {
                    url: url.to_owned(),
                    result: "unexpected".to_owned(),
                });
                bail!("invalid_request: Pro test browser received an unexpected call");
            };
            let result = match expected {
                BrowserOutcome::Success => "success",
                BrowserOutcome::Failure => "failure",
            };
            receipt.browser.calls.push(BrowserCallReceipt {
                url: url.to_owned(),
                result: result.to_owned(),
            });
            match expected {
                BrowserOutcome::Success => Ok(()),
                BrowserOutcome::Failure => {
                    bail!("browser_unavailable: scripted browser failure")
                }
            }
        })
    }

    fn finish(&self, command_outcome: &str) -> Result<()> {
        self.update_receipt(|receipt| {
            if receipt.service_calls.as_slice() != [self.expected_operation] {
                bail!(
                    "invalid_request: Pro test control service call was not observed exactly once"
                );
            }
            if receipt.browser.calls.len() != self.manifest.browser.outcomes.len() {
                bail!("invalid_request: Pro test browser call count did not match its script");
            }
            receipt.command_outcome = Some(command_outcome.to_owned());
            receipt.completed = true;
            Ok(())
        })
    }

    fn update_receipt<T>(
        &self,
        update: impl FnOnce(&mut ControlReceipt) -> Result<T>,
    ) -> Result<T> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow!("invalid_request: Pro test control receipt lock is poisoned"))?;
        let value = update(&mut writer.receipt)?;
        writer.write()?;
        Ok(value)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TestControlHelperBundle {
    root: PathBuf,
    path: PathBuf,
    sha256: String,
}

impl TestControlHelperBundle {
    fn new(root: &Path, helper: &HelperControl) -> Result<Self> {
        validate_relative_path(&helper.path, "helper path")?;
        validate_sha256(&helper.sha256, "helper SHA-256")?;
        let bundle = Self {
            root: root.to_path_buf(),
            path: root.join(&helper.path),
            sha256: helper.sha256.clone(),
        };
        bundle.verified_path()?;
        Ok(bundle)
    }

    pub(super) fn verified_path(&self) -> Result<PathBuf> {
        self.verified_path_ref().map(Path::to_path_buf)
    }

    pub(super) fn verified_path_ref(&self) -> Result<&Path> {
        verify_bounded_file_in_root(&self.root, &self.path, MAX_HELPER_BYTES, &self.sha256, true)?;
        Ok(&self.path)
    }
}

struct ReceiptWriter {
    file: File,
    receipt: ControlReceipt,
}

impl ReceiptWriter {
    fn create(root: &Path, name: &str, receipt: ControlReceipt) -> Result<Self> {
        validate_receipt_name(name)?;
        let path = root.join(name);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .context("invalid_request: create Pro test control receipt")?;
        let mut writer = Self { file, receipt };
        writer.write()?;
        Ok(writer)
    }

    fn write(&mut self) -> Result<()> {
        self.file
            .rewind()
            .context("invalid_request: rewind Pro test control receipt")?;
        self.file
            .set_len(0)
            .context("invalid_request: truncate Pro test control receipt")?;
        let mut bytes = serde_json::to_vec(&self.receipt)
            .context("invalid_request: encode Pro test control receipt")?;
        bytes.push(b'\n');
        self.file
            .write_all(&bytes)
            .context("invalid_request: write Pro test control receipt")?;
        self.file
            .sync_data()
            .context("invalid_request: sync Pro test control receipt")
    }
}

#[derive(Debug, Serialize)]
struct ControlReceipt {
    schema_version: u16,
    fixture_id: String,
    observer_root: String,
    expected_operation: String,
    vault_backend: &'static str,
    native_vault_calls: u64,
    network_calls: u64,
    native_browser_calls: u64,
    service_calls: Vec<String>,
    clock_calls: Vec<i64>,
    browser: BrowserReceipt,
    command_outcome: Option<String>,
    completed: bool,
}

impl ControlReceipt {
    fn new(manifest: &ControlManifest) -> Self {
        Self {
            schema_version: 1,
            fixture_id: manifest.fixture_id.clone(),
            observer_root: ".".to_owned(),
            expected_operation: expected_operation(manifest).unwrap_or("invalid").to_owned(),
            vault_backend: "isolated_process_manifest",
            native_vault_calls: 0,
            network_calls: 0,
            native_browser_calls: 0,
            service_calls: Vec::new(),
            clock_calls: Vec::new(),
            browser: BrowserReceipt {
                expected: manifest.browser.outcomes.clone(),
                calls: Vec::new(),
            },
            command_outcome: None,
            completed: false,
        }
    }
}

#[derive(Debug, Serialize)]
struct BrowserReceipt {
    expected: Vec<BrowserOutcome>,
    calls: Vec<BrowserCallReceipt>,
}

#[derive(Debug, Serialize)]
struct BrowserCallReceipt {
    url: String,
    result: String,
}
