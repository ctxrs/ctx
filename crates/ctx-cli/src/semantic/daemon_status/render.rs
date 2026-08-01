use std::path::Path;

use serde_json::Value;

use crate::ui::{
    fields, hint, outcome, section, Action, Document, Field, Hint, Outcome, OutcomeState,
    RenderContext, Token,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonPresentation {
    Healthy,
    Partial,
    Failed,
    Completed,
    NotStarted,
    Stopped,
    Disabled,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::semantic) struct DaemonStatusView<'a> {
    daemon: &'a Value,
    pro_status: Option<&'a str>,
}

impl<'a> DaemonStatusView<'a> {
    pub(in crate::semantic) fn daemon_only(daemon: &'a Value) -> Self {
        Self {
            daemon,
            pro_status: None,
        }
    }

    pub(in crate::semantic) fn from_reports(daemon: &'a Value, pro: &'a Value) -> Self {
        let pro_status = (pro.get("installed").and_then(Value::as_bool) == Some(true)).then(|| {
            pro.get("state")
                .and_then(Value::as_str)
                .unwrap_or("unavailable")
        });
        Self { daemon, pro_status }
    }
}

/// Builds the human daemon status document without reading runtime state or
/// writing to a terminal. JSON output deliberately bypasses this renderer.
pub(in crate::semantic) fn render_daemon_status_human(
    context: &RenderContext,
    view: DaemonStatusView<'_>,
) -> Document {
    let DaemonStatusView { daemon, pro_status } = view;
    let enabled = daemon
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let running = daemon
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let recoverable = daemon
        .get("recoverable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status = daemon
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let history = daemon
        .get("jobs")
        .and_then(|jobs| jobs.get("history_refresh"));
    let source_refresh = daemon
        .get("jobs")
        .and_then(|jobs| jobs.get("source_backed_refresh"));
    let semantic = daemon
        .get("jobs")
        .and_then(|jobs| jobs.get("semantic_index"));

    let rejected_records = rejected_record_count(history);
    let history_failed = job_failed(history) || job_failed(source_refresh);
    let semantic_failed = job_failed(semantic);
    let history_catching_up = job_catching_up(history) || job_catching_up(source_refresh);
    let semantic_fallback = semantic_fallback(semantic);
    let config_issue = config_reload_issue(daemon);
    let supervisor_issue = daemon
        .get("supervisor")
        .and_then(supervisor_persistence_issue);
    let daemon_error = daemon
        .get("last_error")
        .and_then(Value::as_str)
        .is_some_and(|error| !error.is_empty());
    let service_issue = config_issue || supervisor_issue.is_some() || daemon_error;
    let service_failed = recoverable
        || matches!(status, "failed" | "stale_lock")
        || (!running && enabled && status != "completed");

    let presentation = if status == "completed" {
        DaemonPresentation::Completed
    } else if !enabled || status == "disabled" {
        DaemonPresentation::Disabled
    } else if !running && status == "unknown" {
        DaemonPresentation::NotStarted
    } else if service_failed {
        DaemonPresentation::Failed
    } else if running
        && (history_failed
            || semantic_failed
            || history_catching_up
            || rejected_records > 0
            || semantic_fallback.is_some()
            || service_issue)
    {
        DaemonPresentation::Partial
    } else if running {
        DaemonPresentation::Healthy
    } else {
        DaemonPresentation::Stopped
    };

    let (outcome_state, title, detail) = match presentation {
        DaemonPresentation::Healthy => (OutcomeState::Success, "Daemon is healthy", None),
        DaemonPresentation::Partial if history_catching_up => (
            OutcomeState::Warning,
            "Daemon is running; history is catching up",
            Some("The current search index remains available."),
        ),
        DaemonPresentation::Partial if rejected_records > 0 => (
            OutcomeState::Warning,
            "Daemon is partially healthy",
            Some("History refresh rejected one or more records."),
        ),
        DaemonPresentation::Partial if semantic_fallback.is_some() => (
            OutcomeState::Warning,
            "Daemon is partially healthy",
            Some("Semantic search is using a fallback backend."),
        ),
        DaemonPresentation::Partial => (
            OutcomeState::Warning,
            "Daemon is partially healthy",
            Some("One or more background services need attention."),
        ),
        DaemonPresentation::Failed if recoverable => (
            OutcomeState::Error,
            "Daemon failed but can recover",
            Some("The previous daemon did not shut down cleanly. Restarting it is safe."),
        ),
        DaemonPresentation::Failed if history_failed => (
            OutcomeState::Error,
            "History refresh failed",
            Some("No new history generation was published."),
        ),
        DaemonPresentation::Failed if semantic_failed => (
            OutcomeState::Error,
            "Semantic indexing failed",
            Some("Keyword search remains available."),
        ),
        DaemonPresentation::Failed => (
            OutcomeState::Error,
            "Daemon failed",
            Some("Automatic history refresh is not running."),
        ),
        DaemonPresentation::Completed => (OutcomeState::Success, "Daemon run completed", None),
        DaemonPresentation::NotStarted => (
            OutcomeState::Warning,
            "Daemon is enabled but has not started",
            Some("No daemon lifecycle state has been observed yet."),
        ),
        DaemonPresentation::Stopped => (OutcomeState::Neutral, "Daemon is not running", None),
        DaemonPresentation::Disabled => (
            OutcomeState::Neutral,
            "Daemon is disabled",
            Some("Automatic history refresh and semantic serving are off."),
        ),
    };
    let mut document = outcome(
        context,
        Outcome {
            state: outcome_state,
            title,
            detail,
        },
    );

    let (service_state, service_token) = service_state(enabled, running, recoverable, status);
    let mut service = vec![state_field("Status", service_state, service_token)];
    let mut service_details = Vec::new();
    if let Some(mode) = daemon
        .get("mode")
        .and_then(Value::as_str)
        .filter(|mode| *mode != "full")
    {
        service_details.push(("Mode", humanize_code(mode)));
    }
    if let Some(issue) = supervisor_issue.as_deref() {
        service.push(state_field("Persistence", "not verified", Token::Warning));
        service_details.push(("Caveat", issue.to_owned()));
    }
    if config_issue {
        let reload = daemon.get("config_reload");
        let reload_status = reload
            .and_then(|reload| reload.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        service.push(state_field(
            "Configuration",
            config_state(reload_status),
            Token::Warning,
        ));
        if let Some(error) = job_error(reload) {
            push_unique_detail(&mut service_details, "Error", error);
        }
    }
    if recoverable {
        service_details.push(("Recovery", "available".to_owned()));
    }
    if matches!(presentation, DaemonPresentation::Completed) && !enabled {
        service_details.push(("Automatic refresh", "disabled".to_owned()));
    }
    if matches!(presentation, DaemonPresentation::Failed) {
        if let Some(reason) = daemon
            .get("reason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty())
        {
            service_details.push(("Reason", humanize_code(reason)));
        }
    }
    if let Some(error) = daemon
        .get("last_error")
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty() && !history_failed && !semantic_failed)
    {
        push_unique_detail(&mut service_details, "Error", error);
    }
    append_details(&mut service, &service_details);
    document.push_blank();
    document.append(section("Service", fields(context, &service)));

    if history.is_some() || source_refresh.is_some() {
        let (history_state, history_token) = history_state(
            history,
            source_refresh,
            enabled || matches!(presentation, DaemonPresentation::Completed),
            rejected_records,
        );
        let mut history_fields = vec![state_field("Status", history_state, history_token)];
        let mut history_details = Vec::new();
        if history_catching_up {
            if let Some(phase) = source_refresh
                .and_then(|job| job.get("progress"))
                .and_then(|progress| progress.get("phase"))
                .and_then(Value::as_str)
                .filter(|phase| !phase.is_empty())
            {
                history_details.push(("Progress", humanize_code(phase)));
            }
        }
        if let Some(count) = source_refresh
            .and_then(|job| {
                job.get("certified_source_count")
                    .or_else(|| job.get("source_count"))
            })
            .and_then(Value::as_u64)
        {
            history_details.push((
                "Sources",
                counted(count, "certified source", "certified sources"),
            ));
        }
        if rejected_records > 0 {
            history_details.push(("Rejected", counted(rejected_records, "record", "records")));
        }
        if history_failed {
            history_details.push((
                "Issue",
                "One or more history sources could not be refreshed.".to_owned(),
            ));
        } else {
            for error in [job_error(source_refresh), job_error(history)]
                .into_iter()
                .flatten()
            {
                push_unique_detail(&mut history_details, "Error", error);
            }
        }
        append_details(&mut history_fields, &history_details);
        document.push_blank();
        document.append(section("History refresh", fields(context, &history_fields)));
    }

    if semantic.is_some() || daemon.get("semantic_runtime_active").is_some() {
        let runtime_active = daemon
            .get("semantic_runtime_active")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (semantic_state, semantic_token) =
            semantic_state(semantic, runtime_active, semantic_fallback);
        let mut semantic_fields = vec![state_field("Status", semantic_state, semantic_token)];
        let mut semantic_details = Vec::new();
        if let Some(fallback) = semantic_fallback {
            let runtime = semantic.and_then(|job| job.get("embedding_runtime"));
            if let Some(backend) = runtime
                .and_then(|runtime| runtime.get("backend"))
                .and_then(Value::as_str)
                .filter(|backend| !backend.is_empty())
            {
                semantic_details.push(("Backend", humanize_code(backend)));
            }
            if let Some(compute) = runtime
                .and_then(|runtime| runtime.get("compute_mode"))
                .and_then(Value::as_str)
                .filter(|compute| !compute.is_empty())
            {
                semantic_details.push(("Compute", humanize_code(compute)));
            }
            semantic_details.push(("Fallback", humanize_code(fallback)));
        }
        if let Some(reason) = semantic
            .and_then(|job| job.get("reason"))
            .and_then(Value::as_str)
            .filter(|reason| {
                !matches!(
                    *reason,
                    "semantic_disabled" | "daemon_disabled" | "daemon_mode_source_refresh_only"
                )
            })
        {
            semantic_details.push(("Reason", humanize_code(reason)));
        }
        if semantic_failed {
            semantic_details.push(("Issue", "Semantic indexing could not complete.".to_owned()));
        } else if let Some(error) = job_error(semantic) {
            push_unique_detail(&mut semantic_details, "Error", error);
        }
        append_details(&mut semantic_fields, &semantic_details);
        document.push_blank();
        document.append(section("Semantic", fields(context, &semantic_fields)));
    }

    if let Some(pro_status) = pro_status {
        document.push_blank();
        document.append(section(
            "Pro",
            fields(context, &[state_field("Status", pro_status, Token::Text)]),
        ));
    }

    if let Some((message, command)) = recovery_action(
        presentation,
        recoverable,
        history_failed,
        rejected_records,
        semantic_failed,
        history_catching_up,
        service_issue,
    ) {
        document.push_blank();
        document.append(hint(
            context,
            Hint { text: message },
            Some(Action { command }),
        ));
    }
    document
}

/// Builds the `ctx daemon enable` receipt from values the lifecycle operation
/// has already established.
pub(in crate::semantic) fn render_daemon_enable_receipt(
    context: &RenderContext,
    running: bool,
    persistent: bool,
    supervisor: &Value,
    config_path: &Path,
) -> Document {
    render_daemon_enabled_receipt(context, true, running, persistent, supervisor, config_path)
}

/// Builds the ordinary `ctx daemon disable` receipt from the post-operation
/// supervisor report.
pub(in crate::semantic) fn render_daemon_disable_receipt(
    context: &RenderContext,
    supervisor: &Value,
    config_path: &Path,
) -> Document {
    render_daemon_enabled_receipt(context, false, false, false, supervisor, config_path)
}

fn render_daemon_enabled_receipt(
    context: &RenderContext,
    enabled: bool,
    running: bool,
    persistent: bool,
    supervisor: &Value,
    config_path: &Path,
) -> Document {
    if enabled && running && !persistent {
        let mut document = outcome(
            context,
            Outcome {
                state: OutcomeState::Warning,
                title: "Daemon running; persistence not verified",
                detail: None,
            },
        );
        document.append(hint(
            context,
            Hint {
                text: "Check supervisor status.",
            },
            Some(Action {
                command: "ctx daemon status",
            }),
        ));
        return document;
    }

    let supervisor_disabled = supervisor.get("status").and_then(Value::as_str) == Some("disabled");
    let (state, title, detail) = if enabled && running && persistent {
        (
            OutcomeState::Success,
            "Daemon enabled",
            "Background history refresh will continue after this terminal closes.",
        )
    } else if enabled {
        (
            OutcomeState::Error,
            "Daemon enabled but not running",
            "The preference was saved, but startup was not verified.",
        )
    } else if supervisor_disabled {
        (
            OutcomeState::Success,
            "Daemon disabled",
            "Background refresh is stopped and persistent startup was removed.",
        )
    } else {
        (
            OutcomeState::Warning,
            "Daemon disabled with a supervisor caveat",
            "Background refresh is stopped, but supervisor removal was not verified.",
        )
    };
    let mut document = outcome(
        context,
        Outcome {
            state,
            title,
            detail: Some(detail),
        },
    );

    let mut service = vec![if enabled {
        state_field(
            "Status",
            if running { "running" } else { "not running" },
            if running {
                Token::Success
            } else {
                Token::Error
            },
        )
    } else {
        state_field("Status", "disabled", Token::Text)
    }];
    service.push(state_field(
        "Persistence",
        if enabled && persistent {
            "managed"
        } else if enabled {
            "not verified"
        } else if supervisor_disabled {
            "removed"
        } else {
            "needs attention"
        },
        if enabled && persistent || (!enabled && supervisor_disabled) {
            Token::Success
        } else {
            Token::Warning
        },
    ));

    let mut details = Vec::new();
    if enabled && !persistent || !enabled && !supervisor_disabled {
        let issue = supervisor_persistence_issue(supervisor).unwrap_or_else(|| {
            let status = supervisor
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            format!("supervisor status is {}", humanize_code(status))
        });
        details.push(("Caveat", issue));
        details.push(("Config", config_path.display().to_string()));
    }
    append_details(&mut service, &details);
    document.push_blank();
    document.append(section("Service", fields(context, &service)));

    if enabled && !running {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Retry startup after resolving the service error.",
            },
            Some(Action {
                command: "ctx daemon enable",
            }),
        ));
    } else if !enabled && !supervisor_disabled {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Inspect the remaining supervisor state.",
            },
            Some(Action {
                command: "ctx doctor",
            }),
        ));
    }
    document
}

/// Builds the hosted-uninstaller handoff receipt without removing files or
/// changing daemon state.
pub(in crate::semantic) fn render_daemon_prepare_uninstall_receipt(
    context: &RenderContext,
    report: &Value,
) -> Document {
    let complete = report.get("ok").and_then(Value::as_bool) == Some(true)
        && report.get("scope").and_then(Value::as_str) == Some("installation")
        && report
            .get("installation_quiescent")
            .and_then(Value::as_bool)
            == Some(true)
        && report.get("daemon_enabled").and_then(Value::as_bool) == Some(false)
        && report.get("daemon_running").and_then(Value::as_bool) == Some(false)
        && report.get("owner_lock_released").and_then(Value::as_bool) == Some(true)
        && report.get("endpoint_released").and_then(Value::as_bool) == Some(true)
        && report.get("supervisor_removed").and_then(Value::as_bool) == Some(true)
        && report
            .get("coordination_state_removed")
            .and_then(Value::as_bool)
            == Some(true)
        && report.get("binary_retained").and_then(Value::as_bool) == Some(true);
    let mut document = outcome(
        context,
        Outcome {
            state: if complete {
                OutcomeState::Success
            } else {
                OutcomeState::Warning
            },
            title: if complete {
                "Daemon prepared for uninstall"
            } else {
                "Daemon uninstall preparation is incomplete"
            },
            detail: Some(if complete {
                "All registered daemon roots are disabled and stopped, and the singleton supervisor registration was removed."
            } else {
                "One or more lifecycle resources still need cleanup."
            }),
        },
    );
    document.push_blank();
    document.append(fields(
        context,
        &[Field::new(
            "Caveat",
            "The ctx binary and history data have not been removed.",
        )],
    ));
    if !complete {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Retry the idempotent cleanup before removing ctx.",
            },
            Some(Action {
                command: "ctx daemon disable --prepare-uninstall",
            }),
        ));
    }
    document
}

fn service_state(
    enabled: bool,
    running: bool,
    recoverable: bool,
    status: &str,
) -> (&'static str, Token) {
    if status == "completed" {
        ("completed", Token::Success)
    } else if !enabled || status == "disabled" {
        ("disabled", Token::Text)
    } else if recoverable {
        ("failed (recoverable)", Token::Error)
    } else if running {
        ("running", Token::Success)
    } else if status == "unknown" {
        ("not started", Token::Warning)
    } else {
        ("failed", Token::Error)
    }
}

fn history_state(
    history: Option<&Value>,
    source_refresh: Option<&Value>,
    enabled: bool,
    rejected_records: u64,
) -> (&'static str, Token) {
    if !enabled {
        return ("disabled", Token::Text);
    }
    if job_failed(source_refresh) || job_failed(history) {
        return ("failed", Token::Error);
    }
    if job_catching_up(source_refresh) || job_catching_up(history) {
        return ("catching up", Token::Warning);
    }
    if rejected_records > 0 {
        return ("ready with rejections", Token::Warning);
    }
    let status = preferred_job_status(source_refresh, history);
    match status {
        "completed" | "ready" | "published" | "idle" | "succeeded" => ("ready", Token::Success),
        "disabled" => ("disabled", Token::Text),
        "skipped"
            if source_refresh
                .and_then(|job| job.get("reason"))
                .and_then(Value::as_str)
                == Some("retry_backoff") =>
        {
            ("retrying", Token::Warning)
        }
        "unavailable" => ("unavailable", Token::Error),
        _ => ("unknown", Token::Text),
    }
}

fn semantic_state(
    semantic: Option<&Value>,
    runtime_active: bool,
    fallback: Option<&str>,
) -> (&'static str, Token) {
    if job_failed(semantic) {
        return ("failed", Token::Error);
    }
    if fallback.is_some() {
        return ("ready with fallback", Token::Warning);
    }
    let status = job_status(semantic);
    if runtime_active {
        return ("active", Token::Success);
    }
    match status {
        "completed" | "ready" | "published" | "succeeded" => ("ready", Token::Success),
        "pending" | "running" | "queued" | "accepted" => ("starting", Token::Warning),
        "disabled" => ("disabled", Token::Text),
        "unavailable" => ("unavailable", Token::Error),
        _ => ("unknown", Token::Text),
    }
}

fn state_field<'a>(label: &'a str, value: &'a str, token: Token) -> Field<'a> {
    Field::new(label, value).with_value_token(token)
}

fn append_details<'a>(table: &mut Vec<Field<'a>>, details: &'a [(&'a str, String)]) {
    table.extend(
        details
            .iter()
            .map(|(label, value)| Field::new(label, value)),
    );
}

fn push_unique_detail(details: &mut Vec<(&'static str, String)>, label: &'static str, value: &str) {
    if !details
        .iter()
        .any(|(_, existing)| existing.as_str() == value)
    {
        details.push((label, value.to_owned()));
    }
}

fn recovery_action(
    presentation: DaemonPresentation,
    recoverable: bool,
    history_failed: bool,
    rejected_records: u64,
    semantic_failed: bool,
    history_catching_up: bool,
    service_issue: bool,
) -> Option<(&'static str, &'static str)> {
    if presentation == DaemonPresentation::Disabled {
        return Some((
            "Enable the daemon to resume automatic history refresh.",
            "ctx daemon enable",
        ));
    }
    if presentation == DaemonPresentation::NotStarted {
        return Some(("Check daemon startup and service health.", "ctx doctor"));
    }
    if recoverable {
        return Some((
            "Restart the daemon and check its health.",
            "ctx daemon enable",
        ));
    }
    if history_failed || rejected_records > 0 {
        return Some((
            "Inspect source-level refresh failures.",
            "ctx import --all --no-daemon",
        ));
    }
    if semantic_failed || service_issue {
        return Some(("Inspect the affected service.", "ctx doctor"));
    }
    if presentation == DaemonPresentation::Failed {
        return Some(("Inspect the failed daemon service.", "ctx doctor"));
    }
    if history_catching_up {
        return Some(("Watch history refresh progress.", "ctx index watch"));
    }
    None
}

fn job_status(job: Option<&Value>) -> &str {
    job.and_then(|job| job.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

fn preferred_job_status<'a>(primary: Option<&'a Value>, fallback: Option<&'a Value>) -> &'a str {
    let primary_status = job_status(primary);
    if primary_status == "unknown" {
        job_status(fallback)
    } else {
        primary_status
    }
}

fn job_failed(job: Option<&Value>) -> bool {
    matches!(job_status(job), "failed" | "error") || job_error(job).is_some()
}

fn job_catching_up(job: Option<&Value>) -> bool {
    matches!(
        job_status(job),
        "pending" | "running" | "queued" | "accepted"
    ) || (job_status(job) == "skipped"
        && job
            .and_then(|job| job.get("reason"))
            .and_then(Value::as_str)
            == Some("retry_backoff"))
}

fn job_error(job: Option<&Value>) -> Option<&str> {
    job.and_then(|job| job.get("last_error"))
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty())
}

fn rejected_record_count(history: Option<&Value>) -> u64 {
    history
        .and_then(|job| {
            job.get("rejection_diagnostics")
                .and_then(|diagnostics| diagnostics.get("rejected_records"))
                .or_else(|| {
                    job.get("totals")
                        .and_then(|totals| totals.get("rejected_records"))
                })
        })
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn semantic_fallback(semantic: Option<&Value>) -> Option<&str> {
    semantic
        .and_then(|job| job.get("embedding_runtime"))
        .and_then(|runtime| runtime.get("acquisition_fallback"))
        .and_then(Value::as_str)
        .filter(|fallback| !fallback.is_empty())
}

fn config_reload_issue(daemon: &Value) -> bool {
    let reload = daemon.get("config_reload");
    let status = reload
        .and_then(|reload| reload.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    matches!(status, "failed" | "activation_failed")
        || (status == "pending"
            && reload
                .and_then(|reload| reload.get("out_of_sync"))
                .and_then(Value::as_bool)
                == Some(true))
}

fn config_state(status: &str) -> &'static str {
    match status {
        "failed" | "activation_failed" => "failed",
        "pending" => "pending",
        _ => "needs attention",
    }
}

fn supervisor_persistence_issue(supervisor: &Value) -> Option<String> {
    let status = supervisor
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if supervisor
        .pointer("/environment_snapshot/restart_required")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Some(
            "native supervisor environment changed; run `ctx daemon enable` to install the current nonsecret snapshot and restart"
                .to_owned(),
        );
    }
    if status == "installed"
        && supervisor
            .get("registration_verified")
            .and_then(Value::as_bool)
            == Some(true)
        && supervisor
            .get("live_owner_verified")
            .and_then(Value::as_bool)
            == Some(true)
    {
        return None;
    }
    if matches!(status, "disabled" | "unknown") {
        return None;
    }
    for key in ["limitation", "revalidation_error", "last_error"] {
        if let Some(issue) = supervisor
            .get(key)
            .and_then(Value::as_str)
            .filter(|issue| !issue.is_empty())
        {
            return Some(issue.to_owned());
        }
    }
    Some(format!("supervisor status is {}", humanize_code(status)))
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
