use std::path::PathBuf;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::analytics::{self, SetupMode, SetupTelemetry};
use crate::config::CONFIG_FILE;
use crate::output::print_json;
use crate::semantic::{
    autostart_daemon_and_wait, coordinate_source_backed_refresh,
    daemon_autostart_suppression_reason, semantic_query_service_supported,
    source_epoch_status_report, DaemonHandoff, SourceBackedRefreshMode,
};
use crate::ui::{
    fields, outcome, section, Document, Field, Line, Outcome, OutcomeState, RenderContext, Span,
    Token, Ui,
};
use crate::{config, SetupArgs};

pub(crate) fn run_setup(
    args: SetupArgs,
    data_root: PathBuf,
    telemetry: &mut SetupTelemetry,
    _provider_refreshes: &mut crate::commands::import::ProviderRefreshCollector,
    quiet: bool,
    config: &mut config::AppConfig,
    ui: &mut Ui,
) -> Result<()> {
    let semantic_supported = semantic_query_service_supported();
    if args.semantic && (!config.daemon.enabled || args.no_daemon) {
        bail!(
            "`ctx setup --semantic` requires daemon maintenance. Enable [daemon] enabled = true and rerun without --no-daemon"
        );
    }
    if args.semantic {
        config::set_semantic_search_enabled(&data_root, true)?;
        config.search.semantic = Some(true);
    }
    let semantic_enabled = config.semantic_search_enabled();
    if semantic_enabled && semantic_supported && (!config.daemon.enabled || args.no_daemon) {
        bail!(
            "local semantic search requires the ctx daemon. Set [daemon] enabled = true, remove --no-daemon, or set [search] semantic = false"
        );
    }

    config::write_default_config(&data_root)?;

    let json_output = args.format.is_json();
    let suppression_reason = daemon_autostart_suppression_reason();
    let daemon_autostart_requested =
        config.daemon.enabled && !args.no_daemon && suppression_reason.is_none();
    let daemon_autostart_reason = if args.no_daemon {
        Some("explicit_opt_out")
    } else if !config.daemon.enabled {
        Some("daemon_disabled")
    } else {
        suppression_reason
    };
    let daemon_handoff = if daemon_autostart_requested {
        Some(autostart_daemon_and_wait(
            &data_root,
            config,
            crate::DaemonTriggerCommandArg::Setup,
        )?)
    } else {
        None
    };
    let refresh_request = request_source_refresh(
        &data_root,
        config.daemon.enabled,
        args.no_daemon,
        args.wait,
        daemon_autostart_reason,
    );
    let source = source_epoch_status_report(&data_root, config)?;
    let supervisor = source.report["daemon"]["supervisor"].clone();
    let lexical_status = source.report["lexical"]["status"]
        .as_str()
        .unwrap_or("unavailable");
    telemetry.mode = Some(if lexical_status == "ready" {
        SetupMode::Ready
    } else {
        SetupMode::Background
    });
    telemetry.providers_detected = source.indexed_sources.map(analytics::count_bucket);
    telemetry.has_indexed_content = source.indexed_items.map(|count| count > 0);

    let mode = match lexical_status {
        "ready" => "ready",
        "pending" => "pending",
        "stale" => "stale",
        _ => "unavailable",
    };
    let output = json!({
        "schema_version": 2,
        "data_root": data_root,
        "config_path": data_root.join(CONFIG_FILE),
        "mode": mode,
        "history_epoch": source.report["history_epoch"].clone(),
        "lexical": source.report["lexical"].clone(),
        "catalog": source.report["catalog"].clone(),
        "resolver": source.report["resolver"].clone(),
        "refresh": source.report["refresh"].clone(),
        "refresh_request": refresh_request,
        "semantic": source.report["semantic"].clone(),
        "relational": source.report["relational"].clone(),
        "pro_projection": source.report["pro_projection"].clone(),
        "daemon": source.report["daemon"].clone(),
        "daemon_autostart": daemon_autostart_json(
            daemon_autostart_requested,
            daemon_autostart_reason,
            daemon_handoff.as_ref(),
            &supervisor,
        ),
        "deprecated_catalog_only_ignored": args.catalog_only,
        "network_required": false,
        "repo_writes": false,
    });

    if json_output {
        print_json(output)?;
    } else if !quiet {
        let document = render_setup_human(
            ui.stdout_context(),
            &data_root,
            mode,
            &source.report,
            &refresh_request,
            DaemonAutostartHuman {
                requested: daemon_autostart_requested,
                reason: daemon_autostart_reason,
                handoff: daemon_handoff.as_ref(),
                supervisor: &supervisor,
            },
        );
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn request_source_refresh(
    data_root: &std::path::Path,
    daemon_enabled: bool,
    no_daemon: bool,
    wait: bool,
    daemon_unavailable_reason: Option<&str>,
) -> Value {
    if no_daemon || !daemon_enabled {
        return json!({
            "status": "unavailable",
            "reason": if no_daemon {
                "explicit_opt_out"
            } else {
                "daemon_disabled"
            },
            "mode": if wait { "wait" } else { "background" },
            "daemon_available": false,
        });
    }
    let mode = if wait {
        SourceBackedRefreshMode::Wait
    } else {
        SourceBackedRefreshMode::Background
    };
    match coordinate_source_backed_refresh(data_root, mode) {
        Ok(observation) => {
            let receipt = observation
                .receipt
                .as_ref()
                .map(|receipt| receipt.to_json());
            json!({
                "status": observation.status,
                "reason": Value::Null,
                "mode": if wait { "wait" } else { "background" },
                "request_id": observation.request_id,
                "daemon_available": observation.daemon_available,
                "source_count": observation.source_count,
                "published_generation": observation.pin.generation_id(),
                "receipt": receipt,
            })
        }
        Err(error) => {
            let daemon_unavailable = error
                .downcast_ref::<crate::semantic::SourceBackedRefreshDaemonUnavailable>()
                .is_some();
            json!({
                "status": if daemon_unavailable {
                    "unavailable"
                } else if !wait {
                    "pending"
                } else {
                    "unavailable"
                },
                "reason": if daemon_unavailable {
                    daemon_unavailable_reason.unwrap_or("daemon_unavailable")
                } else if !wait {
                    "refresh_queued_without_published_generation"
                } else {
                    "refresh_failed"
                },
                "mode": if wait { "wait" } else { "background" },
                "daemon_available": !daemon_unavailable,
                "last_error": format!("{error:#}"),
            })
        }
    }
}

fn daemon_autostart_json(
    requested: bool,
    reason: Option<&str>,
    handoff: Option<&DaemonHandoff>,
    supervisor: &Value,
) -> Value {
    let persistently_supervised = supervisor_persistently_verified(supervisor);
    match handoff {
        Some(handoff) => json!({
            "status": if persistently_supervised { "verified" } else { "degraded" },
            "reason": if persistently_supervised {
                Value::Null
            } else {
                Value::String("native_supervisor_unavailable".to_owned())
            },
            "requested": requested,
            "pid": handoff.pid,
            "persistent": persistently_supervised,
            "supervisor": supervisor,
            "status_command": "ctx daemon status",
        }),
        None => json!({
            "status": if requested { "unavailable" } else { "not_requested" },
            "reason": reason.unwrap_or("not_requested"),
            "requested": requested,
            "persistent": false,
            "supervisor": supervisor,
            "status_command": "ctx daemon status",
        }),
    }
}

fn supervisor_persistently_verified(supervisor: &Value) -> bool {
    supervisor.get("status").and_then(Value::as_str) == Some("installed")
        && supervisor
            .get("registration_verified")
            .and_then(Value::as_bool)
            == Some(true)
        && supervisor
            .get("live_owner_verified")
            .and_then(Value::as_bool)
            == Some(true)
}

struct DaemonAutostartHuman<'a> {
    requested: bool,
    reason: Option<&'a str>,
    handoff: Option<&'a DaemonHandoff>,
    supervisor: &'a Value,
}

fn render_setup_human(
    context: &RenderContext,
    data_root: &std::path::Path,
    mode: &str,
    source: &Value,
    refresh_request: &Value,
    daemon: DaemonAutostartHuman<'_>,
) -> Document {
    let refresh_status = refresh_request["status"].as_str().unwrap_or("unavailable");
    let queued = mode == "pending"
        || matches!(
            refresh_status,
            "accepted" | "pending" | "queued" | "running"
        );
    let (state, title, detail) = if mode == "ready" {
        (
            OutcomeState::Success,
            "History is ready to search",
            queued.then_some("A refresh is running; the current index remains searchable."),
        )
    } else if queued {
        (
            OutcomeState::Neutral,
            "History indexing is queued",
            Some("Background indexing will publish the first searchable index."),
        )
    } else {
        (
            OutcomeState::Warning,
            "History is not ready",
            Some("Setup completed, but no verified search index is available."),
        )
    };
    let mut document = outcome(
        context,
        Outcome {
            state,
            title,
            detail,
        },
    );

    let source_count = source["indexed_sources"]
        .as_u64()
        .or_else(|| source["lexical"]["certified_sources"].as_u64())
        .or_else(|| source["refresh"]["certified_source_count"].as_u64())
        .or_else(|| refresh_request["source_count"].as_u64());
    let mut history_values = Vec::new();
    if let Some(count) = source_count {
        history_values.push(("Sources", counted(count, "source", "sources").to_owned()));
    }
    if let Some(count) = source["indexed_sessions"].as_u64() {
        history_values.push(("Sessions", counted(count, "session", "sessions")));
    }
    let event_count = source["indexed_events"]
        .as_u64()
        .or_else(|| source["lexical"]["indexed_documents"].as_u64());
    if let Some(count) = event_count {
        history_values.push((
            "Events",
            counted(count, "searchable event", "searchable events"),
        ));
    }
    history_values.push(("Refresh", human_refresh_status(refresh_request).to_owned()));
    history_values.push(("Semantic", component_status(&source["semantic"]).to_owned()));
    if let Some(status) = daemon_human_status(&daemon) {
        history_values.push(("Background", status));
    }
    let history_fields = history_values
        .iter()
        .map(|(label, value)| Field::new(label, value.as_str()))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("History", fields(context, &history_fields)));

    if mode != "ready" && !queued {
        let data_root = data_root.display().to_string();
        document.push_blank();
        document.append(section(
            "Data",
            fields(context, &[Field::new("Root", &data_root)]),
        ));
    }

    let next_command = if mode == "ready" {
        "ctx search \"test failure\""
    } else if queued {
        "ctx index watch"
    } else if daemon.requested && daemon.handoff.is_none() {
        "ctx daemon status"
    } else if matches!(daemon.reason, Some("daemon_disabled" | "explicit_opt_out")) {
        "ctx daemon enable"
    } else {
        "ctx doctor"
    };
    document.push_blank();
    document.append(section(
        "Next",
        Document::from_line(
            Line::new()
                .with(Span::text("  "))
                .with(Span::new(next_command, Token::Command)),
        ),
    ));
    document
}

fn component_status(component: &Value) -> &str {
    component
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unavailable")
}

fn human_refresh_status(refresh_request: &Value) -> String {
    let status = component_status(refresh_request);
    match status {
        "accepted" | "pending" | "queued" | "running" => "in progress".to_owned(),
        "published" => "ready".to_owned(),
        "unavailable" => refresh_request
            .get("reason")
            .and_then(Value::as_str)
            .map(humanize_code)
            .map(|reason| format!("unavailable ({reason})"))
            .unwrap_or_else(|| "unavailable".to_owned()),
        status => humanize_code(status),
    }
}

fn daemon_human_status(daemon: &DaemonAutostartHuman<'_>) -> Option<String> {
    match daemon.handoff {
        Some(_) if supervisor_persistently_verified(daemon.supervisor) => None,
        Some(_) => {
            let limitation = daemon
                .supervisor
                .get("limitation")
                .and_then(Value::as_str)
                .unwrap_or("persistent supervision is unavailable");
            Some(format!("running with degraded maintenance ({limitation})"))
        }
        None if daemon.requested => {
            Some("startup was not verified; run ctx daemon status".to_owned())
        }
        None if daemon.reason == Some("explicit_opt_out") => {
            Some("skipped because --no-daemon was used".to_owned())
        }
        None if daemon.reason == Some("daemon_disabled") => Some("disabled".to_owned()),
        None => None,
    }
}

fn humanize_code(value: &str) -> String {
    value.replace('_', " ")
}

fn counted(count: u64, singular: &str, plural: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    format!("{} {noun}", grouped_count(count))
}

fn grouped_count(count: u64) -> String {
    let digits = count.to_string();
    let mut reversed = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (index, character) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            reversed.push(',');
        }
        reversed.push(character);
    }
    reversed.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;
    use unicode_width::UnicodeWidthStr as _;

    use crate::ui::{ColorMode, StreamKind, TestContext};

    use super::*;

    fn context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let width = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            assert!(line.width() <= width, "{line:?} exceeded {width} columns");
        }
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    fn installed_supervisor() -> Value {
        json!({
            "status": "installed",
            "registration_verified": true,
            "live_owner_verified": true,
        })
    }

    fn ready_source() -> Value {
        json!({
            "indexed_sources": 1,
            "indexed_sessions": 2,
            "indexed_events": 1000,
            "lexical": {
                "status": "ready",
                "certified_sources": 9,
                "indexed_documents": 9,
            },
            "refresh": {"status": "ready"},
            "semantic": {"status": "disabled"},
        })
    }

    fn render_ready(context: &RenderContext) -> Document {
        let supervisor = installed_supervisor();
        render_setup_human(
            context,
            std::path::Path::new("/tmp/ctx"),
            "ready",
            &ready_source(),
            &json!({"status": "published"}),
            DaemonAutostartHuman {
                requested: false,
                reason: None,
                handoff: None,
                supervisor: &supervisor,
            },
        )
    }

    #[test]
    fn setup_ready_is_outcome_first_and_has_one_search_action() {
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_ready(&context);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("✓ History is ready to search\n\nHistory\n"));
            assert!(rendered.contains("Sources   1 source\n"));
            assert!(rendered.contains("Sessions  2 sessions\n"));
            assert!(normalized.contains("Events 1,000 searchable events"));
            assert!(!rendered.contains("9 sources"));
            assert!(!rendered.contains("9 searchable events"));
            assert!(rendered.contains("Next\n  ctx search \"test failure\"\n"));
            assert!(!rendered.contains("Generation"));
            assert!(!rendered.contains("PID"));
            assert!(!rendered.contains("\nData\n"));
            assert!(!rendered.contains("/tmp/ctx"));
            assert_eq!(rendered.matches("\nNext\n").count(), 1);
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_events_fall_back_to_the_equivalent_lexical_document_count() {
        let source = json!({
            "lexical": {
                "status": "ready",
                "indexed_documents": 1,
            },
            "refresh": {"status": "ready"},
            "semantic": {"status": "disabled"},
        });
        let supervisor = installed_supervisor();
        let document = render_setup_human(
            &context(80, ColorMode::Never),
            std::path::Path::new("/tmp/ctx"),
            "ready",
            &source,
            &json!({"status": "published"}),
            DaemonAutostartHuman {
                requested: false,
                reason: None,
                handoff: None,
                supervisor: &supervisor,
            },
        );
        let rendered = document.render_plain();
        assert!(rendered.contains("Events    1 searchable event\n"));
        assert!(!rendered.contains("Sessions"));
    }

    #[test]
    fn setup_queued_has_watch_as_its_primary_action_without_an_eta() {
        let source = json!({
            "lexical": {"status": "pending"},
            "refresh": {"status": "pending"},
            "semantic": {"status": "disabled"},
        });
        let refresh = json!({"status": "pending"});
        let supervisor = installed_supervisor();
        let handoff = DaemonHandoff {
            pid: 42,
            heartbeat_at_ms: 1,
        };

        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_setup_human(
                &context,
                std::path::Path::new("/tmp/ctx"),
                "pending",
                &source,
                &refresh,
                DaemonAutostartHuman {
                    requested: true,
                    reason: None,
                    handoff: Some(&handoff),
                    supervisor: &supervisor,
                },
            );
            let rendered = document.render_plain();
            assert!(rendered.starts_with("History indexing is queued\n"));
            assert!(rendered.contains("Refresh   in progress\n"));
            assert!(rendered.contains("Next\n  ctx index watch\n"));
            assert!(!rendered.contains("Estimated"));
            assert!(!rendered.contains("ctx search"));
            assert!(!rendered.contains("42"));
            assert!(!rendered.contains("\nData\n"));
            assert!(!rendered.contains("/tmp/ctx"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_degraded_explains_disabled_background_work_and_recovery() {
        let source = json!({
            "lexical": {"status": "unavailable"},
            "refresh": {"status": "unavailable"},
            "semantic": {"status": "disabled"},
        });
        let refresh = json!({
            "status": "unavailable",
            "reason": "daemon_disabled",
        });
        let supervisor = json!({"status": "not_installed"});

        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_setup_human(
                &context,
                std::path::Path::new("/tmp/ctx"),
                "unavailable",
                &source,
                &refresh,
                DaemonAutostartHuman {
                    requested: false,
                    reason: Some("daemon_disabled"),
                    handoff: None,
                    supervisor: &supervisor,
                },
            );
            let rendered = document.render_plain();
            assert!(rendered.starts_with("! History is not ready\n"));
            assert!(rendered.contains("Background  disabled\n"));
            assert!(rendered.contains("Data\nRoot  /tmp/ctx\n"));
            assert!(rendered.contains("Next\n  ctx daemon enable\n"));
            assert!(!rendered.contains("ctx index watch"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_plain_output_equals_ansi_stripped_styled_output() {
        let context = context(80, ColorMode::Always);
        let document = render_ready(&context);
        assert_eq!(
            strip_ansi(&document.render(&context)),
            document.render_plain()
        );
    }

    #[test]
    fn setup_source_has_no_legacy_store_runtime_dependency() {
        let source = include_str!("setup.rs");
        let runtime = source.split("#[cfg(test)]").next().unwrap();
        for forbidden in [
            "ctx_history_store::Store",
            "Store::open",
            "run_import_internal",
            "inventory_available_sources",
            "ctx import --all",
        ] {
            assert!(
                !runtime.contains(forbidden),
                "setup retained forbidden legacy dependency {forbidden}"
            );
        }
    }
}
