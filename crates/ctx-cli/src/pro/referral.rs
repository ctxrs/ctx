use std::{
    convert::Infallible,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context as _, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::{
    commercial_api::{ReferralCreateResult, ReferralPayoutResult, ReferralStatusResult},
    commercial_config::CommercialConfig,
    commercial_lifecycle::{open_browser, CommercialLifecycleService},
};
use crate::output::JsonOutputFormat;

const REFERRAL_TAGLINE: &str = "Refer a developer. Earn $10/month toward your agent bill.";
const REFERRAL_SECONDARY: &str = "Up to $120 per friend.";
const REFERRAL_CODENAME_MIN_BYTES: usize = 3;
const REFERRAL_CODENAME_MAX_BYTES: usize = 32;
const REFERRAL_CTA_MARKER: &str = ".referral-cta-v1.shown";
pub(crate) const REFERRAL_CTA: &str = "Refer a developer. Earn $10/month toward your agent bill.\n\
Up to $120 per friend.\n\
ctx referral create <codename>";

#[derive(Debug, Args)]
pub(crate) struct ReferralArgs {
    #[command(subcommand)]
    command: ReferralCommand,
}

#[derive(Debug, Subcommand)]
enum ReferralCommand {
    #[command(about = "Claim or show your stable referral codename")]
    Create(ReferralCreateArgs),
    #[command(about = "Show your aggregate referral ledger and payout state")]
    Status(ReferralStatusArgs),
    #[command(about = "Open Stripe-hosted payout onboarding when eligible")]
    Payout(ReferralPayoutArgs),
}

#[derive(Debug, Args)]
struct ReferralCreateArgs {
    #[arg(value_parser = parse_referral_codename_unchecked)]
    codename: ReferralCodename,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
}

#[derive(Debug, Args)]
struct ReferralStatusArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
}

#[derive(Debug, Args)]
struct ReferralPayoutArgs {
    #[arg(long, help = "Print the Stripe-hosted URL without opening a browser")]
    no_open: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
    #[arg(long, value_parser = parse_country_code)]
    country: Option<CountryCode>,
    #[arg(long, value_enum)]
    entity_type: Option<ReferralEntityType>,
}

impl ReferralArgs {
    pub(crate) fn validate_invocation(&self) -> Result<()> {
        if let ReferralCommand::Create(args) = &self.command {
            args.codename.validate()?;
        }
        Ok(())
    }

    pub(crate) const fn json_output(&self) -> bool {
        match &self.command {
            ReferralCommand::Create(args) => args.format.is_json(),
            ReferralCommand::Status(args) => args.format.is_json(),
            ReferralCommand::Payout(args) => args.format.is_json(),
        }
    }
}

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub(crate) struct ReferralCodename(String);

impl ReferralCodename {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if !valid_referral_codename(&self.0) {
            bail!(
                "invalid_request: referral codename must be \
                 {REFERRAL_CODENAME_MIN_BYTES}-{REFERRAL_CODENAME_MAX_BYTES} lowercase ASCII \
                 letters, digits, or single hyphens, and must start with a letter"
            );
        }
        Ok(())
    }
}

impl fmt::Debug for ReferralCodename {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReferralCodename([REDACTED])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CountryCode(String);

impl CountryCode {
    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ReferralEntityType {
    Individual,
    Company,
}

impl ReferralEntityType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Individual => "individual",
            Self::Company => "company",
        }
    }
}

#[cfg(test)]
pub(crate) fn parse_referral_codename(value: &str) -> Result<ReferralCodename, String> {
    let codename = ReferralCodename(value.to_owned());
    codename.validate().map_err(|error| error.to_string())?;
    Ok(codename)
}

pub(super) fn parse_referral_codename_unchecked(
    value: &str,
) -> std::result::Result<ReferralCodename, Infallible> {
    Ok(ReferralCodename(value.to_owned()))
}

pub(super) fn valid_referral_codename(value: &str) -> bool {
    (REFERRAL_CODENAME_MIN_BYTES..=REFERRAL_CODENAME_MAX_BYTES).contains(&value.len())
        && value.is_ascii()
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.as_bytes().windows(2).any(|pair| pair == b"--")
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

fn parse_country_code(value: &str) -> Result<CountryCode, String> {
    if value.len() != 2 || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err("country must be a two-letter uppercase ISO country code".to_owned());
    }
    Ok(CountryCode(value.to_owned()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReferralAuthMode {
    Interactive,
    CachedOnly,
}

trait ReferralService {
    fn create(
        &mut self,
        codename: &str,
        auth_mode: ReferralAuthMode,
    ) -> Result<ReferralCreateResult>;
    fn status(&mut self, auth_mode: ReferralAuthMode) -> Result<ReferralStatusResult>;
    fn payout(
        &mut self,
        country: Option<&str>,
        entity_type: Option<&str>,
        auth_mode: ReferralAuthMode,
    ) -> Result<ReferralPayoutResult>;
}

impl CommercialLifecycleService {
    fn referral_access_token(&self, auth_mode: ReferralAuthMode) -> Result<String> {
        match auth_mode {
            ReferralAuthMode::Interactive => self.access_token(),
            ReferralAuthMode::CachedOnly => {
                self.access_token_noninteractive().map_err(|error| {
                    if crate::pro::stable_error_code(&error) == Some("authentication_required") {
                        anyhow::anyhow!(
                            "authentication_required: rerun the referral command without --format json to sign in"
                        )
                    } else {
                        error
                    }
                })
            }
        }
    }
}

impl ReferralService for CommercialLifecycleService {
    fn create(
        &mut self,
        codename: &str,
        auth_mode: ReferralAuthMode,
    ) -> Result<ReferralCreateResult> {
        let mut access_token = self.referral_access_token(auth_mode)?;
        let result = self.api.referral_create(&access_token, codename);
        access_token.zeroize();
        result
    }

    fn status(&mut self, auth_mode: ReferralAuthMode) -> Result<ReferralStatusResult> {
        let mut access_token = self.referral_access_token(auth_mode)?;
        let result = self.api.referral_status(&access_token);
        access_token.zeroize();
        result
    }

    fn payout(
        &mut self,
        country: Option<&str>,
        entity_type: Option<&str>,
        auth_mode: ReferralAuthMode,
    ) -> Result<ReferralPayoutResult> {
        let mut access_token = self.referral_access_token(auth_mode)?;
        let result = self
            .api
            .referral_payout(&access_token, country, entity_type);
        access_token.zeroize();
        result
    }
}

pub(crate) fn run(args: ReferralArgs, data_root: PathBuf) -> Result<()> {
    args.validate_invocation()?;
    CommercialConfig::ensure_referrals_available()?;
    let json_output = args.json_output();
    prepare_referral_identity(&data_root, json_output)?;
    let mut service = CommercialLifecycleService::production(&data_root)?;
    run_with_service(args, &mut service, &mut io::stdout().lock(), &open_browser)
}

pub(crate) fn show_cta_once(data_root: &Path, eligible: bool, output: &mut impl io::Write) -> bool {
    show_cta_once_when_available(
        data_root,
        eligible,
        CommercialConfig::referrals_available(),
        output,
    )
}

fn show_cta_once_when_available(
    data_root: &Path,
    eligible: bool,
    available: bool,
    output: &mut impl io::Write,
) -> bool {
    if !available || !eligible || !claim_cta_marker(data_root) {
        return false;
    }
    let rendered = format!("\n{REFERRAL_CTA}\n");
    if output.write_all(rendered.as_bytes()).is_ok() {
        true
    } else {
        rollback_cta_marker(data_root);
        false
    }
}

fn claim_cta_marker(data_root: &Path) -> bool {
    use ctx_history_core::platform_security::{
        restrict_private_file, verify_private_directory, verify_private_file,
    };

    let marker = data_root.join(REFERRAL_CTA_MARKER);
    if verify_private_directory(data_root).is_err() {
        return false;
    }
    let temporary = data_root.join(format!(".referral-cta-v1.{}.tmp", Uuid::new_v4().simple()));
    let result = (|| -> std::io::Result<bool> {
        crate::identity::create_private_file(&temporary, b"shown\n")?;
        restrict_private_file(&temporary).map_err(std::io::Error::other)?;
        verify_private_file(&temporary).map_err(std::io::Error::other)?;
        fs::File::open(&temporary)?.sync_all()?;
        match fs::hard_link(&temporary, &marker) {
            Ok(()) => {
                #[cfg(unix)]
                fs::File::open(data_root)?.sync_all()?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    })();
    let _ = fs::remove_file(&temporary);
    result.unwrap_or(false)
}

fn rollback_cta_marker(data_root: &Path) {
    let marker = data_root.join(REFERRAL_CTA_MARKER);
    if fs::remove_file(marker).is_ok() {
        #[cfg(unix)]
        let _ = fs::File::open(data_root).and_then(|directory| directory.sync_all());
    }
}

fn prepare_referral_identity(data_root: &Path, json_output: bool) -> Result<()> {
    if json_output {
        if crate::identity::existing_installation_id(data_root)
            .context("authentication_required: load cached ctx identity")?
            .is_none()
        {
            bail!("authentication_required: rerun the referral command without --format json to sign in");
        }
    } else {
        crate::identity::installation_id(data_root)
            .context("key_store_unavailable: initialize local ctx identity")?;
    }
    Ok(())
}

fn run_with_service(
    args: ReferralArgs,
    service: &mut dyn ReferralService,
    output: &mut impl io::Write,
    opener: &impl Fn(&str) -> Result<()>,
) -> Result<()> {
    args.validate_invocation()?;
    let auth_mode = if args.json_output() {
        ReferralAuthMode::CachedOnly
    } else {
        ReferralAuthMode::Interactive
    };
    match args.command {
        ReferralCommand::Create(args) => {
            let result = service.create(args.codename.as_str(), auth_mode)?;
            if args.format.is_json() {
                write_json(output, &create_output(&result))
            } else {
                write!(output, "{}", render_create_human(&result))?;
                Ok(())
            }
        }
        ReferralCommand::Status(args) => {
            let result = service.status(auth_mode)?;
            if args.format.is_json() {
                write_json(output, &status_output(&result))
            } else {
                write!(output, "{}", render_status_human(&result))?;
                Ok(())
            }
        }
        ReferralCommand::Payout(args) => {
            let result = service.payout(
                args.country.as_ref().map(CountryCode::as_str),
                args.entity_type.map(ReferralEntityType::as_str),
                auth_mode,
            )?;
            if args.format.is_json() {
                return write_json(output, &payout_output(&result, false));
            }
            let mut browser_opened = false;
            if !args.no_open {
                browser_opened = opener(&result.url).is_ok();
            }
            write!(
                output,
                "{}",
                render_payout_human(&result, args.no_open, browser_opened)
            )?;
            Ok(())
        }
    }
}

fn write_json(output: &mut impl io::Write, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    writeln!(output)?;
    Ok(())
}

fn share_command(codename: &str) -> String {
    format!("ctx pro --referral {codename}")
}

#[derive(Serialize)]
struct ReferralCreateOutput<'a> {
    schema_version: u16,
    payload_type: &'static str,
    codename: &'a str,
    share_command: String,
    disposition: &'a str,
}

fn create_output(result: &ReferralCreateResult) -> ReferralCreateOutput<'_> {
    ReferralCreateOutput {
        schema_version: 1,
        payload_type: "referral_create",
        codename: &result.codename,
        share_command: share_command(&result.codename),
        disposition: &result.disposition,
    }
}

#[derive(Serialize)]
struct ReferralStatusOutput<'a> {
    schema_version: u16,
    payload_type: &'static str,
    codename: &'a str,
    share_command: String,
    attributed: u64,
    subscribed: u64,
    earned_cents: u64,
    pending_cents: u64,
    manual_review_cents: u64,
    payable_cents: u64,
    processing_cents: u64,
    paid_cents: u64,
    debt_cents: u64,
    currency: &'a str,
    payout_state: &'a str,
}

fn status_output(result: &ReferralStatusResult) -> ReferralStatusOutput<'_> {
    ReferralStatusOutput {
        schema_version: 1,
        payload_type: "referral_status",
        codename: &result.codename,
        share_command: share_command(&result.codename),
        attributed: result.attributed,
        subscribed: result.subscribed,
        earned_cents: result.earned_cents,
        pending_cents: result.pending_cents,
        manual_review_cents: result.manual_review_cents,
        payable_cents: result.payable_cents,
        processing_cents: result.processing_cents,
        paid_cents: result.paid_cents,
        debt_cents: result.debt_cents,
        currency: &result.currency,
        payout_state: &result.payout_state,
    }
}

#[derive(Serialize)]
struct ReferralPayoutOutput<'a> {
    schema_version: u16,
    payload_type: &'static str,
    payout_state: &'a str,
    onboarding_url: &'a str,
    expires_at_unix: i64,
    browser_opened: bool,
}

fn payout_output(result: &ReferralPayoutResult, browser_opened: bool) -> ReferralPayoutOutput<'_> {
    ReferralPayoutOutput {
        schema_version: 1,
        payload_type: "referral_payout",
        payout_state: &result.payout_state,
        onboarding_url: &result.url,
        expires_at_unix: result.expires_at_unix,
        browser_opened,
    }
}

fn render_create_human(result: &ReferralCreateResult) -> String {
    format!(
        "{REFERRAL_TAGLINE}\n{REFERRAL_SECONDARY}\nCodename: {}\nShare: {}\n",
        result.codename,
        share_command(&result.codename)
    )
}

fn render_status_human(result: &ReferralStatusResult) -> String {
    format!(
        "{REFERRAL_TAGLINE}\n{REFERRAL_SECONDARY}\nCodename: {}\nShare: {}\nAttributed: {}\nSubscribed: {}\nEarned: {}\nPending: {}\nManual review: {}\nPayable: {}\nProcessing: {}\nPaid: {}\nDebt: {}\nPayout: {}\n",
        result.codename,
        share_command(&result.codename),
        result.attributed,
        result.subscribed,
        format_usd(result.earned_cents),
        format_usd(result.pending_cents),
        format_usd(result.manual_review_cents),
        format_usd(result.payable_cents),
        format_usd(result.processing_cents),
        format_usd(result.paid_cents),
        format_usd(result.debt_cents),
        result.payout_state,
    )
}

fn render_payout_human(
    result: &ReferralPayoutResult,
    no_open: bool,
    browser_opened: bool,
) -> String {
    if no_open {
        return format!(
            "{REFERRAL_TAGLINE}\n{REFERRAL_SECONDARY}\nComplete Stripe payout setup:\n{}\n",
            result.url
        );
    }
    if browser_opened {
        format!(
            "{REFERRAL_TAGLINE}\n{REFERRAL_SECONDARY}\nStripe payout setup opened in your browser.\n"
        )
    } else {
        format!(
            "{REFERRAL_TAGLINE}\n{REFERRAL_SECONDARY}\nA browser could not be opened. Complete Stripe payout setup at:\n{}\n",
            result.url
        )
    }
}

fn format_usd(cents: u64) -> String {
    format!("${}.{:02}", cents / 100, cents % 100)
}

#[cfg(test)]
mod cta_tests;

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use clap::{CommandFactory as _, Parser as _};
    use serde_json::Value;

    use super::*;
    use crate::cli::{Cli, CommandRoot};

    #[derive(Default)]
    struct FakeReferralService {
        auth_modes: RefCell<Vec<ReferralAuthMode>>,
        create: Option<ReferralCreateResult>,
        status: Option<ReferralStatusResult>,
        payout: Option<ReferralPayoutResult>,
    }

    impl ReferralService for FakeReferralService {
        fn create(
            &mut self,
            _codename: &str,
            auth_mode: ReferralAuthMode,
        ) -> Result<ReferralCreateResult> {
            self.auth_modes.borrow_mut().push(auth_mode);
            self.create
                .take()
                .ok_or_else(|| anyhow::anyhow!("missing create fixture"))
        }

        fn status(&mut self, auth_mode: ReferralAuthMode) -> Result<ReferralStatusResult> {
            self.auth_modes.borrow_mut().push(auth_mode);
            self.status
                .take()
                .ok_or_else(|| anyhow::anyhow!("missing status fixture"))
        }

        fn payout(
            &mut self,
            _country: Option<&str>,
            _entity_type: Option<&str>,
            auth_mode: ReferralAuthMode,
        ) -> Result<ReferralPayoutResult> {
            self.auth_modes.borrow_mut().push(auth_mode);
            self.payout
                .take()
                .ok_or_else(|| anyhow::anyhow!("missing payout fixture"))
        }
    }

    fn parse(args: &[&str]) -> ReferralArgs {
        let cli = Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied())).unwrap();
        let CommandRoot::Referral(args) = cli.command else {
            panic!("expected referral command");
        };
        args
    }

    fn create_result() -> ReferralCreateResult {
        ReferralCreateResult {
            codename: "agent-smith".to_owned(),
            disposition: "created".to_owned(),
        }
    }

    fn status_result() -> ReferralStatusResult {
        ReferralStatusResult {
            codename: "agent-smith".to_owned(),
            attributed: 4,
            subscribed: 3,
            earned_cents: 7_000,
            pending_cents: 2_000,
            manual_review_cents: 2_000,
            payable_cents: 2_000,
            processing_cents: 1_000,
            paid_cents: 2_000,
            debt_cents: 2_000,
            currency: "usd".to_owned(),
            payout_state: "paused".to_owned(),
        }
    }

    fn payout_result() -> ReferralPayoutResult {
        ReferralPayoutResult {
            kind: "payout_onboarding_created".to_owned(),
            payout_state: "onboarding_pending".to_owned(),
            url: "https://connect.stripe.com/setup/s/test".to_owned(),
            expires_at_unix: super::super::commercial_api::unix_time().unwrap() + 300,
        }
    }

    #[test]
    fn parser_exposes_one_top_level_namespace_and_the_bare_pro_referral_form() {
        for args in [
            &["referral", "create", "agent-smith"][..],
            &["referral", "status"][..],
            &["referral", "payout", "--no-open"][..],
            &["pro", "--referral", "agent-smith"][..],
        ] {
            Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied())).unwrap();
        }
        for args in [
            &["status", "--referral", "agent-smith"][..],
            &["search", "agents", "--referral", "agent-smith"][..],
            &["pro", "setup", "--referral", "agent-smith"][..],
            &["pro", "manage", "--referral", "agent-smith"][..],
            &["pro", "uninstall", "--referral", "agent-smith"][..],
        ] {
            assert!(
                Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied())).is_err(),
                "{args:?}"
            );
        }
    }

    #[test]
    fn referral_help_states_the_recurring_commission_and_direct_cap() {
        let mut command = Cli::command();
        let referral = command
            .find_subcommand_mut("referral")
            .expect("referral subcommand");
        let help = referral.render_long_help().to_string();
        assert!(help.contains("Refer a developer. Earn $10/month toward your agent bill."));
        assert!(help.contains("Up to $120 per friend."));
        assert!(help.contains("first 12 distinct qualifying paid monthly invoices"));
        assert!(help.contains("first two commissions remain pending until invoice 2 settles"));
        assert!(help.contains("invoices 3-12 each have their own 14-day hold and reconciliation"));
        assert!(!help.contains("Get $20"));
        assert!(!help.contains("one-time"));
    }

    #[test]
    fn codename_and_payout_identity_inputs_are_strictly_bounded_ascii() {
        for valid in ["abc", "agent-smith", "a1b2", &"a".repeat(32)] {
            assert!(parse_referral_codename(valid).is_ok(), "{valid:?}");
        }
        for invalid in [
            "",
            "ab",
            "-agent",
            "9agent",
            "agent-",
            "agent--smith",
            "Agent",
            "agent_name",
            "café",
            "agent smith",
            &"a".repeat(33),
        ] {
            assert!(parse_referral_codename(invalid).is_err(), "{invalid:?}");
        }
        assert!(parse_country_code("US").is_ok());
        for invalid in ["us", "USA", "U1", ""] {
            assert!(parse_country_code(invalid).is_err());
        }
    }

    #[test]
    fn create_and_status_human_and_json_render_exact_share_command() {
        let create = create_result();
        assert_eq!(
            render_create_human(&create),
            "Refer a developer. Earn $10/month toward your agent bill.\nUp to $120 per friend.\nCodename: agent-smith\nShare: ctx pro --referral agent-smith\n"
        );
        assert_eq!(
            serde_json::to_value(create_output(&create)).unwrap(),
            serde_json::json!({
                "schema_version": 1,
                "payload_type": "referral_create",
                "codename": "agent-smith",
                "share_command": "ctx pro --referral agent-smith",
                "disposition": "created",
            })
        );

        let status = status_result();
        assert_eq!(
            render_status_human(&status),
            "Refer a developer. Earn $10/month toward your agent bill.\nUp to $120 per friend.\nCodename: agent-smith\nShare: ctx pro --referral agent-smith\nAttributed: 4\nSubscribed: 3\nEarned: $70.00\nPending: $20.00\nManual review: $20.00\nPayable: $20.00\nProcessing: $10.00\nPaid: $20.00\nDebt: $20.00\nPayout: paused\n"
        );
        assert_eq!(
            serde_json::to_value(status_output(&status)).unwrap(),
            serde_json::json!({
                "schema_version": 1,
                "payload_type": "referral_status",
                "codename": "agent-smith",
                "share_command": "ctx pro --referral agent-smith",
                "attributed": 4,
                "subscribed": 3,
                "earned_cents": 7000,
                "pending_cents": 2000,
                "manual_review_cents": 2000,
                "payable_cents": 2000,
                "processing_cents": 1000,
                "paid_cents": 2000,
                "debt_cents": 2000,
                "currency": "usd",
                "payout_state": "paused",
            })
        );
    }

    #[test]
    fn payout_no_open_and_json_are_browser_free_and_json_uses_cached_auth() {
        for args in [
            parse(&["referral", "payout", "--no-open"]),
            parse(&["referral", "payout", "--format=json"]),
        ] {
            let json = args.json_output();
            let fixture = payout_result();
            let expected_expiry = fixture.expires_at_unix;
            let mut service = FakeReferralService {
                payout: Some(fixture),
                ..FakeReferralService::default()
            };
            let calls = Cell::new(0);
            let opener = |_: &str| {
                calls.set(calls.get() + 1);
                Ok(())
            };
            let mut output = Vec::new();
            run_with_service(args, &mut service, &mut output, &opener).unwrap();
            assert_eq!(calls.get(), 0);
            assert_eq!(
                service.auth_modes.into_inner(),
                [if json {
                    ReferralAuthMode::CachedOnly
                } else {
                    ReferralAuthMode::Interactive
                }]
            );
            let rendered = String::from_utf8(output).unwrap();
            if json {
                let value: Value = serde_json::from_str(rendered.trim()).unwrap();
                assert_eq!(
                    value,
                    serde_json::json!({
                        "schema_version": 1,
                        "payload_type": "referral_payout",
                        "payout_state": "onboarding_pending",
                        "onboarding_url": "https://connect.stripe.com/setup/s/test",
                        "expires_at_unix": expected_expiry,
                        "browser_opened": false,
                    })
                );
            } else {
                assert_eq!(
                    rendered,
                    "Refer a developer. Earn $10/month toward your agent bill.\nUp to $120 per friend.\nComplete Stripe payout setup:\nhttps://connect.stripe.com/setup/s/test\n"
                );
            }
        }

        let mut service = FakeReferralService {
            payout: Some(payout_result()),
            ..FakeReferralService::default()
        };
        let calls = Cell::new(0);
        let opener = |url: &str| {
            assert_eq!(url, "https://connect.stripe.com/setup/s/test");
            calls.set(calls.get() + 1);
            Ok(())
        };
        let mut output = Vec::new();
        run_with_service(
            parse(&["referral", "payout"]),
            &mut service,
            &mut output,
            &opener,
        )
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "Refer a developer. Earn $10/month toward your agent bill.\nUp to $120 per friend.\nStripe payout setup opened in your browser.\n"
        );
    }

    #[test]
    fn every_json_command_uses_cached_auth_and_never_calls_the_payout_opener() {
        let cases = [
            (
                parse(&["referral", "create", "agent-smith", "--format=json"]),
                FakeReferralService {
                    create: Some(create_result()),
                    ..FakeReferralService::default()
                },
            ),
            (
                parse(&["referral", "status", "--format=json"]),
                FakeReferralService {
                    status: Some(status_result()),
                    ..FakeReferralService::default()
                },
            ),
            (
                parse(&["referral", "payout", "--format=json"]),
                FakeReferralService {
                    payout: Some(payout_result()),
                    ..FakeReferralService::default()
                },
            ),
        ];
        for (args, mut service) in cases {
            let calls = Cell::new(0);
            let opener = |_: &str| {
                calls.set(calls.get() + 1);
                Ok(())
            };
            run_with_service(args, &mut service, &mut Vec::new(), &opener).unwrap();
            assert_eq!(calls.get(), 0);
            assert_eq!(
                service.auth_modes.into_inner(),
                [ReferralAuthMode::CachedOnly]
            );
        }
    }

    #[test]
    fn referral_commands_emit_neither_analytics_nor_local_usage() {
        for args in [
            &["referral", "create", "agent-smith"][..],
            &["referral", "status"][..],
            &["referral", "payout", "--format=json"][..],
        ] {
            let cli =
                Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied())).unwrap();
            assert!(
                crate::analytics::ClientOperationDraft::from_command(&cli.command, false).is_none()
            );
            assert!(crate::local_usage::CliUsage::from_command(&cli.command)
                .completed(true, std::time::Duration::ZERO)
                .is_none());
        }
    }

    #[test]
    fn redacted_codename_debug_never_exposes_the_value() {
        let value = parse_referral_codename("secret-agent").unwrap();
        let debug = format!("{value:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret-agent"));
    }
}
