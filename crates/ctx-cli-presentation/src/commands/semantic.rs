use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use serde_json::Value;

use crate::output::JsonOutputFormat;
use crate::ui::{
    fields, outcome, section, Document, Field, Line, Outcome, OutcomeState, Span, Token,
};

use super::doctor_presentation::bounded_terminal_detail;

#[derive(Debug, Args)]
pub struct SemanticArgs {
    #[command(subcommand)]
    pub command: SemanticCommand,
}

impl SemanticArgs {
    pub fn json_output(&self) -> bool {
        match &self.command {
            SemanticCommand::Enable(args) => args.format.is_json(),
            SemanticCommand::Status(args) | SemanticCommand::Disable(args) => args.format.is_json(),
            SemanticCommand::Runtime(args) => match &args.command {
                SemanticRuntimeCommand::Install(args) => args.format.is_json(),
                SemanticRuntimeCommand::Status(args) => args.format.is_json(),
            },
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum SemanticCommand {
    #[command(about = "Enable semantic search and start model indexing")]
    Enable(SemanticEnableArgs),
    #[command(about = "Show semantic search readiness and executor selection")]
    Status(SemanticFormatArgs),
    #[command(about = "Disable semantic search and retain local assets")]
    Disable(SemanticFormatArgs),
    #[command(about = "Manage the local ONNX Runtime that runs the built-in executor")]
    Runtime(SemanticRuntimeArgs),
}

#[derive(Debug, Args)]
pub struct SemanticEnableArgs {
    #[arg(
        long,
        help = "Wait until semantic search is ready for the current index"
    )]
    pub wait: bool,
    #[arg(
        long,
        value_name = "builtin|URL",
        help = "Select builtin or discover and accept the semantic space served by URL"
    )]
    pub executor: Option<String>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub struct SemanticFormatArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub struct SemanticRuntimeArgs {
    #[command(subcommand)]
    pub command: SemanticRuntimeCommand,
}

#[derive(Debug, Subcommand)]
pub enum SemanticRuntimeCommand {
    #[command(
        about = "Install a digest-pinned ONNX Runtime archive; the archive's own contents decide which runtime is installed"
    )]
    Install(SemanticRuntimeInstallArgs),
    #[command(
        about = "Show which ONNX Runtimes this build can install locally and which are installed"
    )]
    Status(SemanticRuntimeStatusArgs),
}

#[derive(Debug, Args)]
pub struct SemanticRuntimeInstallArgs {
    #[arg(
        long,
        value_name = "PATH",
        help = "ONNX Runtime archive to verify and install"
    )]
    pub archive: PathBuf,
    #[arg(
        long,
        value_name = "HEX",
        help = "Expected archive SHA-256; defaults to the adjacent <archive>.sha256 file"
    )]
    pub sha256: Option<String>,
    // Every sidecar names its own files, so the archive already says which
    // runtime it is. Defaulting this to a backend made handing ctx the other
    // archive fail as a file-level contract error instead of installing it.
    #[arg(
        long,
        value_enum,
        help = "Assert the runtime this archive must contain; omitted, the backend is read from the archive itself"
    )]
    pub backend: Option<SemanticRuntimeBackendArg>,
    #[arg(long, help = "Replace an already installed runtime of the same version")]
    pub force: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub struct SemanticRuntimeStatusArgs {
    #[arg(
        long,
        value_enum,
        help = "Report one backend; omitted, every backend this build can install locally is reported"
    )]
    pub backend: Option<SemanticRuntimeBackendArg>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SemanticRuntimeBackendArg {
    Cpu,
    Cuda,
    // The published platform directory and manifest spell this backend without
    // a separator, so the CLI value must not be kebab-cased into `windows-ml`.
    #[value(name = "windowsml")]
    WindowsMl,
}

pub fn render_semantic_status(context: &crate::ui::RenderContext, report: &Value) -> Document {
    let enabled = bool_at(report, "/enabled");
    let status = str_at(report, "/status", "unavailable");
    let indexing_mode = str_at(report, "/indexing/mode", "unknown");
    let (state, title, detail) = if !enabled {
        if status == "disabling" {
            (
                OutcomeState::Neutral,
                "Semantic search is disabling",
                Some("The opt-out is saved; background semantic serving is stopping."),
            )
        } else {
            (
                OutcomeState::Neutral,
                "Semantic search is disabled",
                Some("No model acquisition, embedding requests, or semantic indexing will run."),
            )
        }
    } else {
        match status {
            "ready" => (
                OutcomeState::Success,
                "Semantic search is ready",
                Some("Hybrid and semantic search can use the current index."),
            ),
            "pending" if indexing_mode == "manual" => (
                OutcomeState::Neutral,
                "Semantic search is enabled",
                Some("Automatic model acquisition and indexing are paused in manual mode."),
            ),
            "pending" => (
                OutcomeState::Neutral,
                "Semantic search is preparing",
                Some("Model acquisition or semantic indexing is still in progress."),
            ),
            "failed" | "unavailable" => (
                OutcomeState::Warning,
                "Semantic search needs attention",
                Some("Inspect the reported reason or run ctx doctor."),
            ),
            _ => (
                OutcomeState::Neutral,
                "Semantic search is enabled",
                Some("Background maintenance has not reported a ready index yet."),
            ),
        }
    };
    // Surface the persisted background failure text so a missing ONNX Runtime,
    // model, or provisioning error is visible instead of only a state word.
    let background_error = semantic_background_error(report);
    let detail = match background_error.as_deref() {
        Some(error) if matches!(status, "failed" | "unavailable") => Some(error),
        _ => detail,
    };
    let mut document = outcome(
        context,
        Outcome {
            state,
            title,
            detail,
        },
    );
    let daemon_status = str_at(report, "/daemon/status", "unavailable");
    let reason = report
        .pointer("/reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty());
    let builtin_throttling = builtin_throttling_display(report);
    let mut values = vec![
        Field::new("Status", status),
        Field::new("Indexing", indexing_mode),
        Field::new("Background", daemon_status),
        Field::new("Executor", str_at(report, "/executor/kind", "builtin")),
        Field::new("Built-in throttling", &builtin_throttling),
    ];
    if let Some(endpoint) = report
        .pointer("/executor/endpoint")
        .and_then(Value::as_str)
        .filter(|endpoint| !endpoint.is_empty())
    {
        values.push(Field::new("Endpoint", endpoint));
    }
    if let Some(space_id) = report
        .pointer("/executor/space_id")
        .and_then(Value::as_str)
        .filter(|space_id| !space_id.is_empty())
    {
        values.push(Field::new("Space", space_id));
    }
    let dimensions = report
        .pointer("/executor/dimensions")
        .and_then(Value::as_u64)
        .map(|dimensions| dimensions.to_string());
    if let Some(dimensions) = dimensions.as_deref() {
        values.push(Field::new("Dimensions", dimensions));
    }
    if report
        .pointer("/executor/endpoint")
        .and_then(Value::as_str)
        .is_some()
    {
        let content = if str_at(report, "/executor/scope", "remote") == "loopback" {
            if enabled {
                "is sent to the loopback executor; trust it not to retain or forward"
            } else {
                "will be sent to the loopback executor when semantic search is enabled"
            }
        } else if enabled {
            "can be sent to the configured executor when semantic work runs"
        } else {
            "remote transfer is configured for when semantic search is enabled"
        };
        values.push(Field::new("Content", content));
    }
    if let Some(reason) = reason {
        values.push(Field::new("Reason", reason));
    }
    document.push_blank();
    document.append(section("Semantic", fields(context, &values)));

    let next = if !enabled {
        Some("ctx semantic enable")
    } else if status == "ready" {
        Some("ctx search \"your query\"")
    } else if indexing_mode == "manual" {
        Some("ctx index mode auto")
    } else if matches!(status, "failed" | "unavailable") {
        Some("ctx doctor")
    } else {
        Some("ctx semantic status")
    };
    if let Some(next) = next {
        document.push_blank();
        document.append(next_commands(&[next]));
    }
    document
}

pub fn render_semantic_disabled(context: &crate::ui::RenderContext, report: &Value) -> Document {
    let status = str_at(report, "/status", "disabled");
    let pending = status == "disabling";
    let mut document = outcome(
        context,
        Outcome {
            state: if pending {
                OutcomeState::Neutral
            } else {
                OutcomeState::Success
            },
            title: if pending {
                "Semantic search is disabling"
            } else {
                "Semantic search disabled"
            },
            detail: Some(if pending {
                "The opt-out is saved; background serving is stopping. Downloaded assets are retained."
            } else {
                "Downloaded model, runtime, and semantic index data were retained."
            }),
        },
    );
    document.push_blank();
    document.append(section(
        "Semantic",
        fields(context, &[Field::new("Status", status)]),
    ));
    document
}

/// Human rendering for `ctx semantic runtime install`. A provisioned runtime is
/// inert until semantic search is enabled, so this never claims the runtime is
/// already executing anything.
pub fn render_semantic_runtime_install(
    context: &crate::ui::RenderContext,
    report: &Value,
) -> Document {
    let title = format!(
        "{} ONNX Runtime installed",
        backend_display_name(report_backend(report))
    );
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Success,
            title: &title,
            detail: Some(
                "The archive digest and every installed file were verified against the pinned runtime contract.",
            ),
        },
    );
    document.push_blank();
    document.append(runtime_section(
        context,
        "Runtime",
        &installed_runtime_rows(report),
    ));
    document.push_blank();
    document.append(next_commands(&["ctx semantic enable"]));
    document
}

/// Human rendering for `ctx semantic runtime status`, for one backend or for
/// every backend this build can install locally. Nothing installed is not a
/// failure: the CPU runtime also loads from an unpacked sidecar layout or an
/// explicit environment override, so this reports the missing provisioning step
/// instead of an error.
pub fn render_semantic_runtime_status(
    context: &crate::ui::RenderContext,
    report: &Value,
) -> Document {
    match report.pointer("/runtimes").and_then(Value::as_array) {
        Some(runtimes) => render_every_runtime_status(context, report, runtimes),
        None => render_single_runtime_status(context, report),
    }
}

fn render_every_runtime_status(
    context: &crate::ui::RenderContext,
    report: &Value,
    runtimes: &[Value],
) -> Document {
    let runtime_root = str_at(report, "/runtime_root", "unknown");
    if runtimes.is_empty() {
        let mut document = outcome(
            context,
            Outcome {
                state: OutcomeState::Neutral,
                title: "This build installs no ONNX Runtime locally",
                detail: Some(
                    "This build's runtime sidecars are not installable from a local archive, so the hosted installer provisions them; --backend still reports one backend directly.",
                ),
            },
        );
        document.push_blank();
        document.append(runtime_section(
            context,
            "Runtime",
            &[("Root", runtime_root.to_owned())],
        ));
        return document;
    }

    let installed = runtimes
        .iter()
        .filter(|entry| bool_at(entry, "/installed"))
        .count();
    let title = format!(
        "{installed} of {} local ONNX Runtimes installed",
        runtimes.len()
    );
    // A host with a GPU and no accelerator runtime silently ran on the CPU, so
    // the detected accelerator leads the summary whenever there is one to act
    // on. Absent detection, nothing about GPUs is claimed.
    let accelerator = detected_installable_accelerator(report, runtimes);
    let accelerator_detail = accelerator.map(|(backend, installed)| {
        let display = backend_display_name(backend);
        if installed {
            format!(
                "This machine has a {display} accelerator and its runtime is installed, so GPU execution is available."
            )
        } else {
            format!(
                "This machine has a {display} accelerator, but its runtime is not installed; install the {display} archive for GPU execution."
            )
        }
    });
    let mut document = outcome(
        context,
        Outcome {
            state: if installed == 0 {
                OutcomeState::Neutral
            } else {
                OutcomeState::Success
            },
            title: &title,
            detail: Some(accelerator_detail.as_deref().unwrap_or(
                "Semantic search runs on the CPU runtime; an accelerator runtime is an opt-in addition for GPU execution.",
            )),
        },
    );
    document.push_blank();
    let backends = runtimes
        .iter()
        .map(|entry| report_backend(entry))
        .collect::<Vec<_>>()
        .join(", ");
    let mut runtime_rows = vec![("Root", runtime_root.to_owned()), ("Backends", backends)];
    if let Some((backend, installed)) = accelerator {
        runtime_rows.push((
            "Accelerator",
            format!(
                "{backend} detected ({})",
                if installed {
                    "runtime installed"
                } else {
                    "runtime not installed"
                }
            ),
        ));
    }
    document.append(runtime_section(context, "Runtime", &runtime_rows));
    for entry in runtimes {
        document.push_blank();
        document.append(runtime_section(
            context,
            backend_display_name(report_backend(entry)),
            &runtime_status_rows(entry),
        ));
    }
    document.push_blank();
    let mut commands: Vec<String> = runtimes
        .iter()
        .filter(|entry| !bool_at(entry, "/installed"))
        .map(|entry| install_command(report_backend(entry)))
        .collect();
    if installed > 0 {
        commands.push("ctx semantic enable".to_owned());
    }
    let commands: Vec<&str> = commands.iter().map(|command| command.as_str()).collect();
    document.append(next_commands(&commands));
    document
}

fn render_single_runtime_status(
    context: &crate::ui::RenderContext,
    report: &Value,
) -> Document {
    let backend = report_backend(report);
    let display = backend_display_name(backend);
    let runtime_root = str_at(report, "/runtime_root", "unknown");
    let host_accelerator = detected_accelerator(report) == Some(backend);
    if !bool_at(report, "/installed") {
        // A backend this build cannot provision locally has no install command
        // to offer, so it must not be handed one that is guaranteed to fail.
        let locally_installable = report
            .pointer("/locally_installable")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let title = format!("No {display} ONNX Runtime is installed");
        let detail = if !locally_installable {
            format!(
                "This build cannot provision the {display} runtime from a local archive; the hosted installer provisions it."
            )
        } else if host_accelerator {
            format!(
                "This machine has a {display} accelerator, but its runtime is not installed; install the {display} archive for GPU execution."
            )
        } else {
            missing_runtime_detail(backend).to_owned()
        };
        let mut document = outcome(
            context,
            Outcome {
                state: OutcomeState::Neutral,
                title: &title,
                detail: Some(&detail),
            },
        );
        document.push_blank();
        document.append(runtime_section(
            context,
            "Runtime",
            &[
                ("Backend", backend.to_owned()),
                ("Installed", "no".to_owned()),
                ("Root", runtime_root.to_owned()),
            ],
        ));
        if locally_installable {
            document.push_blank();
            let command = install_command(backend);
            document.append(next_commands(&[command.as_str()]));
        }
        return document;
    }
    let title = format!("{display} ONNX Runtime is installed");
    let detail = if host_accelerator {
        format!(
            "This machine has a {display} accelerator and this runtime is installed, so GPU execution is available."
        )
    } else {
        "Semantic search can use this runtime while it is enabled.".to_owned()
    };
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Success,
            title: &title,
            detail: Some(&detail),
        },
    );
    document.push_blank();
    document.append(runtime_section(
        context,
        "Runtime",
        &installed_runtime_rows(report),
    ));
    document.push_blank();
    document.append(next_commands(&["ctx semantic enable"]));
    document
}

/// The accelerator this host could execute on, as the report observed it.
fn detected_accelerator(report: &Value) -> Option<&str> {
    report
        .pointer("/detected_accelerator")
        .and_then(Value::as_str)
}

/// The detected accelerator paired with whether its runtime is installed, but
/// only when this build can actually install it: a hosted-installer-only
/// accelerator has no local step to offer, so nagging about it would be noise.
fn detected_installable_accelerator<'a>(
    report: &'a Value,
    runtimes: &'a [Value],
) -> Option<(&'a str, bool)> {
    let detected = detected_accelerator(report)?;
    runtimes
        .iter()
        .find(|entry| report_backend(entry) == detected)
        .map(|entry| (detected, bool_at(entry, "/installed")))
}

/// Why a backend is absent. The CPU runtime has non-manifest load routes, so its
/// absence here never claims semantic search cannot run.
fn missing_runtime_detail(backend: &str) -> &'static str {
    if backend == "cpu" {
        "No digest-verified CPU runtime is provisioned under this runtime root; an unpacked sidecar layout or CTX_ONNXRUNTIME_DYLIB still loads one."
    } else {
        "GPU execution needs a provisioned accelerator runtime; semantic search runs on the CPU runtime until one is installed."
    }
}

/// Human label for a backend. The canonical `--backend` value stays visible as
/// a field so the reported backend is also the token to retype.
fn backend_display_name(backend: &str) -> &str {
    match backend {
        "cpu" => "CPU",
        "cuda" => "CUDA",
        "windowsml" => "Windows ML",
        other => other,
    }
}

/// The command that provisions one backend. It names the archive rather than a
/// `--backend` selector, because the archive is what decides which runtime is
/// installed.
fn install_command(backend: &str) -> String {
    format!("ctx semantic runtime install --archive <{backend}-archive>")
}

fn report_backend(report: &Value) -> &str {
    str_at(
        report,
        "/backend",
        str_at(report, "/runtime/backend", "unknown"),
    )
}

fn runtime_section(
    context: &crate::ui::RenderContext,
    title: &str,
    rows: &[(&'static str, String)],
) -> Document {
    let values: Vec<Field<'_>> = rows
        .iter()
        .map(|(label, value)| Field::new(label, value))
        .collect();
    section(title, fields(context, &values))
}

/// Per-backend rows for `runtime status`. Every backend reports whether it is
/// installed; only an installed one carries the verified runtime facts.
fn runtime_status_rows(entry: &Value) -> Vec<(&'static str, String)> {
    let installed = bool_at(entry, "/installed");
    let mut rows = vec![
        ("Backend", report_backend(entry).to_owned()),
        (
            "Installed",
            if installed { "yes" } else { "no" }.to_owned(),
        ),
    ];
    if installed {
        rows.extend(installed_runtime_detail_rows(entry));
    }
    rows
}

/// Label/value rows for an installed runtime. Values are owned because the
/// report carries a file count and paths that render as text.
fn installed_runtime_rows(report: &Value) -> Vec<(&'static str, String)> {
    let mut rows = vec![("Backend", report_backend(report).to_owned())];
    rows.extend(installed_runtime_detail_rows(report));
    rows
}

fn installed_runtime_detail_rows(report: &Value) -> Vec<(&'static str, String)> {
    vec![
        (
            "Platform",
            str_at(report, "/runtime/platform", "unknown").to_owned(),
        ),
        (
            "Version",
            str_at(report, "/runtime/version", "unknown").to_owned(),
        ),
        (
            "Trust",
            format!(
                "{} ({})",
                str_at(report, "/runtime/metadata_trust", "unknown"),
                str_at(report, "/runtime/manager", "unknown"),
            ),
        ),
        (
            "Files",
            report
                .pointer("/runtime/files")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .to_string(),
        ),
        (
            "Directory",
            str_at(report, "/runtime/root", "unknown").to_owned(),
        ),
        (
            "Library",
            str_at(report, "/runtime/library", "unknown").to_owned(),
        ),
        (
            "Archive",
            format!(
                "sha256:{}",
                str_at(report, "/runtime/archive_sha256", "unknown")
            ),
        ),
        (
            "Identity",
            str_at(report, "/runtime/identity", "unknown").to_owned(),
        ),
    ]
}

fn next_commands(commands: &[&str]) -> Document {
    let mut body = Document::new();
    for command in commands {
        body.push_line(
            Line::new()
                .with(Span::text("  "))
                .with(Span::new(*command, Token::Command)),
        );
    }
    section("Next", body)
}

fn bool_at(report: &Value, pointer: &str) -> bool {
    report
        .pointer(pointer)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn builtin_throttling_display(report: &Value) -> String {
    let configured = report
        .pointer("/builtin_throttling/configured")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let configured = if configured { "enabled" } else { "disabled" };
    let effective = match report.pointer("/builtin_throttling/effective") {
        Some(Value::Bool(true)) => "enabled",
        Some(Value::Bool(false)) => "disabled",
        Some(Value::Null) => "not applicable",
        _ if report
            .pointer("/builtin_throttling/reason")
            .and_then(Value::as_str)
            == Some("external_executor") =>
        {
            "not applicable"
        }
        _ if report.pointer("/executor/kind").and_then(Value::as_str) == Some("http") => {
            "not applicable"
        }
        _ => configured,
    };
    format!("{effective} (configured: {configured})")
}

fn str_at<'a>(report: &'a Value, pointer: &str, fallback: &'a str) -> &'a str {
    report
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
}

/// Persisted error text from the last background semantic iteration, bounded
/// for terminal rendering. Successful runs and resource deferrals omit it.
fn semantic_background_error(report: &Value) -> Option<String> {
    report
        .pointer("/daemon/semantic_index/last_error")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|error| !error.is_empty())
        .map(bounded_terminal_detail)
}

#[cfg(test)]
mod tests {
    use ctx_terminal::{RenderContext, StreamKind, TestContext};
    use serde_json::json;

    use super::*;

    fn context() -> RenderContext {
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout))
    }

    #[derive(clap::Parser)]
    struct SemanticProbe {
        #[command(subcommand)]
        command: SemanticCommand,
    }

    fn parse_runtime_command(arguments: &[&str]) -> SemanticRuntimeCommand {
        let mut argv = vec!["probe", "runtime"];
        argv.extend_from_slice(arguments);
        let probe: SemanticProbe = clap::Parser::parse_from(argv);
        match probe.command {
            SemanticCommand::Runtime(args) => args.command,
            _ => unreachable!("the runtime namespace was requested"),
        }
    }

    fn runtime_install_args(arguments: &[&str]) -> SemanticRuntimeInstallArgs {
        let mut argv = vec!["install"];
        argv.extend_from_slice(arguments);
        match parse_runtime_command(&argv) {
            SemanticRuntimeCommand::Install(args) => args,
            SemanticRuntimeCommand::Status(_) => unreachable!("install was requested"),
        }
    }

    fn runtime_status_backend(arguments: &[&str]) -> Option<SemanticRuntimeBackendArg> {
        let mut argv = vec!["status"];
        argv.extend_from_slice(arguments);
        match parse_runtime_command(&argv) {
            SemanticRuntimeCommand::Status(args) => args.backend,
            SemanticRuntimeCommand::Install(_) => unreachable!("status was requested"),
        }
    }

    #[test]
    fn runtime_install_asserts_a_backend_only_when_one_is_given() {
        // No default: the archive decides, so an operator who downloaded the
        // CUDA sidecar must not have to know to override a cpu selector.
        assert_eq!(
            runtime_install_args(&["--archive", "runtime.tar.zst"]).backend,
            None
        );
        assert_eq!(
            runtime_install_args(&["--archive", "runtime.tar.zst", "--backend", "cuda"]).backend,
            Some(SemanticRuntimeBackendArg::Cuda)
        );
        // The published platform directory spells this backend without a
        // separator, so `windows-ml` must not be the accepted spelling.
        assert_eq!(
            runtime_install_args(&["--archive", "runtime.tar.zst", "--backend", "windowsml"])
                .backend,
            Some(SemanticRuntimeBackendArg::WindowsMl)
        );
        let rejected: Result<SemanticProbe, clap::Error> = clap::Parser::try_parse_from([
            "probe",
            "runtime",
            "install",
            "--archive",
            "runtime.tar.zst",
            "--backend",
            "windows-ml",
        ]);
        assert!(rejected.is_err());
    }

    #[test]
    fn runtime_status_reports_every_backend_until_one_is_requested() {
        assert_eq!(runtime_status_backend(&[]), None);
        assert_eq!(
            runtime_status_backend(&["--backend", "cpu"]),
            Some(SemanticRuntimeBackendArg::Cpu)
        );
        assert_eq!(
            runtime_status_backend(&["--backend", "cuda"]),
            Some(SemanticRuntimeBackendArg::Cuda)
        );
    }

    #[test]
    fn disabled_status_points_to_the_namespace() {
        let rendered = render_semantic_status(
            &context(),
            &json!({
                "enabled": false,
                "status": "disabled",
                "indexing": {"mode": "auto"},
                "daemon": {"status": "running"},
            }),
        )
        .render_plain();

        assert!(
            rendered.contains("Semantic search is disabled"),
            "{rendered}"
        );
        assert!(rendered.contains("ctx semantic enable"), "{rendered}");
    }

    #[test]
    fn failed_status_shows_the_background_runtime_error() {
        let rendered = render_semantic_status(
            &context(),
            &json!({
                "enabled": true,
                "status": "failed",
                "reason": "model_load_failed",
                "indexing": {"mode": "auto"},
                "daemon": {
                    "status": "running",
                    "semantic_index": {
                        "status": "pending",
                        "last_run_status": "skipped",
                        "last_run_reason": "model_load_failed",
                        "last_error": "no ONNX Runtime dynamic library candidates were found for linux-x64; set an absolute path with CTX_ONNXRUNTIME_DYLIB",
                    },
                },
            }),
        )
        .render_plain();

        assert!(
            rendered.contains("Semantic search needs attention"),
            "{rendered}"
        );
        assert!(
            rendered.contains("no ONNX Runtime dynamic library candidates were found"),
            "{rendered}"
        );
        assert!(rendered.contains("model_load_failed"), "{rendered}");
    }

    #[test]
    fn manual_pending_status_explains_the_required_lifecycle() {
        let rendered = render_semantic_status(
            &context(),
            &json!({
                "enabled": true,
                "status": "pending",
                "reason": "flat_f32_projection_missing",
                "indexing": {"mode": "manual"},
                "daemon": {"status": "disabled"},
                "executor": {
                    "kind": "http",
                    "endpoint": "https://embed.example.test",
                    "space_id": "acme/multilingual-v2",
                    "dimensions": 768
                },
                "local_only": false,
            }),
        )
        .render_plain();

        assert!(
            rendered.contains("Semantic search is enabled"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Automatic model acquisition and indexing are paused"),
            "{rendered}"
        );
        assert!(rendered.contains("ctx index mode auto"), "{rendered}");
        assert!(
            rendered.contains(
                "Content              can be sent to the configured executor when semantic work runs"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn external_executor_status_makes_the_content_boundary_visible() {
        let rendered = render_semantic_status(
            &context(),
            &json!({
                "enabled": true,
                "status": "ready",
                "indexing": {"mode": "auto"},
                "daemon": {"status": "running"},
                "executor": {
                    "kind": "http",
                    "endpoint": "https://embed.example.test",
                    "space_id": "acme/multilingual-v2",
                    "dimensions": 768
                },
                "builtin_throttling": {
                    "configured": true,
                    "effective": null,
                    "config_source": "default",
                    "reason": "external_executor"
                },
                "local_only": false,
            }),
        )
        .render_plain();

        assert!(rendered.contains("Executor             http"), "{rendered}");
        assert!(
            rendered.contains("https://embed.example.test"),
            "{rendered}"
        );
        assert!(rendered.contains("acme/multilingual-v2"), "{rendered}");
        assert!(rendered.contains("Dimensions"), "{rendered}");
        assert!(rendered.contains("768"), "{rendered}");
        assert!(
            rendered.contains("not applicable (configured: enabled)"),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "Content              can be sent to the configured executor when semantic work runs"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn builtin_throttling_human_status_distinguishes_enabled_and_disabled() {
        for (configured, expected) in [
            (true, "enabled (configured: enabled)"),
            (false, "disabled (configured: disabled)"),
        ] {
            let rendered = render_semantic_status(
                &context(),
                &json!({
                    "enabled": true,
                    "status": "pending",
                    "indexing": {"mode": "manual"},
                    "daemon": {"status": "disabled"},
                    "executor": {"kind": "builtin"},
                    "builtin_throttling": {
                        "configured": configured,
                        "effective": configured,
                        "config_source": if configured { "default" } else { "config" },
                    },
                }),
            )
            .render_plain();

            assert!(rendered.contains(expected), "{rendered}");
        }
    }

    #[test]
    fn disabled_external_executor_reports_transfer_as_configured_not_active() {
        let rendered = render_semantic_status(
            &context(),
            &json!({
                "enabled": false,
                "status": "disabled",
                "indexing": {"mode": "auto"},
                "daemon": {"status": "running"},
                "executor": {
                    "kind": "http",
                    "endpoint": "https://embed.example.test"
                },
                "local_only": false,
            }),
        )
        .render_plain();

        assert!(
            rendered.contains(
                "Content              remote transfer is configured for when semantic search is enabled"
            ),
            "{rendered}"
        );
        assert!(
            !rendered.contains("Content              sent to the configured executor"),
            "{rendered}"
        );
    }

    #[test]
    fn loopback_executor_warns_that_the_local_process_can_retain_or_forward_content() {
        let rendered = render_semantic_status(
            &context(),
            &json!({
                "enabled": true,
                "status": "ready",
                "indexing": {"mode": "manual"},
                "daemon": {"status": "disabled"},
                "executor": {
                    "kind": "http",
                    "endpoint": "http://127.0.0.1:8080/",
                    "scope": "loopback",
                    "space_id": "local/model-v1",
                    "dimensions": 384
                },
                "local_only": true,
            }),
        )
        .render_plain();

        assert!(
            rendered.contains(
                "Content              is sent to the loopback executor; trust it not to retain or forward"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn pending_disable_reports_saved_policy_and_background_shutdown() {
        let rendered = render_semantic_disabled(
            &context(),
            &json!({
                "enabled": false,
                "status": "disabling",
            }),
        )
        .render_plain();

        assert!(
            rendered.contains("Semantic search is disabling"),
            "{rendered}"
        );
        assert!(rendered.contains("opt-out is saved"), "{rendered}");
        assert!(rendered.contains("Status  disabling"), "{rendered}");
    }

    fn verified_runtime(backend: &str, platform: &str, files: u64) -> Value {
        json!({
            "backend": backend,
            "platform": platform,
            "version": "1.27.0",
            "root": format!("/data/runtime/onnxruntime/1.27.0/{platform}"),
            "library": format!("/data/runtime/onnxruntime/1.27.0/{platform}/lib/libonnxruntime.so"),
            "archive_sha256": "a".repeat(64),
            "manager": "ctx-local-operator",
            "metadata_trust": "operator-pinned-digest",
            "files": files,
            "identity": format!("onnxruntime|platform={platform}"),
        })
    }

    fn single_backend_report(operation: &str, backend: &str, runtime: Option<Value>) -> Value {
        let mut report = json!({
            "schema_version": 1,
            "operation": operation,
            "backend": backend,
            "runtime_root": "/data/runtime",
            "installed": runtime.is_some(),
            "locally_installable": true,
            "read_only": operation == "runtime_status",
        });
        if let Some(runtime) = runtime {
            report["runtime"] = runtime;
        }
        report
    }

    #[test]
    fn a_backend_this_build_cannot_install_locally_offers_no_install_command() {
        let mut report = single_backend_report("runtime_status", "windowsml", None);
        report["locally_installable"] = json!(false);

        let rendered = render_semantic_runtime_status(&context(), &report).render_plain();

        assert!(
            rendered.contains("No Windows ML ONNX Runtime is installed"),
            "{rendered}"
        );
        assert!(
            rendered.contains("hosted installer provisions it"),
            "{rendered}"
        );
        assert!(rendered.contains("Installed  no"), "{rendered}");
        assert!(
            !rendered.contains("ctx semantic runtime install"),
            "an install that cannot succeed must not be offered: {rendered}"
        );
    }

    #[test]
    fn runtime_install_reports_the_backend_platform_trust_and_file_count() {
        let rendered = render_semantic_runtime_install(
            &context(),
            &single_backend_report(
                "runtime_install",
                "cpu",
                Some(verified_runtime("cpu", "linux-x64", 5)),
            ),
        )
        .render_plain();

        assert!(rendered.contains("CPU ONNX Runtime installed"), "{rendered}");
        assert!(rendered.contains("Backend    cpu"), "{rendered}");
        assert!(rendered.contains("Platform   linux-x64"), "{rendered}");
        assert!(rendered.contains("Version    1.27.0"), "{rendered}");
        assert!(
            rendered.contains("Trust      operator-pinned-digest (ctx-local-operator)"),
            "{rendered}"
        );
        assert!(rendered.contains("Files      5"), "{rendered}");
        assert!(
            rendered.contains("Directory  /data/runtime/onnxruntime/1.27.0/linux-x64"),
            "{rendered}"
        );
        assert!(rendered.contains("ctx semantic enable"), "{rendered}");
    }

    #[test]
    fn accelerator_install_names_the_accelerator_backend_and_platform() {
        let rendered = render_semantic_runtime_install(
            &context(),
            &single_backend_report(
                "runtime_install",
                "cuda",
                Some(verified_runtime("cuda", "linux-x64-cuda12", 18)),
            ),
        )
        .render_plain();

        assert!(
            rendered.contains("CUDA ONNX Runtime installed"),
            "{rendered}"
        );
        assert!(rendered.contains("Backend    cuda"), "{rendered}");
        assert!(
            rendered.contains("Platform   linux-x64-cuda12"),
            "{rendered}"
        );
        assert!(rendered.contains("Files      18"), "{rendered}");
    }

    #[test]
    fn absent_accelerator_status_points_at_that_backend_install_command() {
        let rendered = render_semantic_runtime_status(
            &context(),
            &single_backend_report("runtime_status", "cuda", None),
        )
        .render_plain();

        assert!(
            rendered.contains("No CUDA ONNX Runtime is installed"),
            "{rendered}"
        );
        assert!(
            rendered.contains("GPU execution needs a provisioned accelerator runtime"),
            "{rendered}"
        );
        assert!(rendered.contains("Backend    cuda"), "{rendered}");
        assert!(rendered.contains("Installed  no"), "{rendered}");
        assert!(rendered.contains("Root       /data/runtime"), "{rendered}");
        assert!(
            rendered.contains("ctx semantic runtime install --archive <cuda-archive>"),
            "{rendered}"
        );
    }

    #[test]
    fn absent_cpu_status_keeps_the_unmanaged_load_routes_visible() {
        let rendered = render_semantic_runtime_status(
            &context(),
            &single_backend_report("runtime_status", "cpu", None),
        )
        .render_plain();

        assert!(
            rendered.contains("No CPU ONNX Runtime is installed"),
            "{rendered}"
        );
        assert!(rendered.contains("CTX_ONNXRUNTIME_DYLIB"), "{rendered}");
        assert!(
            rendered.contains("ctx semantic runtime install --archive <cpu-archive>"),
            "{rendered}"
        );
    }

    #[test]
    fn installed_single_backend_status_reports_its_trust_tier_and_library() {
        let rendered = render_semantic_runtime_status(
            &context(),
            &single_backend_report(
                "runtime_status",
                "cuda",
                Some(verified_runtime("cuda", "linux-x64-cuda12", 18)),
            ),
        )
        .render_plain();

        assert!(
            rendered.contains("CUDA ONNX Runtime is installed"),
            "{rendered}"
        );
        assert!(rendered.contains("Backend    cuda"), "{rendered}");
        assert!(
            rendered.contains("Trust      operator-pinned-digest (ctx-local-operator)"),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "Library    /data/runtime/onnxruntime/1.27.0/linux-x64-cuda12/lib/libonnxruntime.so"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("Identity   onnxruntime|platform=linux-x64-cuda12"),
            "{rendered}"
        );
        assert!(rendered.contains("ctx semantic enable"), "{rendered}");
    }

    #[test]
    fn every_backend_status_separates_the_installed_one_from_the_missing_one() {
        let rendered = render_semantic_runtime_status(
            &context(),
            &json!({
                "schema_version": 1,
                "operation": "runtime_status",
                "runtime_root": "/data/runtime",
                "read_only": true,
                "runtimes": [
                    {
                        "backend": "cpu",
                        "installed": true,
                        "runtime": verified_runtime("cpu", "linux-x64", 5),
                    },
                    {"backend": "cuda", "installed": false},
                ],
            }),
        )
        .render_plain();

        assert!(
            rendered.contains("1 of 2 local ONNX Runtimes installed"),
            "{rendered}"
        );
        assert!(rendered.contains("Root      /data/runtime"), "{rendered}");
        assert!(rendered.contains("Backends  cpu, cuda"), "{rendered}");
        assert!(rendered.contains("CPU"), "{rendered}");
        assert!(rendered.contains("Platform   linux-x64"), "{rendered}");
        assert!(rendered.contains("Files      5"), "{rendered}");
        assert!(rendered.contains("Installed  yes"), "{rendered}");
        assert!(rendered.contains("CUDA"), "{rendered}");
        assert!(rendered.contains("Installed  no"), "{rendered}");
        assert!(
            rendered.contains("ctx semantic runtime install --archive <cuda-archive>"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("<cpu-archive>"),
            "an installed backend needs no install command: {rendered}"
        );
        assert!(rendered.contains("ctx semantic enable"), "{rendered}");
    }

    #[test]
    fn every_backend_status_with_nothing_installed_offers_both_install_commands() {
        let rendered = render_semantic_runtime_status(
            &context(),
            &json!({
                "schema_version": 1,
                "operation": "runtime_status",
                "runtime_root": "/data/runtime",
                "read_only": true,
                "runtimes": [
                    {"backend": "cpu", "installed": false},
                    {"backend": "cuda", "installed": false},
                ],
            }),
        )
        .render_plain();

        assert!(
            rendered.contains("0 of 2 local ONNX Runtimes installed"),
            "{rendered}"
        );
        assert!(rendered.contains("--archive <cpu-archive>"), "{rendered}");
        assert!(rendered.contains("--archive <cuda-archive>"), "{rendered}");
        assert!(
            !rendered.contains("ctx semantic enable"),
            "nothing is installed yet: {rendered}"
        );
    }

    #[test]
    fn no_locally_installable_backend_names_the_hosted_installer() {
        let rendered = render_semantic_runtime_status(
            &context(),
            &json!({
                "schema_version": 1,
                "operation": "runtime_status",
                "runtime_root": "C:\\data\\runtime",
                "read_only": true,
                "runtimes": [],
            }),
        )
        .render_plain();

        assert!(
            rendered.contains("This build installs no ONNX Runtime locally"),
            "{rendered}"
        );
        assert!(rendered.contains("hosted installer"), "{rendered}");
        assert!(rendered.contains("C:\\data\\runtime"), "{rendered}");
    }

    /// The report shape the multi-backend status renders, with the host's
    /// detected accelerator as the CLI publishes it.
    fn every_backend_report(detected: Option<&str>, cuda_installed: bool) -> Value {
        let mut cuda = json!({"backend": "cuda", "installed": cuda_installed});
        if cuda_installed {
            cuda["runtime"] = verified_runtime("cuda", "linux-x64-cuda12", 18);
        }
        json!({
            "schema_version": 1,
            "operation": "runtime_status",
            "runtime_root": "/data/runtime",
            "detected_accelerator": detected,
            "read_only": true,
            "runtimes": [
                {
                    "backend": "cpu",
                    "installed": true,
                    "runtime": verified_runtime("cpu", "linux-x64", 5),
                },
                cuda,
            ],
        })
    }

    #[test]
    fn a_detected_accelerator_without_its_runtime_names_the_install_step() {
        let rendered =
            render_semantic_runtime_status(&context(), &every_backend_report(Some("cuda"), false))
                .render_plain();

        assert!(
            rendered.contains(
                "This machine has a CUDA accelerator, but its runtime is not installed"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("Accelerator  cuda detected (runtime not installed)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("ctx semantic runtime install --archive <cuda-archive>"),
            "{rendered}"
        );
    }

    #[test]
    fn a_detected_accelerator_with_its_runtime_reports_gpu_execution_available() {
        let rendered =
            render_semantic_runtime_status(&context(), &every_backend_report(Some("cuda"), true))
                .render_plain();

        assert!(
            rendered.contains("GPU execution is available"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Accelerator  cuda detected (runtime installed)"),
            "{rendered}"
        );
    }

    #[test]
    fn a_host_with_no_accelerator_is_not_nagged_about_one() {
        let rendered =
            render_semantic_runtime_status(&context(), &every_backend_report(None, false))
                .render_plain();

        assert!(
            !rendered.contains("This machine has"),
            "an undetected accelerator must not be claimed: {rendered}"
        );
        assert!(
            !rendered.contains("Accelerator"),
            "an undetected accelerator must not be reported: {rendered}"
        );
        // The accelerator runtime is still listed as an available install.
        assert!(
            rendered.contains("ctx semantic runtime install --archive <cuda-archive>"),
            "{rendered}"
        );
    }

    #[test]
    fn a_hosted_installer_only_accelerator_is_not_offered_a_local_install() {
        let rendered = render_semantic_runtime_status(
            &context(),
            &json!({
                "schema_version": 1,
                "operation": "runtime_status",
                "runtime_root": "C:\\data\\runtime",
                "detected_accelerator": "windowsml",
                "read_only": true,
                "runtimes": [],
            }),
        )
        .render_plain();

        assert!(
            !rendered.contains("This machine has"),
            "a backend this build cannot install locally must not be nagged about: {rendered}"
        );
        assert!(
            !rendered.contains("ctx semantic runtime install"),
            "{rendered}"
        );
    }

    #[test]
    fn single_backend_status_carries_the_same_accelerator_guidance() {
        let mut absent = single_backend_report("runtime_status", "cuda", None);
        absent["detected_accelerator"] = json!("cuda");
        let rendered = render_semantic_runtime_status(&context(), &absent).render_plain();
        assert!(
            rendered.contains(
                "This machine has a CUDA accelerator, but its runtime is not installed"
            ),
            "{rendered}"
        );

        let mut installed = single_backend_report(
            "runtime_status",
            "cuda",
            Some(verified_runtime("cuda", "linux-x64-cuda12", 18)),
        );
        installed["detected_accelerator"] = json!("cuda");
        let rendered = render_semantic_runtime_status(&context(), &installed).render_plain();
        assert!(
            rendered.contains("GPU execution is available"),
            "{rendered}"
        );
    }
}
