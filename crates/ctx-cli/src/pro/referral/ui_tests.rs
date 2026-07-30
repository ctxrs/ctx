use std::{
    cell::{Cell, RefCell},
    io,
    sync::{Arc, Mutex},
};

use clap::Parser as _;
use serde_json::Value;

use super::*;
use crate::{
    cli::{Cli, CommandRoot},
    ui::{ColorMode, TestContext},
};

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn test_ui(width: usize) -> (Ui, SharedWriter, SharedWriter) {
    let stdout = SharedWriter::default();
    let stderr = SharedWriter::default();
    let stdout_copy = stdout.clone();
    let stderr_copy = stderr.clone();
    let stdout_context = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never),
    );
    let stderr_context = RenderContext::for_test(
        TestContext::tty(StreamKind::Stderr, width).color(ColorMode::Never),
    );
    (
        Ui::with_writers(stdout, stdout_context, stderr, stderr_context),
        stdout_copy,
        stderr_copy,
    )
}

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
        _ui: &mut Ui,
    ) -> Result<ReferralCreateResult> {
        self.auth_modes.borrow_mut().push(auth_mode);
        self.create
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing create fixture"))
    }

    fn status(
        &mut self,
        auth_mode: ReferralAuthMode,
        _ui: &mut Ui,
    ) -> Result<ReferralStatusResult> {
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
        _ui: &mut Ui,
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
fn create_and_status_human_results_use_ui_stdout() {
    let cases = [
        (
            parse(&["referral", "create", "agent-smith"]),
            FakeReferralService {
                create: Some(create_result()),
                ..FakeReferralService::default()
            },
            "Referral codename created",
        ),
        (
            parse(&["referral", "status"]),
            FakeReferralService {
                status: Some(status_result()),
                ..FakeReferralService::default()
            },
            "Referral debt: $20.00",
        ),
    ];
    for (args, mut service, expected) in cases {
        let calls = Cell::new(0);
        let opener = |_: &str| {
            calls.set(calls.get() + 1);
            Ok(())
        };
        let mut machine_output = Vec::new();
        let (mut ui, stdout, stderr) = test_ui(80);
        run_with_service(args, &mut service, &mut machine_output, &mut ui, &opener).unwrap();
        assert!(machine_output.is_empty());
        assert!(stdout.text().contains(expected));
        assert!(stderr.text().is_empty());
        assert_eq!(calls.get(), 0);
        assert_eq!(
            service.auth_modes.into_inner(),
            [ReferralAuthMode::Interactive {
                browser_enabled: true
            }]
        );
    }
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
        let (mut ui, stdout, stderr) = test_ui(80);
        run_with_service(args, &mut service, &mut output, &mut ui, &opener).unwrap();
        assert_eq!(calls.get(), 0);
        assert_eq!(
            service.auth_modes.into_inner(),
            [if json {
                ReferralAuthMode::CachedOnly
            } else {
                ReferralAuthMode::Interactive {
                    browser_enabled: false,
                }
            }]
        );
        let rendered = String::from_utf8(output).unwrap();
        if json {
            assert!(!rendered.contains('\u{1b}'));
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
            assert!(stdout.text().is_empty());
            assert!(stderr.text().is_empty());
        } else {
            assert!(rendered.is_empty());
            assert!(stdout
                .text()
                .starts_with("Stripe payout setup link created\n"));
            assert!(stdout
                .text()
                .contains("Ordinary customer referrals earn $10/month for 12 paid months"));
            assert!(stdout
                .text()
                .contains("Setup link  https://connect.stripe.com/setup/s/test"));
            assert!(stderr.text().is_empty());
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
    let (mut ui, stdout, stderr) = test_ui(80);
    run_with_service(
        parse(&["referral", "payout"]),
        &mut service,
        &mut output,
        &mut ui,
        &opener,
    )
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert!(output.is_empty());
    assert!(stdout
        .text()
        .contains("Setup link  https://connect.stripe.com/setup/s/test"));
    assert_eq!(
        stderr.text(),
        "Browser open requested for Stripe payout setup.\n"
    );
}

#[test]
fn payout_setup_url_moves_below_its_label_at_narrow_widths() {
    let result = payout_result();
    for width in [32, 48] {
        let rendered = render::payout(
            &RenderContext::for_test(
                TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never),
            ),
            &result,
            false,
        )
        .render_plain();
        assert!(
            rendered.contains("Setup link\n  https://connect.stripe.com/setup/s/test\n"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("Setup link  https://connect.stripe.com"),
            "{rendered}"
        );
    }

    let rendered = render::payout(
        &RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Never)),
        &result,
        false,
    )
    .render_plain();
    assert!(
        rendered.contains("Setup link  https://connect.stripe.com/setup/s/test\n"),
        "{rendered}"
    );
}

#[test]
fn every_json_command_uses_cached_auth_and_bypasses_ui() {
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
        let mut output = Vec::new();
        let (mut ui, stdout, stderr) = test_ui(80);
        run_with_service(args, &mut service, &mut output, &mut ui, &opener).unwrap();
        assert_eq!(calls.get(), 0);
        assert_eq!(
            service.auth_modes.into_inner(),
            [ReferralAuthMode::CachedOnly]
        );
        assert!(!output.contains(&0x1b));
        assert!(stdout.text().is_empty());
        assert!(stderr.text().is_empty());
    }
}
