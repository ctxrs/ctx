use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;

use super::{
    super::{
        context::{DiscoveryContext, DiscoveryPlatform},
        reasons::path_presence_unknown_reason,
        selectors::{direct_entries, encoded_path_within_limit, source_path_kind, SourcePathKind},
        types::{DiscoveryIssueKind, DiscoveryReport, ProviderSourceKind, ProviderSourceSpec},
    },
    issue, path_presence, push_source_candidate, select_current_or_legacy,
    source_from_parts_with_data_root, unsupported_source, PathPresence,
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
const CODEX_COMPRESSION_REASON: &str =
    "Codex compressed .jsonl.zst history is detected but unsupported";
const CODEX_COMPRESSION_SCAN_REASON: &str =
    "bounded Codex compressed-history detection could not complete; use an exact --path for compressed rollouts";
const MAX_CODEX_COMPRESSION_ENTRIES: usize = 10_000;

/// Official winner-only custom-root policy for the thirteen scalar/fixed-root
/// providers owned by the simple resolver lane.
pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    match spec.provider {
        CaptureProvider::Codex => resolve_codex(context, spec),
        CaptureProvider::Claude => resolve_claude(context, spec),
        CaptureProvider::OpenCode => resolve_open_code(context, spec),
        CaptureProvider::Kilo => resolve_kilo(context, spec),
        CaptureProvider::MiMoCode => resolve_mimocode(context, spec),
        CaptureProvider::Goose => resolve_goose(context, spec),
        CaptureProvider::Continue => resolve_continue(context, spec),
        CaptureProvider::Gemini => resolve_gemini(context, spec),
        CaptureProvider::Tabnine => resolve_tabnine(context, spec),
        CaptureProvider::Cursor => resolve_cursor(context, spec),
        CaptureProvider::KimiCodeCli => resolve_kimi(context, spec),
        CaptureProvider::Junie => resolve_junie(context, spec),
        CaptureProvider::FactoryAiDroid => resolve_factory(context, spec),
        CaptureProvider::ForgeCode => resolve_forgecode(context, spec),
        _ => DiscoveryReport::default(),
    }
}

fn resolve_codex(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
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
    add_source(
        &mut report,
        spec,
        root.join("sessions"),
        "codex_session_jsonl_tree",
    );
    add_source(
        &mut report,
        spec,
        root.join("archived_sessions"),
        "codex_session_jsonl_tree",
    );
    add_source(
        &mut report,
        spec,
        root.join("history.jsonl"),
        "codex_history_jsonl",
    );

    for tree in [root.join("sessions"), root.join("archived_sessions")] {
        match compressed_codex_rollouts(&tree) {
            Ok(paths) => {
                for path in paths {
                    let source = unsupported_source(spec, path, CODEX_COMPRESSION_REASON);
                    if !push_source_candidate(&mut report.sources, source) {
                        push_issue_once(
                            &mut report,
                            spec,
                            None,
                            DiscoveryIssueKind::SelectorUnreconstructible,
                            PATH_LIMIT_REASON,
                        );
                    }
                }
            }
            Err(()) => push_issue_once(
                &mut report,
                spec,
                safe_issue_path(&tree),
                DiscoveryIssueKind::SelectorUnreconstructible,
                CODEX_COMPRESSION_SCAN_REASON,
            ),
        }
    }
    report
}

fn resolve_claude(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
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
    one_source(spec, root.join("projects"), "claude_projects_jsonl_tree")
}

fn resolve_open_code(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if let Some(value) = nonempty_env(context, "OPENCODE_DB") {
        if value == OsStr::new(":memory:") {
            return no_disk_report(spec);
        }
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return one_source(spec, path, "opencode_sqlite");
        }
        let data = match xdg_data_root(context, spec, "opencode") {
            Ok(path) => path,
            Err(report) => return report,
        };
        return one_source(spec, data.join(path), "opencode_sqlite");
    }

    let data = match xdg_data_root(context, spec, "opencode") {
        Ok(path) => path,
        Err(report) => return report,
    };
    one_source(spec, data.join("opencode.db"), "opencode_sqlite")
}

fn resolve_kilo(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if let Some(value) = nonempty_env(context, "KILO_DB") {
        if value == OsStr::new(":memory:") {
            return no_disk_report(spec);
        }
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return one_source(spec, path, "kilo_sqlite");
        }
        let data = match kilo_data_root(context, spec) {
            Ok(path) => path,
            Err(report) => return report,
        };
        return one_source(spec, data.join(path), "kilo_sqlite");
    }

    let data = match kilo_data_root(context, spec) {
        Ok(path) => path,
        Err(report) => return report,
    };
    let current = data.join("kilo.db");
    let legacy = data.join("opencode.db");
    let selected = select_current_or_legacy(current, legacy);
    one_source(spec, selected, "kilo_sqlite")
}

fn resolve_mimocode(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if let Some(value) = nonempty_env(context, "MIMOCODE_DB") {
        if value == OsStr::new(":memory:") {
            return no_disk_report(spec);
        }
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return one_source(spec, path, "mimocode_sqlite");
        }
        let data = match mimocode_data_root(context, spec) {
            Ok(path) => path,
            Err(report) => return report,
        };
        return one_source(spec, data.join(path), "mimocode_sqlite");
    }

    let data = match mimocode_data_root(context, spec) {
        Ok(path) => path,
        Err(report) => return report,
    };
    one_source(spec, data.join("mimocode.db"), "mimocode_sqlite")
}

fn resolve_goose(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if let Some(value) = nonempty_env(context, "GOOSE_PATH_ROOT") {
        let root = PathBuf::from(value);
        if !root.is_absolute() {
            return manual_report(spec, safe_issue_path(&root), MANUAL_PATH_REASON);
        }
        return one_source(
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
        spec,
        data.join("sessions/sessions.db"),
        "goose_sessions_sqlite",
    )
}

fn resolve_continue(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
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
    one_source(spec, root.join("sessions"), "continue_cli_sessions_json")
}

fn resolve_gemini(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
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
        spec,
        base.join(".gemini"),
        "gemini_cli_chat_recording_jsonl",
    )
}

fn resolve_tabnine(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
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
        spec,
        base.join(".tabnine/agent"),
        "tabnine_cli_chat_recording_jsonl",
    )
}

fn resolve_cursor(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
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
        spec,
        base.join("projects"),
        "cursor_agent_transcript_jsonl_tree",
    )
}

fn resolve_kimi(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
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
    one_source(spec, root, "kimi_code_cli_wire_jsonl_tree")
}

fn resolve_junie(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
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
        spec,
        home.join("sessions"),
        "junie_session_events_jsonl_tree",
    )
}

fn resolve_factory(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if let Err(report) = supported_default(context, spec) {
        return report;
    }
    one_source(
        spec,
        context.home().join(".factory").join("sessions"),
        "factory_ai_droid_sessions_jsonl",
    )
}

fn resolve_forgecode(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if let Some(value) = context.env("FORGE_CONFIG").and_then(OsStr::to_str) {
        let Some(base) = resolve_from_cwd(context, PathBuf::from(value)) else {
            return manual_report(spec, None, MANUAL_PATH_REASON);
        };
        return one_source_with_data_root(
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
    one_source_with_data_root(context, spec, base.join(".forge.db"), "forgecode_sqlite")
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
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    add_source(&mut report, spec, path, source_format);
    report
}

fn one_source_with_data_root(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    add_source_with_data_root(&mut report, context, spec, path, source_format);
    report
}

fn add_source(
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
) {
    add_source_inner(report, None, spec, path, source_format);
}

fn add_source_with_data_root(
    report: &mut DiscoveryReport,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
) {
    add_source_inner(report, context.data_root(), spec, path, source_format);
}

fn add_source_inner(
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

fn compressed_codex_rollouts(root: &Path) -> Result<Vec<PathBuf>, ()> {
    match path_presence(root) {
        PathPresence::Missing => return Ok(Vec::new()),
        PathPresence::Present if source_path_kind(root) == Ok(SourcePathKind::Directory) => {}
        PathPresence::Present | PathPresence::Unsupported | PathPresence::Unknown(_) => {
            return Err(())
        }
    }

    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut examined = 0usize;
    while let Some(directory) = pending.pop() {
        let entries = direct_entries(&directory).map_err(|_| ())?;
        examined = examined.saturating_add(entries.len());
        if examined > MAX_CODEX_COMPRESSION_ENTRIES {
            return Err(());
        }
        for path in entries.into_iter().rev() {
            match source_path_kind(&path).map_err(|_| ())? {
                SourcePathKind::Directory => pending.push(path),
                SourcePathKind::File
                    if path
                        .file_name()
                        .and_then(OsStr::to_str)
                        .is_some_and(|name| name.ends_with(".jsonl.zst")) =>
                {
                    found.push(path);
                }
                SourcePathKind::File => {}
            }
        }
    }
    found.sort_by_cached_key(|path| super::super::selectors::encoded_path_sort_key(path));
    Ok(found)
}

#[cfg(test)]
#[path = "simple_tests.rs"]
mod tests;
