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
            "history_refresh": {"status": "disabled"},
            "source_backed_refresh": {
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
        "source_refresh_endpoint": {"identity_path": "/tmp/internal-endpoint"},
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
            line.width() <= width,
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

#[test]
fn running_status_is_outcome_first_and_omits_internal_details() {
    let rendered = render_status(&context(80), &running_report()).render_plain();

    assert_eq!(
        rendered,
        "✓ Daemon is healthy\n\
         \n\
         Service\n\
         Status  running\n\
         \n\
         History refresh\n\
         Status  ready\n\
         Sources  1,248 certified sources\n\
         \n\
         Semantic\n\
         Status  active\n"
    );
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
            "history_refresh": {"status": "disabled"},
            "source_backed_refresh": {"status": "disabled"},
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
fn catching_up_status_keeps_search_availability_and_progress_visible() {
    let mut report = running_report();
    report["jobs"]["source_backed_refresh"] = json!({
        "status": "running",
        "progress": {"phase": "scanning_provider_sources"},
        "source_count": 9
    });
    let rendered = render_status(&context(48), &report).render_plain();

    assert!(rendered.starts_with(
        "! Daemon is running; history is catching up\n\
         The current search index remains available.\n"
    ));
    assert!(rendered.contains("Status  catching up\n"));
    assert!(rendered.contains("Progress  scanning provider sources\n"));
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
            "source_backed_refresh": {"status": "completed"},
            "semantic_index": {"status": "disabled"}
        },
        "live_pid": 999,
        "lock_identity": {"owner_id": "omit-me"}
    });
    let rendered = render_status(&context(80), &report).render_plain();

    assert!(rendered.starts_with("✗ Daemon failed but can recover\n"));
    assert!(rendered.contains("Status  failed (recoverable)\n"));
    assert!(rendered.contains("Reason    daemon lock stale\n"));
    assert!(rendered.contains("Error     the previous daemon exited unexpectedly\n"));
    assert_eq!(rendered.matches("ctx daemon enable").count(), 1);
    assert!(!rendered.contains("999"));
    assert!(!rendered.contains("omit-me"));
}

#[test]
fn source_rejections_are_visible_without_internal_provenance() {
    let mut report = running_report();
    report["jobs"]["history_refresh"] = json!({
        "status": "completed",
        "rejection_diagnostics": {"rejected_records": 3},
        "trigger_provenance": "internal-import-route"
    });
    let rendered = render_status(&context(80), &report).render_plain();

    assert!(rendered.starts_with("! Daemon is partially healthy\n"));
    assert!(rendered.contains("Status  ready with rejections\n"));
    assert!(rendered.contains("Rejected  3 records\n"));
    assert!(rendered.contains("ctx import --all --no-daemon\n"));
    assert!(!rendered.contains("internal-import-route"));
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
    assert!(rendered.contains("Status  ready with fallback\n"));
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
    let managed_rendered = render_daemon_enable_receipt(
        &context(80),
        true,
        true,
        &managed,
        Path::new("/tmp/ctx/config.toml"),
    )
    .render_plain();
    assert_eq!(
        managed_rendered,
        "✓ Daemon enabled\n\
         Background history refresh will continue after this terminal closes.\n\
         \n\
         Service\n\
         Status  running\n\
         Persistence  managed\n"
    );
    assert!(!managed_rendered.contains("config.toml"));
    assert!(!managed_rendered.contains("\nNext\n"));

    let limited = json!({
        "status": "fallback",
        "limitation": "native restart registration requires the hosted installer"
    });
    let limited_rendered = render_daemon_enable_receipt(
        &context(80),
        true,
        false,
        &limited,
        Path::new("/tmp/ctx/config.toml"),
    )
    .render_plain();
    assert!(limited_rendered.starts_with("! Daemon enabled with limited persistence\n"));
    assert!(limited_rendered.contains("Persistence  not verified\n"));
    assert!(limited_rendered
        .contains("Caveat  native restart registration requires the hosted installer\n"));
    assert!(limited_rendered.contains("Config  /tmp/ctx/config.toml\n"));
    assert!(!limited_rendered.contains("\nNext\n"));
}

#[test]
fn disable_receipt_confirms_stop_and_supervisor_removal_without_noise() {
    let supervisor = json!({"status": "disabled"});
    let rendered =
        render_daemon_disable_receipt(&context(80), &supervisor, Path::new("/tmp/ctx/config.toml"))
            .render_plain();

    assert_eq!(
        rendered,
        "✓ Daemon disabled\n\
         Background refresh is stopped and persistent startup was removed.\n\
         \n\
         Service\n\
         Status  disabled\n\
         Persistence  removed\n"
    );
    assert!(!rendered.contains("config.toml"));
    assert!(!rendered.contains("\nNext\n"));
}

#[test]
fn uninstall_receipt_preserves_binary_and_data_caveat() {
    let report = json!({
        "ok": true,
        "daemon_enabled": false,
        "daemon_running": false,
        "owner_lock_released": true,
        "endpoint_released": true,
        "supervisor_removed": true,
        "coordination_state_removed": true,
        "retry_safe": true
    });
    let rendered = render_daemon_prepare_uninstall_receipt(&context(80), &report).render_plain();

    assert_eq!(
        rendered,
        "✓ Daemon prepared for uninstall\n\
         The daemon is disabled and stopped, and its supervisor registration was\n\
         removed.\n\
         \n\
         Caveat  The ctx binary and history data have not been removed.\n"
    );
    assert!(!rendered.contains("\nNext\n"));
}

#[test]
fn representative_documents_fit_32_48_80_and_120_columns() {
    let mut partial = running_report();
    partial["jobs"]["source_backed_refresh"] = json!({
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
    report["jobs"]["source_backed_refresh"] = json!({
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
        assert!(compact_plain.contains("\\x1b[31mowned\\x1b[0m\\nsecondline"));
        assert!(!plain.contains('\u{1b}'));
        assert_eq!(strip_ansi(&styled), plain);
        assert!(styled.contains("\u{1b}[2mStatus\u{1b}[0m"));
        assert!(styled.contains("\u{1b}[32mrunning\u{1b}[0m"));
        assert!(styled.contains("\u{1b}[31mfailed\u{1b}[0m"));
        assert!(!styled.contains("\u{1b}[31mprovider history"));
        assert_fits(&document, &plain_context);
    }
}
