use std::path::{Path, PathBuf};

use ctx_history_core::CaptureProvider;

use super::super::{
    context::{DiscoveryContext, DiscoveryPlatform},
    reasons::{empty_source_reason, probe_io_error_reason, unknown_source_reason},
    selectors::{
        direct_entries, encoded_path_within_limit, source_path_kind, SourcePathKind,
        MAX_DIRECT_DIRECTORY_ENTRIES, MAX_FINITE_SELECTOR_ENTRIES, MAX_PROJECT_ANCESTORS,
    },
    types::{
        DiscoveryIssueKind, DiscoveryReport, ProviderSource, ProviderSourceKind,
        ProviderSourceSpec, ProviderSourceStatus,
    },
};
use super::{
    issue, path_presence, push_source_candidate, select_current_or_legacy,
    source_from_parts_with_data_root, unsupported_source, PathPresence,
};

const QODER_DIRECT_UNSUPPORTED: &str =
    "Qoder direct SDK JSONL history without a transcript directory is detected but unsupported";
const MUX_ARCHIVE_UNSUPPORTED: &str = "Mux chat-archive.jsonl history is detected but unsupported";
const CLINE_CURRENT_UNSUPPORTED: &str =
    "current Cline SDK session history is detected but unsupported";
const MANUAL_SELECTOR_REASON: &str =
    "the provider selector cannot be safely reconstructed; use an exact --path";
const UNSAFE_SELECTED_PATH_REASON: &str =
    "the selected provider path is unreadable, non-ordinary, or crosses a link; use an exact real --path";

pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    match spec.provider {
        CaptureProvider::Qoder => resolve_qoder(context, spec),
        CaptureProvider::Firebender => resolve_firebender(context, spec),
        CaptureProvider::Auggie => resolve_auggie(context, spec),
        CaptureProvider::DeepAgents => resolve_deepagents(context, spec),
        CaptureProvider::Mux => resolve_mux(context, spec),
        CaptureProvider::Cline => resolve_cline(context, spec),
        _ => DiscoveryReport::default(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeState {
    Missing,
    Available,
    Empty,
    Unknown,
}

fn native_source(
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
    state: ProbeState,
) -> ProviderSource {
    ProviderSource {
        provider: spec.provider,
        path,
        exists: !matches!(state, ProbeState::Missing),
        source_format,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: spec.import_support,
        catalog_support: spec.catalog_support,
        status: match state {
            ProbeState::Missing => ProviderSourceStatus::Missing,
            ProbeState::Available => ProviderSourceStatus::Available,
            ProbeState::Empty => ProviderSourceStatus::Empty,
            ProbeState::Unknown => ProviderSourceStatus::Unknown,
        },
        unsupported_reason: match state {
            ProbeState::Empty => empty_source_reason(spec.provider),
            ProbeState::Unknown => unknown_source_reason(spec.provider)
                .or_else(|| probe_io_error_reason(spec.provider)),
            ProbeState::Missing | ProbeState::Available => None,
        },
    }
}

fn push_selected_source(report: &mut DiscoveryReport, source: ProviderSource) {
    let provider = source.provider;
    let path = source.path.clone();
    if !push_source_candidate(&mut report.sources, source) {
        report.issues.push(issue(
            provider,
            bounded_issue_path(path),
            DiscoveryIssueKind::SelectorUnreconstructible,
            MANUAL_SELECTOR_REASON,
        ));
    }
}

fn bounded_issue_path(path: PathBuf) -> Option<PathBuf> {
    encoded_path_within_limit(&path).then_some(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SafePathKind {
    Missing,
    File,
    Directory,
    Unsafe,
}

fn safe_path_kind(path: &Path) -> SafePathKind {
    match path_presence(path) {
        PathPresence::Missing => return SafePathKind::Missing,
        PathPresence::Unsupported | PathPresence::Unknown(_) => return SafePathKind::Unsafe,
        PathPresence::Present => {}
    }
    match source_path_kind(path) {
        Ok(SourcePathKind::File) => SafePathKind::File,
        Ok(SourcePathKind::Directory) => SafePathKind::Directory,
        Err(_) => SafePathKind::Unsafe,
    }
}

fn is_regular_file_named(path: &Path, expected: &str) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some(expected)
        && matches!(safe_path_kind(path), SafePathKind::File)
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some(extension)
}

fn default_supported_platform(context: &DiscoveryContext) -> bool {
    !matches!(context.platform(), DiscoveryPlatform::OtherUnix)
}

fn resolve_qoder(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if let Some(value) = context
        .env("QODER_CONFIG_DIR")
        .filter(|value| !value.is_empty())
    {
        let manual_path = PathBuf::from(value);
        let manual_path = manual_path
            .is_absolute()
            .then(|| manual_path.join("projects"))
            .and_then(bounded_issue_path);
        report.issues.push(issue(
            spec.provider,
            manual_path,
            DiscoveryIssueKind::SelectorUnreconstructible,
            "QODER_CONFIG_DIR is SDK-scoped and not a registered standalone writer root; use its exact projects path with --path",
        ));
        return report;
    }
    if default_supported_platform(context) {
        let projects = context.home().join(".qoder").join("projects");
        let (legacy_state, direct_sdk_history) = inspect_qoder_projects(&projects);
        push_selected_source(
            &mut report,
            native_source(
                spec,
                projects.clone(),
                "qoder_transcript_jsonl_tree",
                legacy_state,
            ),
        );
        if direct_sdk_history {
            push_selected_source(
                &mut report,
                unsupported_source(spec, projects, QODER_DIRECT_UNSUPPORTED),
            );
        }
    }

    report
}

fn inspect_qoder_projects(projects: &Path) -> (ProbeState, bool) {
    match safe_path_kind(projects) {
        SafePathKind::Missing => return (ProbeState::Missing, false),
        SafePathKind::Directory => {}
        SafePathKind::File | SafePathKind::Unsafe => return (ProbeState::Unknown, false),
    }
    let buckets = match direct_entries(projects) {
        Ok(entries) => entries,
        Err(_) => return (ProbeState::Unknown, false),
    };
    let mut legacy = false;
    let mut direct = false;
    for bucket in buckets {
        if !matches!(safe_path_kind(&bucket), SafePathKind::Directory) {
            continue;
        }
        let entries = match direct_entries(&bucket) {
            Ok(entries) => entries,
            Err(_) => return (ProbeState::Unknown, direct),
        };
        direct |= entries.iter().any(|path| {
            has_extension(path, "jsonl") && matches!(safe_path_kind(path), SafePathKind::File)
        });

        let transcript = bucket.join("transcript");
        match safe_path_kind(&transcript) {
            SafePathKind::Missing => {}
            SafePathKind::Directory => {
                let files = match direct_entries(&transcript) {
                    Ok(entries) => entries,
                    Err(_) => return (ProbeState::Unknown, direct),
                };
                legacy |= files.iter().any(|path| {
                    has_extension(path, "jsonl")
                        && matches!(safe_path_kind(path), SafePathKind::File)
                });
            }
            SafePathKind::File | SafePathKind::Unsafe => return (ProbeState::Unknown, direct),
        }
    }
    (
        if legacy {
            ProbeState::Available
        } else {
            ProbeState::Empty
        },
        direct,
    )
}

fn resolve_firebender(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    let Some(project_root) = firebender_project_root(context.cwd()) else {
        return report;
    };
    push_selected_source(
        &mut report,
        source_from_parts_with_data_root(
            context.data_root(),
            spec,
            project_root
                .join(".idea")
                .join("firebender")
                .join("chat_history.db"),
            "firebender_chat_history_sqlite",
            ProviderSourceKind::NativeHistory,
        ),
    );
    report
}

fn firebender_project_root(cwd: Option<&Path>) -> Option<PathBuf> {
    let cwd = cwd?;
    for candidate in cwd.ancestors().take(MAX_PROJECT_ANCESTORS) {
        match safe_path_kind(&candidate.join(".idea")) {
            SafePathKind::Directory => return Some(candidate.to_path_buf()),
            SafePathKind::Missing => {}
            SafePathKind::File | SafePathKind::Unsafe => return None,
        }
        match safe_path_kind(&candidate.join(".git")) {
            SafePathKind::File | SafePathKind::Directory => {
                return Some(candidate.to_path_buf());
            }
            SafePathKind::Missing => {}
            SafePathKind::Unsafe => return None,
        }
    }
    None
}

fn resolve_auggie(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if !default_supported_platform(context) {
        return DiscoveryReport::default();
    }
    let sessions = context.home().join(".augment").join("sessions");
    let state = inspect_flat_extension_directory(&sessions, "json");
    let mut report = DiscoveryReport::default();
    push_selected_source(
        &mut report,
        native_source(spec, sessions, "auggie_session_json", state),
    );
    report
}

fn inspect_flat_extension_directory(path: &Path, extension: &str) -> ProbeState {
    match safe_path_kind(path) {
        SafePathKind::Missing => return ProbeState::Missing,
        SafePathKind::Directory => {}
        SafePathKind::File | SafePathKind::Unsafe => return ProbeState::Unknown,
    }
    match direct_entries(path) {
        Ok(entries)
            if entries.iter().any(|entry| {
                has_extension(entry, extension)
                    && matches!(safe_path_kind(entry), SafePathKind::File)
            }) =>
        {
            ProbeState::Available
        }
        Ok(_) => ProbeState::Empty,
        Err(_) => ProbeState::Unknown,
    }
}

fn resolve_deepagents(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let current = context
        .home()
        .join(".deepagents")
        .join(".state")
        .join("sessions.db");
    let legacy = context.home().join(".deepagents").join("sessions.db");
    let selected = select_current_or_legacy(current, legacy);
    let state = match safe_path_kind(&selected) {
        SafePathKind::Missing => ProbeState::Missing,
        SafePathKind::File => ProbeState::Available,
        SafePathKind::Directory | SafePathKind::Unsafe => ProbeState::Unknown,
    };
    let mut report = DiscoveryReport::default();
    push_selected_source(
        &mut report,
        native_source(spec, selected.clone(), "deepagents_sessions_sqlite", state),
    );
    if matches!(state, ProbeState::Unknown) {
        report.issues.push(issue(
            spec.provider,
            bounded_issue_path(selected),
            DiscoveryIssueKind::SelectorUnreconstructible,
            UNSAFE_SELECTED_PATH_REASON,
        ));
    }
    report
}

fn resolve_mux(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    let selected = match raw_env_path(context, "MUX_ROOT", false) {
        EnvPath::Selected(path) => Some(path),
        EnvPath::Unreconstructible(path) => {
            report.issues.push(issue(
                spec.provider,
                path,
                DiscoveryIssueKind::SelectorUnreconstructible,
                MANUAL_SELECTOR_REASON,
            ));
            None
        }
        EnvPath::Absent => {
            if !default_supported_platform(context) {
                return report;
            }
            let root = if context
                .env("NODE_ENV")
                .is_some_and(|value| value == "development")
            {
                context.home().join(".mux-dev")
            } else {
                let current = context.home().join(".mux");
                let legacy = context.home().join(".cmux");
                select_current_or_legacy(current, legacy)
            };
            Some(root)
        }
    };
    let Some(root) = selected else {
        return report;
    };
    let sessions = root.join("sessions");
    let (state, archive) = inspect_mux_sessions(&sessions);
    push_selected_source(
        &mut report,
        native_source(spec, sessions.clone(), "mux_session_jsonl_tree", state),
    );
    if archive {
        push_selected_source(
            &mut report,
            unsupported_source(spec, sessions, MUX_ARCHIVE_UNSUPPORTED),
        );
    }
    report
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EnvPath {
    Absent,
    Selected(PathBuf),
    Unreconstructible(Option<PathBuf>),
}

fn raw_env_path(context: &DiscoveryContext, name: &str, trim: bool) -> EnvPath {
    let Some(raw) = context.env(name) else {
        return EnvPath::Absent;
    };
    let Some(text) = raw.to_str() else {
        return EnvPath::Unreconstructible(None);
    };
    let text = if trim { text.trim() } else { text };
    if text.is_empty() {
        return EnvPath::Absent;
    }
    let path = PathBuf::from(text);
    let path = if path.is_absolute() {
        path
    } else if let Some(cwd) = context.cwd() {
        cwd.join(path)
    } else {
        return EnvPath::Unreconstructible(None);
    };
    if encoded_path_within_limit(&path) {
        EnvPath::Selected(path)
    } else {
        EnvPath::Unreconstructible(None)
    }
}

fn inspect_mux_sessions(path: &Path) -> (ProbeState, bool) {
    match safe_path_kind(path) {
        SafePathKind::Missing => return (ProbeState::Missing, false),
        SafePathKind::Directory => {}
        SafePathKind::File | SafePathKind::Unsafe => return (ProbeState::Unknown, false),
    }
    let mut stack = vec![(path.to_path_buf(), 0usize)];
    let mut examined = 0usize;
    let mut active = false;
    let mut archive = false;
    while let Some((directory, depth)) = stack.pop() {
        let entries = match direct_entries(&directory) {
            Ok(entries) => entries,
            Err(_) => return (ProbeState::Unknown, archive),
        };
        examined = examined.saturating_add(entries.len());
        if examined > MAX_DIRECT_DIRECTORY_ENTRIES {
            return (ProbeState::Unknown, archive);
        }
        for entry in entries.into_iter().rev() {
            match safe_path_kind(&entry) {
                SafePathKind::File => match entry.file_name().and_then(|name| name.to_str()) {
                    Some("chat.jsonl" | "partial.json") => active = true,
                    Some("chat-archive.jsonl") => archive = true,
                    _ => {}
                },
                SafePathKind::Directory if depth < 4 => stack.push((entry, depth + 1)),
                SafePathKind::Directory | SafePathKind::Missing | SafePathKind::Unsafe => {}
            }
        }
    }
    (
        if active {
            ProbeState::Available
        } else {
            ProbeState::Empty
        },
        archive,
    )
}

fn resolve_cline(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    let selected_legacy = cline_legacy_root(context, spec, &mut report);
    if let Some(path) = selected_legacy.as_ref() {
        push_selected_source(
            &mut report,
            native_source(
                spec,
                path.clone(),
                "cline_task_directory_json",
                inspect_cline_legacy(path),
            ),
        );
    }

    add_cline_microsoft_host_roots(context, spec, &mut report);
    add_current_cline_detections(context, spec, selected_legacy.as_deref(), &mut report);
    report
}

fn cline_legacy_root(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    report: &mut DiscoveryReport,
) -> Option<PathBuf> {
    let sandbox = context
        .env("CLINE_SANDBOX")
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.trim() == "1");
    for (name, append_data, enabled) in [
        ("CLINE_DATA_DIR", false, true),
        ("CLINE_SANDBOX_DATA_DIR", false, sandbox),
        ("CLINE_DIR", true, true),
    ] {
        if !enabled {
            continue;
        }
        match raw_env_path(context, name, true) {
            EnvPath::Selected(path) => {
                return Some(if append_data { path.join("data") } else { path })
            }
            EnvPath::Unreconstructible(path) => {
                report.issues.push(issue(
                    spec.provider,
                    path,
                    DiscoveryIssueKind::SelectorUnreconstructible,
                    MANUAL_SELECTOR_REASON,
                ));
                return None;
            }
            EnvPath::Absent => {}
        }
    }
    default_supported_platform(context).then(|| context.home().join(".cline").join("data"))
}

fn add_cline_microsoft_host_roots(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    report: &mut DiscoveryReport,
) {
    let Some(config) = cline_platform_config_root(context) else {
        return;
    };
    let mut profile_count = 0usize;
    for host in ["Code", "Code - Insiders"] {
        let user = config.join(host).join("User");
        if !matches!(safe_path_kind(&user), SafePathKind::Directory) {
            continue;
        }
        let default = user.join("globalStorage").join("saoudrizwan.claude-dev");
        push_selected_source(
            report,
            native_source(
                spec,
                default.clone(),
                "cline_task_directory_json",
                inspect_cline_legacy(&default),
            ),
        );

        let profiles = user.join("profiles");
        let entries = match safe_path_kind(&profiles) {
            SafePathKind::Missing => continue,
            SafePathKind::Directory => match direct_entries(&profiles) {
                Ok(entries) => entries,
                Err(_) => {
                    report.issues.push(issue(
                        spec.provider,
                        bounded_issue_path(profiles),
                        DiscoveryIssueKind::SelectorUnreconstructible,
                        "Cline host profile enumeration exceeded a fixed safety bound; use an exact --path",
                    ));
                    continue;
                }
            },
            SafePathKind::File | SafePathKind::Unsafe => continue,
        };
        for profile in entries {
            if profile_count >= MAX_FINITE_SELECTOR_ENTRIES {
                break;
            }
            if !matches!(safe_path_kind(&profile), SafePathKind::Directory) {
                continue;
            }
            profile_count += 1;
            let root = profile.join("globalStorage").join("saoudrizwan.claude-dev");
            push_selected_source(
                report,
                native_source(
                    spec,
                    root.clone(),
                    "cline_task_directory_json",
                    inspect_cline_legacy(&root),
                ),
            );
        }
    }
}

fn cline_platform_config_root(context: &DiscoveryContext) -> Option<PathBuf> {
    match context.platform() {
        DiscoveryPlatform::Linux => context
            .platform_dirs()
            .config
            .clone()
            .or_else(|| Some(context.home().join(".config"))),
        DiscoveryPlatform::MacOS => context
            .platform_dirs()
            .config
            .clone()
            .or_else(|| Some(context.home().join("Library").join("Application Support"))),
        DiscoveryPlatform::Windows => context.platform_dirs().config.clone(),
        DiscoveryPlatform::OtherUnix => None,
    }
}

fn inspect_cline_legacy(root: &Path) -> ProbeState {
    match safe_path_kind(root) {
        SafePathKind::Missing => return ProbeState::Missing,
        SafePathKind::Directory => {}
        SafePathKind::File => {
            return if is_cline_legacy_marker(root) {
                ProbeState::Available
            } else {
                ProbeState::Empty
            }
        }
        SafePathKind::Unsafe => return ProbeState::Unknown,
    }
    let entries = match direct_entries(root) {
        Ok(entries) => entries,
        Err(_) => return ProbeState::Unknown,
    };
    if entries.iter().any(|entry| is_cline_legacy_marker(entry)) {
        return ProbeState::Available;
    }
    for directory in entries
        .iter()
        .filter(|entry| matches!(safe_path_kind(entry), SafePathKind::Directory))
    {
        if directory.file_name().and_then(|name| name.to_str()) == Some("tasks") {
            let tasks = match direct_entries(directory) {
                Ok(entries) => entries,
                Err(_) => return ProbeState::Unknown,
            };
            if tasks.iter().any(|task| {
                matches!(safe_path_kind(task), SafePathKind::Directory)
                    && cline_directory_has_marker(task)
            }) {
                return ProbeState::Available;
            }
        } else if cline_directory_has_marker(directory) {
            return ProbeState::Available;
        }
    }
    ProbeState::Empty
}

fn cline_directory_has_marker(path: &Path) -> bool {
    [
        "api_conversation_history.json",
        "ui_messages.json",
        "task_metadata.json",
    ]
    .iter()
    .any(|name| is_regular_file_named(&path.join(name), name))
}

fn is_cline_legacy_marker(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("api_conversation_history.json" | "ui_messages.json" | "task_metadata.json")
    ) && matches!(safe_path_kind(path), SafePathKind::File)
}

fn add_current_cline_detections(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    legacy_root: Option<&Path>,
    report: &mut DiscoveryReport,
) {
    let session_root = cline_current_root(
        context,
        spec,
        report,
        "CLINE_SESSION_DATA_DIR",
        legacy_root.map(|root| root.join("sessions")),
    );
    if let Some(path) = session_root.filter(|path| has_current_cline_session_shape(path)) {
        push_selected_source(
            report,
            unsupported_source(spec, path, CLINE_CURRENT_UNSUPPORTED),
        );
    }

    let db_root = cline_current_root(
        context,
        spec,
        report,
        "CLINE_DB_DATA_DIR",
        legacy_root.map(|root| root.join("db")),
    );
    if let Some(path) = db_root {
        let db = path.join("sessions.db");
        if is_regular_file_named(&db, "sessions.db") {
            push_selected_source(
                report,
                unsupported_source(spec, db, CLINE_CURRENT_UNSUPPORTED),
            );
        }
    }
}

fn cline_current_root(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    report: &mut DiscoveryReport,
    name: &str,
    fallback: Option<PathBuf>,
) -> Option<PathBuf> {
    match raw_env_path(context, name, true) {
        EnvPath::Selected(path) => Some(path),
        EnvPath::Absent => fallback,
        EnvPath::Unreconstructible(path) => {
            report.issues.push(issue(
                spec.provider,
                path,
                DiscoveryIssueKind::SelectorUnreconstructible,
                MANUAL_SELECTOR_REASON,
            ));
            None
        }
    }
}

fn has_current_cline_session_shape(path: &Path) -> bool {
    if !matches!(safe_path_kind(path), SafePathKind::Directory) {
        return false;
    }
    if is_regular_file_named(&path.join("sessions.index.json"), "sessions.index.json") {
        return true;
    }
    let entries = match direct_entries(path) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    entries.into_iter().any(|session| {
        if !matches!(safe_path_kind(&session), SafePathKind::Directory) {
            return false;
        }
        let Some(id) = session.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        is_regular_file_named(&session.join(format!("{id}.json")), &format!("{id}.json"))
            && is_regular_file_named(
                &session.join(format!("{id}.messages.json")),
                &format!("{id}.messages.json"),
            )
    })
}

#[cfg(test)]
#[rustfmt::skip]
#[path = "manual_unsupported_tests.rs"]
mod tests;
