use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;

use super::{
    super::{
        context::{DiscoveryContext, DiscoveryPlatform},
        reasons::path_presence_unknown_reason,
        selectors::{encoded_path_within_limit, source_path_kind, SourcePathKind},
        types::{DiscoveryIssueKind, DiscoveryReport, ProviderSourceKind, ProviderSourceSpec},
        StaticProviderProbeCatalog,
    },
    issue, path_presence, push_source_candidate, select_current_or_legacy,
    source_from_parts_with_data_root, PathPresence,
};

const MANUAL_PATH_REASON: &str =
    "the selected provider root cannot be reconstructed safely; use an exact --path";
const UNSUPPORTED_PLATFORM_REASON: &str =
    "no official automatic history default is established on this platform; use an exact --path";
const NO_DISK_REASON: &str =
    "the selected provider database is in memory and has no on-disk history";
const SYMLINK_REASON: &str =
    "the selected history path uses a symlink component; use a trusted real path with --path";
const PATH_LIMIT_REASON: &str =
    "the selected provider history path exceeds the discovery path limit; use an exact --path";
const CODEX_OVERRIDE_REASON: &str =
    "CODEX_HOME does not identify an existing directory; the default is suppressed and --path is required";

/// Winner-only custom-root policy for the scalar/fixed-root
/// providers owned by the simple resolver lane.
pub(super) fn resolve(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    match spec.provider {
        CaptureProvider::Codex => resolve_codex(probes, context, spec),
        CaptureProvider::GrokBuild => resolve_grok_build(probes, context, spec),
        CaptureProvider::DeepSeekHarness => resolve_deepseek_harness(probes, context, spec),
        CaptureProvider::Claude => resolve_claude(probes, context, spec),
        CaptureProvider::OpenCode => resolve_open_code(probes, context, spec),
        CaptureProvider::Kilo => resolve_kilo(probes, context, spec),
        CaptureProvider::MiMoCode => resolve_mimocode(probes, context, spec),
        CaptureProvider::Goose => resolve_goose(probes, context, spec),
        CaptureProvider::Continue => resolve_continue(probes, context, spec),
        CaptureProvider::Gemini => resolve_gemini(probes, context, spec),
        CaptureProvider::Tabnine => resolve_tabnine(probes, context, spec),
        CaptureProvider::Cursor => resolve_cursor(probes, context, spec),
        CaptureProvider::KimiCodeCli => resolve_kimi(probes, context, spec),
        CaptureProvider::Junie => resolve_junie(probes, context, spec),
        CaptureProvider::FactoryAiDroid => resolve_factory(probes, context, spec),
        CaptureProvider::ForgeCode => resolve_forgecode(probes, context, spec),
        CaptureProvider::Fx => resolve_fx(probes, context, spec),
        _ => DiscoveryReport::default(),
    }
}

fn resolve_deepseek_harness(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let root = match context.env("DSH_HOME") {
        Some(value)
            if !value.is_empty() && !value.to_str().is_some_and(|text| text.trim().is_empty()) =>
        {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return manual_report(spec, safe_issue_path(&path), MANUAL_PATH_REASON);
            }
            path
        }
        _ => match supported_default(context, spec) {
            Ok(()) => context.home().join(".dsh"),
            Err(report) => return report,
        },
    };
    one_source(
        probes,
        spec,
        root.join("sessions"),
        "deepseek_harness_session_jsonl_tree",
    )
}

fn resolve_grok_build(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let root = match context.env("GROK_HOME") {
        Some(value) => {
            let path = PathBuf::from(value);
            if value.is_empty() || !path.is_absolute() {
                return manual_report(spec, safe_issue_path(&path), MANUAL_PATH_REASON);
            }
            path
        }
        _ => match supported_default(context, spec) {
            Ok(()) => context.home().join(".grok"),
            Err(report) => return report,
        },
    };
    one_source(
        probes,
        spec,
        root.join("sessions"),
        "grok_build_session_updates_jsonl_tree",
    )
}

fn resolve_codex(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    resolve_inferred_codex(probes, context, spec)
}

fn resolve_inferred_codex(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let root = match context.env("CODEX_HOME").and_then(OsStr::to_str) {
        Some("") | None => match supported_default(context, spec) {
            Ok(()) => context.home().join(".codex"),
            Err(report) => return report,
        },
        Some(value) => {
            let Some(root) = resolve_from_cwd(context, PathBuf::from(value)) else {
                return manual_report(spec, None, MANUAL_PATH_REASON);
            };
            match source_path_kind(&root) {
                Ok(SourcePathKind::Directory) => {}
                Err(super::super::selectors::SourcePathError::Unsupported) => {
                    return manual_report(spec, safe_issue_path(&root), SYMLINK_REASON);
                }
                Ok(SourcePathKind::File) | Err(_) => {
                    return manual_report(spec, safe_issue_path(&root), CODEX_OVERRIDE_REASON);
                }
            }
            root
        }
    };

    let mut report = DiscoveryReport::default();
    add_codex_root_sources(probes, &mut report, spec, &root);
    report
}

pub fn released_provider_home(
    context: &DiscoveryContext,
    provider: CaptureProvider,
) -> Option<PathBuf> {
    match provider {
        CaptureProvider::Claude => match context.env("CLAUDE_CONFIG_DIR") {
            Some(value) if !value.is_empty() => {
                let path = PathBuf::from(value);
                path.is_absolute().then_some(path)
            }
            _ if context.home_directory_available()
                && context.platform() != DiscoveryPlatform::OtherUnix =>
            {
                Some(context.home().join(".claude"))
            }
            _ => None,
        },
        CaptureProvider::Codex => match context.env("CODEX_HOME").and_then(OsStr::to_str) {
            Some("") | None
                if context.home_directory_available()
                    && context.platform() != DiscoveryPlatform::OtherUnix =>
            {
                Some(context.home().join(".codex"))
            }
            Some(value) => resolve_from_cwd(context, PathBuf::from(value))
                .filter(|path| matches!(source_path_kind(path), Ok(SourcePathKind::Directory))),
            None => None,
        },
        _ => None,
    }
}

fn add_codex_root_sources(
    probes: &StaticProviderProbeCatalog,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    root: &Path,
) {
    for tree in [root.join("sessions"), root.join("archived_sessions")] {
        add_source(probes, report, spec, tree, "codex_session_jsonl_tree");
    }
    add_source(
        probes,
        report,
        spec,
        root.join("history.jsonl"),
        "codex_history_jsonl",
    );
}

fn resolve_claude(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    resolve_inferred_claude(probes, context, spec)
}

fn resolve_inferred_claude(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let root = match context.env("CLAUDE_CONFIG_DIR") {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return manual_report(spec, safe_issue_path(&path), MANUAL_PATH_REASON);
            }
            path
        }
        _ => match supported_default(context, spec) {
            Ok(()) => context.home().join(".claude"),
            Err(report) => return report,
        },
    };
    one_source(
        probes,
        spec,
        root.join("projects"),
        "claude_projects_jsonl_tree",
    )
}

fn resolve_open_code(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    if let Some(value) = nonempty_env(context, "OPENCODE_DB") {
        if value == OsStr::new(":memory:") {
            return no_disk_report(spec);
        }
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return one_source(probes, spec, path, "opencode_sqlite");
        }
        let data = match xdg_data_root(context, spec, "opencode") {
            Ok(path) => path,
            Err(report) => return report,
        };
        return one_source(probes, spec, data.join(path), "opencode_sqlite");
    }

    let data = match xdg_data_root(context, spec, "opencode") {
        Ok(path) => path,
        Err(report) => return report,
    };
    one_source(probes, spec, data.join("opencode.db"), "opencode_sqlite")
}

fn resolve_kilo(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    if let Some(value) = nonempty_env(context, "KILO_DB") {
        if value == OsStr::new(":memory:") {
            return no_disk_report(spec);
        }
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return one_source(probes, spec, path, "kilo_sqlite");
        }
        let data = match kilo_data_root(context, spec) {
            Ok(path) => path,
            Err(report) => return report,
        };
        return one_source(probes, spec, data.join(path), "kilo_sqlite");
    }

    let data = match kilo_data_root(context, spec) {
        Ok(path) => path,
        Err(report) => return report,
    };
    let current = data.join("kilo.db");
    let legacy = data.join("opencode.db");
    let selected = select_current_or_legacy(current, legacy);
    one_source(probes, spec, selected, "kilo_sqlite")
}

fn resolve_mimocode(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    if let Some(value) = nonempty_env(context, "MIMOCODE_DB") {
        if value == OsStr::new(":memory:") {
            return no_disk_report(spec);
        }
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return one_source(probes, spec, path, "mimocode_sqlite");
        }
        let data = match mimocode_data_root(context, spec) {
            Ok(path) => path,
            Err(report) => return report,
        };
        return one_source(probes, spec, data.join(path), "mimocode_sqlite");
    }

    let data = match mimocode_data_root(context, spec) {
        Ok(path) => path,
        Err(report) => return report,
    };
    one_source(probes, spec, data.join("mimocode.db"), "mimocode_sqlite")
}

fn resolve_goose(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    if let Some(value) = nonempty_env(context, "GOOSE_PATH_ROOT") {
        let root = PathBuf::from(value);
        if !root.is_absolute() {
            return manual_report(spec, safe_issue_path(&root), MANUAL_PATH_REASON);
        }
        return one_source(
            probes,
            spec,
            root.join("data/sessions/sessions.db"),
            "goose_sessions_sqlite",
        );
    }

    let data = match context.platform() {
        DiscoveryPlatform::Linux | DiscoveryPlatform::MacOS => match context.env("XDG_DATA_HOME") {
            Some(value) if !value.is_empty() && Path::new(value).is_absolute() => {
                PathBuf::from(value).join("goose")
            }
            _ => context.home().join(".local/share/goose"),
        },
        DiscoveryPlatform::Windows => {
            let Some(roaming) = context.platform_dirs().data.as_ref() else {
                return manual_report(spec, None, MANUAL_PATH_REASON);
            };
            roaming.join("Block/goose/data")
        }
        DiscoveryPlatform::OtherUnix => {
            if let Some(value) = context
                .env("XDG_DATA_HOME")
                .filter(|value| !value.is_empty() && Path::new(value).is_absolute())
            {
                PathBuf::from(value).join("goose")
            } else {
                return unsupported_platform_report(spec);
            }
        }
    };
    one_source(
        probes,
        spec,
        data.join("sessions/sessions.db"),
        "goose_sessions_sqlite",
    )
}

fn resolve_continue(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let root = match nonempty_env(context, "CONTINUE_GLOBAL_DIR") {
        Some(value) => match resolve_from_cwd(context, PathBuf::from(value)) {
            Some(path) => path,
            None => return manual_report(spec, None, MANUAL_PATH_REASON),
        },
        None => match supported_default(context, spec) {
            Ok(()) => context.home().join(".continue"),
            Err(report) => return report,
        },
    };
    one_source(
        probes,
        spec,
        root.join("sessions"),
        "continue_cli_sessions_json",
    )
}

fn resolve_gemini(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let base = match nonempty_env(context, "GEMINI_CLI_HOME") {
        Some(value) => match resolve_from_cwd(context, PathBuf::from(value)) {
            Some(path) => path,
            None => return manual_report(spec, None, MANUAL_PATH_REASON),
        },
        None => match supported_default(context, spec) {
            Ok(()) => context.home().to_path_buf(),
            Err(report) => return report,
        },
    };
    one_source(
        probes,
        spec,
        base.join(".gemini"),
        "gemini_cli_chat_recording_jsonl",
    )
}

fn resolve_tabnine(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let base = match nonempty_env(context, "GEMINI_CLI_HOME") {
        Some(value) => match resolve_from_cwd(context, PathBuf::from(value)) {
            Some(path) => path,
            None => return manual_report(spec, None, MANUAL_PATH_REASON),
        },
        None => match supported_default(context, spec) {
            Ok(()) => context.home().to_path_buf(),
            Err(report) => return report,
        },
    };
    one_source(
        probes,
        spec,
        base.join(".tabnine/agent"),
        "tabnine_cli_chat_recording_jsonl",
    )
}

fn resolve_cursor(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let base = match context.env("CURSOR_DATA_DIR") {
        Some(value) => {
            let Some(text) = value.to_str() else {
                return manual_report(spec, None, MANUAL_PATH_REASON);
            };
            if text.trim().is_empty() {
                match supported_default(context, spec) {
                    Ok(()) => context.home().join(".cursor"),
                    Err(report) => return report,
                }
            } else {
                match resolve_from_cwd(context, PathBuf::from(value)) {
                    Some(path) => path,
                    None => return manual_report(spec, None, MANUAL_PATH_REASON),
                }
            }
        }
        None => match supported_default(context, spec) {
            Ok(()) => context.home().join(".cursor"),
            Err(report) => return report,
        },
    };
    one_source(
        probes,
        spec,
        base.join("projects"),
        "cursor_agent_transcript_jsonl_tree",
    )
}

fn resolve_kimi(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let root = match nonempty_env(context, "KIMI_CODE_HOME") {
        Some(value) => match resolve_from_cwd(context, PathBuf::from(value)) {
            Some(path) => path,
            None => return manual_report(spec, None, MANUAL_PATH_REASON),
        },
        None => match supported_default(context, spec) {
            Ok(()) => context.home().join(".kimi-code"),
            Err(report) => return report,
        },
    };
    one_source(probes, spec, root, "kimi_code_cli_wire_jsonl_tree")
}

fn resolve_junie(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let home = match context.env("JUNIE_HOME") {
        Some(value) => {
            let Some(value) = value.to_str() else {
                return manual_report(spec, None, MANUAL_PATH_REASON);
            };
            match resolve_from_cwd(context, PathBuf::from(value)) {
                Some(path) => path,
                None => return manual_report(spec, None, MANUAL_PATH_REASON),
            }
        }
        None => match supported_default(context, spec) {
            Ok(()) => context.home().join(".junie"),
            Err(report) => return report,
        },
    };
    one_source(
        probes,
        spec,
        home.join("sessions"),
        "junie_session_events_jsonl_tree",
    )
}

fn resolve_factory(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    if let Err(report) = supported_default(context, spec) {
        return report;
    }
    one_source(
        probes,
        spec,
        context.home().join(".factory").join("sessions"),
        "factory_ai_droid_sessions_jsonl",
    )
}

fn resolve_fx(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    if let Err(report) = supported_default(context, spec) {
        return report;
    }
    one_source(
        probes,
        spec,
        context.home().join(".fx").join("sessions"),
        "fx_sessions_tree",
    )
}

fn resolve_forgecode(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    if let Some(value) = context.env("FORGE_CONFIG").and_then(OsStr::to_str) {
        let Some(base) = resolve_from_cwd(context, PathBuf::from(value)) else {
            return manual_report(spec, None, MANUAL_PATH_REASON);
        };
        return one_source_with_data_root(
            probes,
            context,
            spec,
            base.join(".forge.db"),
            "forgecode_sqlite",
        );
    }

    if let Err(report) = supported_default(context, spec) {
        return report;
    }
    let legacy = context.home().join("forge");
    let base = match path_presence(&legacy) {
        PathPresence::Present | PathPresence::Unsupported | PathPresence::Unknown(_) => legacy,
        PathPresence::Missing => context.home().join(".forge"),
    };
    one_source_with_data_root(
        probes,
        context,
        spec,
        base.join(".forge.db"),
        "forgecode_sqlite",
    )
}

fn kilo_data_root(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> Result<PathBuf, DiscoveryReport> {
    let base = match context.env("XDG_DATA_HOME") {
        Some(value) if !value.is_empty() => {
            let cleaned = value
                .to_string_lossy()
                .chars()
                .filter(|character| !matches!(character, '\r' | '\n'))
                .collect::<String>();
            let path = PathBuf::from(cleaned);
            if !path.is_absolute() {
                return Err(manual_report(
                    spec,
                    safe_issue_path(&path),
                    MANUAL_PATH_REASON,
                ));
            }
            path
        }
        _ => {
            supported_default(context, spec)?;
            context.home().join(".local/share")
        }
    };
    Ok(base.join("kilo"))
}

fn mimocode_data_root(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> Result<PathBuf, DiscoveryReport> {
    if let Some(value) = nonempty_env(context, "MIMOCODE_HOME") {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(manual_report(
                spec,
                safe_issue_path(&path),
                MANUAL_PATH_REASON,
            ));
        }
        return Ok(path.join("data"));
    }
    xdg_data_root(context, spec, "mimocode")
}

fn xdg_data_root(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    application: &str,
) -> Result<PathBuf, DiscoveryReport> {
    match context.env("XDG_DATA_HOME") {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return Err(manual_report(
                    spec,
                    safe_issue_path(&path),
                    MANUAL_PATH_REASON,
                ));
            }
            Ok(path.join(application))
        }
        _ => {
            supported_default(context, spec)?;
            Ok(context.home().join(".local/share").join(application))
        }
    }
}

fn supported_default(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> Result<(), DiscoveryReport> {
    if context.platform() == DiscoveryPlatform::OtherUnix {
        Err(unsupported_platform_report(spec))
    } else {
        Ok(())
    }
}

fn resolve_from_cwd(context: &DiscoveryContext, path: PathBuf) -> Option<PathBuf> {
    if path.is_absolute() {
        Some(path)
    } else {
        context.cwd().map(|cwd| cwd.join(path))
    }
}

fn nonempty_env<'a>(context: &'a DiscoveryContext, name: &str) -> Option<&'a OsStr> {
    context.env(name).filter(|value| !value.is_empty())
}

fn one_source(
    probes: &StaticProviderProbeCatalog,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    add_source(probes, &mut report, spec, path, source_format);
    report
}

fn one_source_with_data_root(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    add_source_with_data_root(probes, &mut report, context, spec, path, source_format);
    report
}

fn add_source(
    probes: &StaticProviderProbeCatalog,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
) {
    add_source_inner(probes, report, None, spec, path, source_format);
}

fn add_source_with_data_root(
    probes: &StaticProviderProbeCatalog,
    report: &mut DiscoveryReport,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
) {
    add_source_inner(
        probes,
        report,
        context.data_root(),
        spec,
        path,
        source_format,
    );
}

fn add_source_inner(
    probes: &StaticProviderProbeCatalog,
    report: &mut DiscoveryReport,
    data_root: Option<&Path>,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
) {
    if !encoded_path_within_limit(&path) {
        push_issue_once(
            report,
            spec,
            None,
            DiscoveryIssueKind::SelectorUnreconstructible,
            PATH_LIMIT_REASON,
        );
        return;
    }
    match path_presence(&path) {
        PathPresence::Unknown(kind) => {
            push_issue_once(
                report,
                spec,
                safe_issue_path(&path),
                DiscoveryIssueKind::SelectorUnreconstructible,
                path_presence_unknown_reason(kind),
            );
            return;
        }
        PathPresence::Unsupported => {
            push_issue_once(
                report,
                spec,
                safe_issue_path(&path),
                DiscoveryIssueKind::SelectorUnreconstructible,
                SYMLINK_REASON,
            );
            return;
        }
        PathPresence::Missing | PathPresence::Present => {}
    }
    let source = source_from_parts_with_data_root(
        probes,
        data_root,
        spec,
        path,
        source_format,
        ProviderSourceKind::NativeHistory,
    );
    if !push_source_candidate(&mut report.sources, source) {
        push_issue_once(
            report,
            spec,
            None,
            DiscoveryIssueKind::SelectorUnreconstructible,
            PATH_LIMIT_REASON,
        );
    }
}

fn safe_issue_path(path: &Path) -> Option<PathBuf> {
    encoded_path_within_limit(path).then(|| path.to_path_buf())
}

fn no_disk_report(spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    report.issues.push(issue(
        spec.provider,
        None,
        DiscoveryIssueKind::NoDiskHistory,
        NO_DISK_REASON,
    ));
    report
}

fn manual_report(
    spec: &ProviderSourceSpec,
    path: Option<PathBuf>,
    reason: &'static str,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    report.issues.push(issue(
        spec.provider,
        path,
        DiscoveryIssueKind::SelectorUnreconstructible,
        reason,
    ));
    report
}

fn unsupported_platform_report(spec: &ProviderSourceSpec) -> DiscoveryReport {
    manual_report(spec, None, UNSUPPORTED_PLATFORM_REASON)
}

fn push_issue_once(
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: Option<PathBuf>,
    kind: DiscoveryIssueKind,
    reason: &'static str,
) {
    if !report
        .issues
        .iter()
        .any(|existing| existing.kind == kind && existing.reason == reason)
    {
        report.issues.push(issue(spec.provider, path, kind, reason));
    }
}

#[cfg(test)]
#[path = "simple_tests.rs"]
mod tests;
