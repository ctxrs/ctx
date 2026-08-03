use std::path::Path;

use serde_json::{json, Value};
use unicode_width::UnicodeWidthStr as _;

use super::{
    render_daemon_disable_receipt, render_daemon_enable_receipt,
    render_daemon_prepare_uninstall_receipt, render_daemon_status_human, DaemonStatusView,
};
use crate::ui::{ColorMode, Document, RenderContext, StreamKind, TestContext};

fn context(width: usize) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
}

fn styled_context(width: usize) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Always))
}

fn running_report() -> Value {
    json!({
        "enabled": true,
        "status": "running",
        "mode": "full",
        "running": true,
        "semantic_runtime_active": true,
        "config_reload": {"status": "applied"},
        "supervisor": {
            "status": "installed",
            "registration_verified": true,
            "live_owner_verified": true
        },
        "jobs": {
            "core_refresh": {
                "status": "completed",
                "certified_source_count": 1248,
                "published_generation": "internal-generation-to-omit"
            },
            "semantic_index": {
                "status": "completed",
                "embedding_runtime": {
                    "backend": "cuda",
                    "compute_mode": "accelerated"
                }
            }
        },
        "live_pid": 4242,
        "lock_identity": {"owner_id": "internal-lock-owner"},
        "core_refresh_endpoint": {"identity_path": "/tmp/internal-endpoint"},
        "trigger_provenance": "daemon_scheduler"
    })
}

fn render_status(context: &RenderContext, daemon: &Value) -> Document {
    render_daemon_status_human(context, DaemonStatusView::daemon_only(daemon))
}

fn assert_fits(document: &Document, context: &RenderContext) {
    let width = context.content_width().unwrap_or(1);
    for line in document.render_plain().lines() {
        assert!(
            line.trim_start().starts_with("ctx ") || line.width() <= width,
            "{line:?} exceeded {width} columns in:\n{}",
            document.render_plain()
        );
    }
}

fn strip_ansi(rendered: &str) -> String {
    let bytes = rendered.as_bytes();
    let mut plain = String::with_capacity(rendered.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
            index += 2;
            while index < bytes.len() {
                let byte = bytes[index];
                index += 1;
                if (0x40..=0x7e).contains(&byte) {
                    break;
                }
            }
        } else {
            let character = rendered[index..].chars().next().expect("valid UTF-8");
            plain.push(character);
            index += character.len_utf8();
        }
    }
    plain
}

fn assert_exact_cross_width(
    render: impl Fn(&RenderContext) -> Document,
    expected_plain: [(usize, &str); 4],
    expected_ansi_at_80: &str,
) {
    for (width, expected) in expected_plain {
        let plain_context = context(width);
        let document = render(&plain_context);
        let plain = document.render_plain();
        let ansi = document.render(&styled_context(width));

        assert_eq!(plain, expected, "plain output at {width} columns");
        assert_eq!(
            strip_ansi(&ansi),
            expected,
            "ANSI structure at {width} columns"
        );
        if width == 80 {
            assert_eq!(ansi, expected_ansi_at_80, "ANSI output at 80 columns");
        }
        assert_fits(&document, &plain_context);
    }
}

#[test]
fn running_status_is_outcome_first_and_omits_internal_details() {
    let wide = concat!(
        "✓ Daemon is healthy\n",
        "\n",
        "Service\n",
        "Status  running\n",
        "\n",
        "History refresh\n",
        "Status   ready\n",
        "Sources  1,248 certified sources\n",
        "\n",
        "Semantic\n",
        "Status  active\n",
    );
    assert_exact_cross_width(
        |context| render_status(context, &running_report()),
        [
            (
                32,
                concat!(
                    "✓ Daemon is healthy\n",
                    "\n",
                    "Service\n",
                    "Status  running\n",
                    "\n",
                    "History refresh\n",
                    "Status   ready\n",
                    "Sources  1,248 certified\n",
                    "         sources\n",
                    "\n",
                    "Semantic\n",
                    "Status  active\n",
                ),
            ),
            (48, wide),
            (80, wide),
            (120, wide),
        ],
        concat!(
            "\u{1b}[32m✓\u{1b}[0m \u{1b}[1mDaemon is healthy\u{1b}[0m\n",
            "\n",
            "\u{1b}[1mService\u{1b}[0m\n",
            "\u{1b}[2mStatus\u{1b}[0m  \u{1b}[32mrunning\u{1b}[0m\n",
            "\n",
            "\u{1b}[1mHistory refresh\u{1b}[0m\n",
            "\u{1b}[2mStatus\u{1b}[0m   \u{1b}[32mready\u{1b}[0m\n",
            "\u{1b}[2mSources\u{1b}[0m  1,248 certified sources\n",
            "\n",
            "\u{1b}[1mSemantic\u{1b}[0m\n",
            "\u{1b}[2mStatus\u{1b}[0m  \u{1b}[32mactive\u{1b}[0m\n",
        ),
    );

    let rendered = render_status(&context(80), &running_report()).render_plain();
    for omitted in [
        "4242",
        "internal-generation",
        "internal-lock-owner",
        "internal-endpoint",
        "daemon_scheduler",
        "cuda",
        "accelerated",
    ] {
        assert!(
            !rendered.contains(omitted),
            "{omitted:?} leaked:\n{rendered}"
        );
    }
}

#[test]
fn changed_installed_supervisor_environment_is_a_restart_caveat() {
    let mut report = running_report();
    report["supervisor"]["environment_snapshot"] = json!({
        "captured_at_ms": 1_725_000_000_000_i64,
        "sha256": "installed-snapshot",
        "current_sha256": "current-snapshot",
        "restart_required": true,
        "values_exposed": false
    });
    let rendered = render_status(&context(120), &report).render_plain();
    let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");

    assert!(rendered.starts_with("! Daemon is partially healthy\n"));
    assert!(rendered.contains("Persistence  not verified\n"));
    assert!(normalized.contains(
        "Caveat native supervisor environment changed; run `ctx daemon enable` to install the current nonsecret snapshot and restart"
    ));
    assert!(!rendered.contains("installed-snapshot"));
    assert!(!rendered.contains("current-snapshot"));
}

#[test]
fn service_status_configuration_and_details_share_one_value_column() {
    let mut report = running_report();
    report["supervisor"] = json!({
        "status": "fallback",
        "limitation": "restart required"
    });
    report["config_reload"] = json!({
        "status": "failed",
        "last_error": "reload rejected"
    });
    let aligned = concat!(
        "Service\n",
        "Status         running\n",
        "Persistence    not verified\n",
        "Configuration  failed\n",
        "Caveat         restart required\n",
        "Error          reload rejected\n",
    );

    for width in [32, 48, 80, 120] {
        let plain_context = context(width);
        let document = render_status(&plain_context, &report);
        let plain = document.render_plain();
        assert!(plain.contains(aligned), "{plain}");
        assert_eq!(strip_ansi(&document.render(&styled_context(width))), plain);
        assert_fits(&document, &plain_context);
    }

    let styled = render_status(&context(80), &report).render(&styled_context(80));
    assert!(styled.contains(concat!(
        "\u{1b}[1mService\u{1b}[0m\n",
        "\u{1b}[2mStatus\u{1b}[0m         \u{1b}[32mrunning\u{1b}[0m\n",
        "\u{1b}[2mPersistence\u{1b}[0m    \u{1b}[33mnot verified\u{1b}[0m\n",
        "\u{1b}[2mConfiguration\u{1b}[0m  \u{1b}[33mfailed\u{1b}[0m\n",
        "\u{1b}[2mCaveat\u{1b}[0m         restart required\n",
        "\u{1b}[2mError\u{1b}[0m          reload rejected\n",
    )));
}

#[test]
fn status_preserves_installed_pro_state_and_omits_only_absent_pro() {
    let daemon = running_report();
    let installed = json!({"installed": true, "state": "ready"});
    let rendered = render_daemon_status_human(
        &context(80),
        DaemonStatusView::from_reports(&daemon, &installed),
    )
    .render_plain();
    assert!(rendered.contains("\nPro\nStatus  ready\n"));

    let installed_without_state = json!({"installed": true});
    let rendered = render_daemon_status_human(
        &context(80),
        DaemonStatusView::from_reports(&daemon, &installed_without_state),
    )
    .render_plain();
    assert!(rendered.contains("\nPro\nStatus  unavailable\n"));

    let absent = json!({"installed": false, "state": "ready"});
    let rendered = render_daemon_status_human(
        &context(80),
        DaemonStatusView::from_reports(&daemon, &absent),
    )
    .render_plain();
    assert!(!rendered.contains("\nPro\n"));
}

#[test]
fn disabled_status_is_clear_and_enable_is_the_only_action() {
    let report = json!({
        "enabled": false,
        "status": "disabled",
        "mode": "full",
        "running": false,
        "semantic_runtime_active": false,
        "config_reload": {"status": "applied"},
        "jobs": {
            "core_refresh": {"status": "disabled"},
            "semantic_index": {"status": "disabled", "reason": "semantic_disabled"}
        }
    });
    let rendered = render_status(&context(80), &report).render_plain();

    assert!(rendered.starts_with(
        "Daemon is disabled\nAutomatic history refresh and semantic serving are off.\n"
    ));
    assert!(rendered.contains("Service\nStatus  disabled\n"));
    assert!(rendered.contains("History refresh\nStatus  disabled\n"));
    assert!(rendered.contains("Semantic\nStatus  disabled\n"));
    assert_eq!(rendered.matches("ctx daemon enable").count(), 1);
}

#[test]
fn completed_finite_run_wins_over_disabled_persistent_preference() {
    let report = json!({
        "enabled": false,
        "status": "completed",
        "mode": "full",
        "running": false,
        "semantic_runtime_active": false,
        "config_reload": {"status": "applied"},
        "jobs": {
            "core_refresh": {"status": "completed"},
            "semantic_index": {"status": "disabled", "reason": "semantic_disabled"}
        }
    });
    let wide = concat!(
        "✓ Daemon run completed\n",
        "\n",
        "Service\n",
        "Status             completed\n",
        "Automatic refresh  disabled\n",
        "\n",
        "History refresh\n",
        "Status  ready\n",
        "\n",
        "Semantic\n",
        "Status  disabled\n",
    );
    assert_exact_cross_width(
        |context| render_status(context, &report),
        [
            (
                32,
                concat!(
                    "✓ Daemon run completed\n",
                    "\n",
                    "Service\n",
                    "Status\n",
                    "  completed\n",
                    "Automatic refresh\n",
                    "  disabled\n",
                    "\n",
                    "History refresh\n",
                    "Status  ready\n",
                    "\n",
                    "Semantic\n",
                    "Status  disabled\n",
                ),
            ),
            (48, wide),
            (80, wide),
            (120, wide),
        ],
        concat!(
            "\u{1b}[32m✓\u{1b}[0m \u{1b}[1mDaemon run completed\u{1b}[0m\n",
            "\n",
            "\u{1b}[1mService\u{1b}[0m\n",
            "\u{1b}[2mStatus\u{1b}[0m             \u{1b}[32mcompleted\u{1b}[0m\n",
            "\u{1b}[2mAutomatic refresh\u{1b}[0m  disabled\n",
            "\n",
            "\u{1b}[1mHistory refresh\u{1b}[0m\n",
            "\u{1b}[2mStatus\u{1b}[0m  \u{1b}[32mready\u{1b}[0m\n",
            "\n",
            "\u{1b}[1mSemantic\u{1b}[0m\n",
            "\u{1b}[2mStatus\u{1b}[0m  disabled\n",
        ),
    );
}

#[test]
fn catching_up_status_keeps_record_and_byte_progress_visible() {
    let mut report = running_report();
    report["jobs"]["core_refresh"] = json!({
        "status": "running",
        "progress": {
            "phase": "scanning_provider_sources",
            "current_source": "~/.local/share/opencode/opencode.db",
            "completed_records": 1234,
            "completed_bytes": 4 * 1024 * 1024
        },
        "source_count": 9
    });
    let rendered = render_status(&context(48), &report).render_plain();

    assert!(rendered.starts_with(
        "! Daemon is running; history is catching up\n\
         The current search index remains available.\n"
    ));
    assert!(rendered.contains("Status    catching up\n"));
    assert!(rendered.contains("Progress  scanning provider sources\n"));
    assert!(rendered.contains("Source    ~/.local/share/opencode/opencode.db\n"));
    assert!(rendered.contains("Accepted  1,234 records\n"));
    assert!(rendered.contains("Scanned   4.0 MiB\n"));
    assert!(rendered.contains("ctx index watch\n"));
}

#[test]
fn recoverable_failure_surfaces_error_and_one_restart_action() {
    let report = json!({
        "enabled": true,
        "status": "stale_lock",
        "running": false,
        "recoverable": true,
        "reason": "daemon_lock_stale",
        "last_error": "the previous daemon exited unexpectedly",
        "config_reload": {"status": "applied"},
        "jobs": {
            "core_refresh": {"status": "completed"},
            "semantic_index": {"status": "disabled"}
        },
        "live_pid": 999,
        "lock_identity": {"owner_id": "omit-me"}
    });
    let rendered = render_status(&context(80), &report).render_plain();

    assert!(rendered.starts_with("✗ Daemon failed but can recover\n"));
    assert!(rendered.contains("Status    failed (recoverable)\n"));
    assert!(rendered.contains("Reason    daemon lock stale\n"));
    assert!(rendered.contains("Error     the previous daemon exited unexpectedly\n"));
    assert_eq!(rendered.matches("ctx daemon enable").count(), 1);
    assert!(!rendered.contains("999"));
    assert!(!rendered.contains("omit-me"));
}

#[test]
fn enabled_daemon_without_observed_lifecycle_is_not_a_failure() {
    let report = json!({
        "enabled": true,
        "status": "unknown",
        "running": false,
        "semantic_runtime_active": false,
        "config_reload": {"status": "unknown"},
        "jobs": {
            "core_refresh": {
                "status": "disabled",
                "reason": "not_started"
            },
            "semantic_index": {"status": "disabled"}
        }
    });

    for width in [32, 48, 80, 120] {
        let plain_context = context(width);
        let document = render_status(&plain_context, &report);
        let plain = document.render_plain();
        let normalized = plain.split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(normalized.starts_with(
            "! Daemon is enabled but has not started No daemon lifecycle state has been observed yet."
        ));
        assert!(normalized.contains("Service Status not started"));
        assert!(normalized.contains("History refresh Status disabled"));
        assert!(normalized.contains("Semantic Status disabled"));
        assert!(!plain.contains("Daemon failed"));
        assert!(!plain.contains("Status  failed"));
        assert!(!plain.contains("ctx daemon enable"));
        assert!(normalized.contains("Hint: Check daemon startup and service health."));
        assert!(plain.contains("\nNext\n  ctx doctor\n"));
        assert_eq!(strip_ansi(&document.render(&styled_context(width))), plain);
        assert_fits(&document, &plain_context);
    }
}

#[test]
fn source_rejections_are_visible_without_internal_provenance() {
    let mut report = running_report();
    report["jobs"]["core_refresh"] = json!({
        "status": "completed",
        "rejection_diagnostics": {"rejected_records": 3},
        "trigger_provenance": "internal-import-route"
    });
    let rendered = render_status(&context(80), &report).render_plain();

    assert!(rendered.starts_with("! Daemon is partially healthy\n"));
    assert!(rendered.contains("Status    ready with rejections\n"));
    assert!(rendered.contains("Rejected  3 records\n"));
    assert!(rendered.contains("ctx import --all --no-daemon\n"));
    assert!(!rendered.contains("internal-import-route"));
}

#[test]
fn failed_transcript_route_prevents_a_misleading_healthy_daemon_status() {
    let mut report = running_report();
    report["jobs"]["core_refresh"] = json!({
        "status": "completed",
        "request_state": "published",
        "receipt": {
            "outcome": "completed_with_source_failures",
            "source_failure_total": 1,
            "current": {"current_rejected_records": 0},
        },
    });
    let rendered = render_status(&context(80), &report).render_plain();

    assert!(rendered.starts_with("! Daemon is partially healthy\n"));
    assert!(
        rendered.contains("ready with source failures"),
        "{rendered}"
    );
    assert!(rendered.contains("transcript routes could not be refreshed"));
    assert!(!rendered.contains("Daemon is healthy"));
}

#[test]
fn failed_source_refresh_is_bounded_actionable_and_never_leaks_backend_details() {
    let backend_error = "all_provider_terminal_coverage_unavailable at /tmp/private/source";
    let report = json!({
        "enabled": true,
        "status": "failed",
        "running": false,
        "last_error": format!("source-backed refresh failed: {backend_error}"),
        "jobs": {
            "core_refresh": {
                "status": "failed",
                "last_error": backend_error,
                "certified_source_count": 0
            },
            "semantic_index": {"status": "unknown"}
        }
    });
    let wide = concat!(
        "✗ History refresh failed\n",
        "No new history generation was published.\n",
        "\n",
        "Service\n",
        "Status  failed\n",
        "\n",
        "History refresh\n",
        "Status   failed\n",
        "Sources  0 certified sources\n",
        "Issue    One or more history sources could not be refreshed.\n",
        "\n",
        "Semantic\n",
        "Status  unknown\n",
        "\n",
        "Hint: Inspect source-level refresh failures.\n",
        "\n",
        "Next\n",
        "  ctx import --all --no-daemon\n",
    );
    assert_exact_cross_width(
        |context| render_status(context, &report),
        [
            (
                32,
                concat!(
                    "✗ History refresh failed\n",
                    "No new history generation was\n",
                    "published.\n",
                    "\n",
                    "Service\n",
                    "Status  failed\n",
                    "\n",
                    "History refresh\n",
                    "Status   failed\n",
                    "Sources  0 certified sources\n",
                    "Issue    One or more history\n",
                    "         sources could not be\n",
                    "         refreshed.\n",
                    "\n",
                    "Semantic\n",
                    "Status  unknown\n",
                    "\n",
                    "Hint: Inspect source-level\n",
                    "      refresh failures.\n",
                    "\n",
                    "Next\n",
                    "  ctx import --all --no-daemon\n",
                ),
            ),
            (
                48,
                concat!(
                    "✗ History refresh failed\n",
                    "No new history generation was published.\n",
                    "\n",
                    "Service\n",
                    "Status  failed\n",
                    "\n",
                    "History refresh\n",
                    "Status   failed\n",
                    "Sources  0 certified sources\n",
                    "Issue    One or more history sources could not\n",
                    "         be refreshed.\n",
                    "\n",
                    "Semantic\n",
                    "Status  unknown\n",
                    "\n",
                    "Hint: Inspect source-level refresh failures.\n",
                    "\n",
                    "Next\n",
                    "  ctx import --all --no-daemon\n",
                ),
            ),
            (80, wide),
            (120, wide),
        ],
        concat!(
            "\u{1b}[31m✗\u{1b}[0m \u{1b}[1mHistory refresh failed\u{1b}[0m\n",
            "No new history generation was published.\n",
            "\n",
            "\u{1b}[1mService\u{1b}[0m\n",
            "\u{1b}[2mStatus\u{1b}[0m  \u{1b}[31mfailed\u{1b}[0m\n",
            "\n",
            "\u{1b}[1mHistory refresh\u{1b}[0m\n",
            "\u{1b}[2mStatus\u{1b}[0m   \u{1b}[31mfailed\u{1b}[0m\n",
            "\u{1b}[2mSources\u{1b}[0m  0 certified sources\n",
            "\u{1b}[2mIssue\u{1b}[0m    One or more history sources could not be refreshed.\n",
            "\n",
            "\u{1b}[1mSemantic\u{1b}[0m\n",
            "\u{1b}[2mStatus\u{1b}[0m  unknown\n",
            "\n",
            "\u{1b}[2mHint\u{1b}[0m: Inspect source-level refresh failures.\n",
            "\n",
            "\u{1b}[2mNext\u{1b}[0m\n",
            "  \u{1b}[36mctx import --all --no-daemon\u{1b}[0m\n",
        ),
    );

    let rendered = render_status(&context(80), &report).render_plain();
    assert_eq!(rendered.matches("ctx import --all --no-daemon").count(), 1);
    assert!(!rendered.contains("ctx daemon enable"));
    assert!(!rendered.contains("all_provider_terminal_coverage_unavailable"));
    assert!(!rendered.contains("/tmp/private/source"));
}

#[test]
fn semantic_fallback_names_backend_and_reason_but_not_model_identity() {
    let mut report = running_report();
    report["jobs"]["semantic_index"] = json!({
        "status": "completed",
        "model_key": "private-model-identity",
        "embedding_runtime": {
            "backend": "cpu",
            "compute_mode": "local_cpu",
            "acquisition_fallback": "cuda_driver_unavailable"
        }
    });
    let rendered = render_status(&context(80), &report).render_plain();

    assert!(rendered.starts_with("! Daemon is partially healthy\n"));
    assert!(rendered.contains("Status    ready with fallback\n"));
    assert!(rendered.contains("Backend   cpu\n"));
    assert!(rendered.contains("Compute   local cpu\n"));
    assert!(rendered.contains("Fallback  cuda driver unavailable\n"));
    assert!(!rendered.contains("private-model-identity"));
    assert!(!rendered.contains("\nNext\n"));
}

#[test]
fn enable_receipts_distinguish_managed_and_limited_persistence() {
    let managed = json!({
        "status": "installed",
        "registration_verified": true,
        "live_owner_verified": true
    });
    let wide = concat!(
        "✓ Daemon enabled\n",
        "Background history refresh will continue after this terminal closes.\n",
        "\n",
        "Service\n",
        "Status       running\n",
        "Persistence  managed\n",
    );
    assert_exact_cross_width(
        |context| {
            render_daemon_enable_receipt(
                context,
                true,
                true,
                &managed,
                Path::new("/tmp/ctx/config.toml"),
            )
        },
        [
            (
                32,
                concat!(
                    "✓ Daemon enabled\n",
                    "Background history refresh will\n",
                    "continue after this terminal\n",
                    "closes.\n",
                    "\n",
                    "Service\n",
                    "Status       running\n",
                    "Persistence  managed\n",
                ),
            ),
            (
                48,
                concat!(
                    "✓ Daemon enabled\n",
                    "Background history refresh will continue after\n",
                    "this terminal closes.\n",
                    "\n",
                    "Service\n",
                    "Status       running\n",
                    "Persistence  managed\n",
                ),
            ),
            (80, wide),
            (120, wide),
        ],
        concat!(
            "\u{1b}[32m✓\u{1b}[0m \u{1b}[1mDaemon enabled\u{1b}[0m\n",
            "Background history refresh will continue after this terminal closes.\n",
            "\n",
            "\u{1b}[1mService\u{1b}[0m\n",
            "\u{1b}[2mStatus\u{1b}[0m       \u{1b}[32mrunning\u{1b}[0m\n",
            "\u{1b}[2mPersistence\u{1b}[0m  \u{1b}[32mmanaged\u{1b}[0m\n",
        ),
    );
    let managed_rendered = render_daemon_enable_receipt(
        &context(80),
        true,
        true,
        &managed,
        Path::new("/tmp/ctx/config.toml"),
    )
    .render_plain();
    assert!(!managed_rendered.contains("config.toml"));
    assert!(!managed_rendered.contains("\nNext\n"));

    let limited = json!({
        "status": "fallback",
        "limitation": "native restart registration requires the hosted installer"
    });
    let limited_wide = concat!(
        "! Daemon running; persistence not verified\n",
        "Hint: Check supervisor status.\n",
        "\n",
        "Next\n",
        "  ctx daemon status\n",
    );
    assert_exact_cross_width(
        |context| {
            render_daemon_enable_receipt(
                context,
                true,
                false,
                &limited,
                Path::new("/tmp/ctx/config.toml"),
            )
        },
        [
            (
                32,
                concat!(
                    "! Daemon running; persistence\n",
                    "  not verified\n",
                    "Hint: Check supervisor status.\n",
                    "\n",
                    "Next\n",
                    "  ctx daemon status\n",
                ),
            ),
            (48, limited_wide),
            (80, limited_wide),
            (120, limited_wide),
        ],
        concat!(
            "\u{1b}[33m!\u{1b}[0m \u{1b}[1mDaemon running; persistence not verified\u{1b}[0m\n",
            "\u{1b}[2mHint\u{1b}[0m: Check supervisor status.\n",
            "\n",
            "\u{1b}[2mNext\u{1b}[0m\n",
            "  \u{1b}[36mctx daemon status\u{1b}[0m\n",
        ),
    );
    let limited_rendered = render_daemon_enable_receipt(
        &context(80),
        true,
        false,
        &limited,
        Path::new("/tmp/ctx/config.toml"),
    )
    .render_plain();
    assert_eq!(limited_rendered.lines().count(), 5);
    assert!(!limited_rendered.contains("config.toml"));
    assert!(!limited_rendered.contains("hosted installer"));
}

#[test]
fn disable_receipt_confirms_stop_and_supervisor_removal_without_noise() {
    let supervisor = json!({"status": "disabled"});
    let wide = concat!(
        "✓ Daemon disabled\n",
        "Background refresh is stopped and persistent startup was removed.\n",
        "\n",
        "Service\n",
        "Status       disabled\n",
        "Persistence  removed\n",
    );
    assert_exact_cross_width(
        |context| {
            render_daemon_disable_receipt(context, &supervisor, Path::new("/tmp/ctx/config.toml"))
        },
        [
            (
                32,
                concat!(
                    "✓ Daemon disabled\n",
                    "Background refresh is stopped\n",
                    "and persistent startup was\n",
                    "removed.\n",
                    "\n",
                    "Service\n",
                    "Status       disabled\n",
                    "Persistence  removed\n",
                ),
            ),
            (
                48,
                concat!(
                    "✓ Daemon disabled\n",
                    "Background refresh is stopped and persistent\n",
                    "startup was removed.\n",
                    "\n",
                    "Service\n",
                    "Status       disabled\n",
                    "Persistence  removed\n",
                ),
            ),
            (80, wide),
            (120, wide),
        ],
        concat!(
            "\u{1b}[32m✓\u{1b}[0m \u{1b}[1mDaemon disabled\u{1b}[0m\n",
            "Background refresh is stopped and persistent startup was removed.\n",
            "\n",
            "\u{1b}[1mService\u{1b}[0m\n",
            "\u{1b}[2mStatus\u{1b}[0m       disabled\n",
            "\u{1b}[2mPersistence\u{1b}[0m  \u{1b}[32mremoved\u{1b}[0m\n",
        ),
    );
    let rendered =
        render_daemon_disable_receipt(&context(80), &supervisor, Path::new("/tmp/ctx/config.toml"))
            .render_plain();
    assert!(!rendered.contains("config.toml"));
    assert!(!rendered.contains("\nNext\n"));
}

#[test]
fn uninstall_receipt_preserves_binary_and_data_caveat() {
    let report = json!({
        "ok": true,
        "scope": "installation",
        "installation_quiescent": true,
        "daemon_enabled": false,
        "daemon_running": false,
        "owner_lock_released": true,
        "endpoint_released": true,
        "supervisor_removed": true,
        "coordination_state_removed": true,
        "binary_retained": true,
        "retry_safe": true
    });
    let rendered = render_daemon_prepare_uninstall_receipt(&context(80), &report).render_plain();

    assert_eq!(
        rendered,
        "✓ Daemon prepared for uninstall\n\
         All registered daemon roots are disabled and stopped, and the singleton\n\
         supervisor registration was removed.\n\
         \n\
         Caveat  The ctx binary and history data have not been removed.\n"
    );
    assert!(!rendered.contains("\nNext\n"));
}

#[test]
fn representative_documents_fit_32_48_80_and_120_columns() {
    let mut partial = running_report();
    partial["jobs"]["core_refresh"] = json!({
        "status": "failed",
        "last_error": "provider history refresh failed after validating the selected source",
        "certified_source_count": 12000
    });
    partial["jobs"]["semantic_index"] = json!({
        "status": "completed",
        "embedding_runtime": {
            "backend": "cpu",
            "compute_mode": "local_cpu",
            "acquisition_fallback": "accelerator_runtime_unavailable"
        }
    });
    let limited = json!({
        "status": "fallback",
        "limitation": "native restart registration requires the hosted installer and the default data root"
    });
    let uninstall = json!({
        "ok": false,
        "daemon_enabled": false,
        "daemon_running": false,
        "owner_lock_released": false,
        "endpoint_released": true,
        "supervisor_removed": false,
        "coordination_state_removed": false
    });

    for width in [32, 48, 80, 120] {
        let context = context(width);
        for document in [
            render_status(&context, &running_report()),
            render_status(&context, &partial),
            render_daemon_enable_receipt(
                &context,
                true,
                false,
                &limited,
                Path::new("/very/long/data/root/with/config.toml"),
            ),
            render_daemon_prepare_uninstall_receipt(&context, &uninstall),
        ] {
            assert_fits(&document, &context);
        }
    }
}

#[test]
fn controls_are_neutralized_and_ansi_stripped_output_matches_plain() {
    let mut report = running_report();
    report["jobs"]["core_refresh"] = json!({
        "status": "failed",
        "last_error": "\u{1b}[31mowned\u{1b}[0m\nsecond line"
    });

    for width in [32, 48, 80, 120] {
        let plain_context = context(width);
        let styled_context = styled_context(width);
        let document = render_status(&plain_context, &report);
        let plain = document.render_plain();
        let styled = document.render(&styled_context);

        let compact_plain = plain
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert!(compact_plain.contains("Oneormorehistorysourcescouldnotberefreshed."));
        assert!(!plain.contains("owned"));
        assert!(!plain.contains("second line"));
        assert!(!plain.contains('\u{1b}'));
        assert_eq!(strip_ansi(&styled), plain);
        assert!(styled.contains("\u{1b}[2mStatus\u{1b}[0m"));
        assert!(styled.contains("\u{1b}[32mrunning\u{1b}[0m"));
        assert!(styled.contains("\u{1b}[31mfailed\u{1b}[0m"));
        assert!(!styled.contains("\u{1b}[31mprovider history"));
        assert_fits(&document, &plain_context);
    }
}
