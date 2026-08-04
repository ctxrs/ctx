use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;

use super::super::{
    context::{DiscoveryContext, DiscoveryPlatform},
    selectors::{
        direct_entries, encoded_path_within_limit, ordinary_directory, ordinary_file,
        SelectorReadError, MAX_SOURCE_CANDIDATES_PER_PROVIDER,
    },
    types::{
        DiscoveryIssueKind, DiscoveryReport, ProviderSource, ProviderSourceKind,
        ProviderSourceSpec, ProviderSourceStatus,
    },
    warp::{
        installed_platform, DiscoveredWarpSource, WarpDiscoveryUnavailable, WarpInstalledPlatform,
        WarpInstalledSurfaceKey, WarpReleaseChannel, WarpTerminalSurface,
    },
};
use super::{
    dedupe_report, issue, path_presence, push_source_candidate, source_from_parts_with_data_root,
    unsupported_source, PathPresence,
};

mod lingma;

pub(in crate::provider_sources) use lingma::resolve_lingma_with_authority;

const KIRO_FORMAT: &str = "kiro_cli_sqlite";
const WARP_FORMAT: &str = "warp_sqlite";
const CODEBUDDY_FORMAT: &str = "codebuddy_history_json";
const TRAE_FORMAT: &str = "trae_state_vscdb";
const ZED_FORMAT: &str = "zed_threads_sqlite";
const COPILOT_FORMAT: &str = "copilot_cli_session_events_jsonl";
const ANTIGRAVITY_FORMAT: &str = "antigravity_cli_transcript_jsonl_tree";
const WINDSURF_FORMAT: &str = "windsurf_cascade_hook_transcript_jsonl_tree";

const SELECTOR_MANUAL_REASON: &str = "the provider selected a path whose location cannot be reconstructed safely; use an exact --path";
const UNSAFE_SOURCE_REASON: &str =
    "the selected provider history path contains a symlink or non-ordinary component";
const ZED_STATELESS_REASON: &str =
    "Zed stateless mode selected an in-memory database, so there is no disk history root";

pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let report = match spec.provider {
        CaptureProvider::KiroCli => resolve_kiro(context, spec),
        CaptureProvider::Warp => resolve_warp(context, spec),
        CaptureProvider::CodeBuddy => resolve_codebuddy(context, spec),
        CaptureProvider::Lingma => lingma::resolve_lingma(context, spec),
        CaptureProvider::Zed => resolve_zed(context, spec),
        CaptureProvider::CopilotCli => resolve_copilot(context, spec),
        CaptureProvider::Trae => resolve_trae(context, spec),
        CaptureProvider::Antigravity => resolve_antigravity(context, spec),
        CaptureProvider::Windsurf => resolve_windsurf(context, spec),
        _ => DiscoveryReport::default(),
    };
    dedupe_report(report)
}

fn resolve_kiro(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if !supported_desktop_platform(context.platform()) {
        return DiscoveryReport::default();
    }

    let current_root = match context.env("KIRO_HOME") {
        None => context.home().join(".kiro"),
        Some(value) if value.is_empty() => context.home().join(".kiro"),
        Some(value) => {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return selector_issue_report(spec, Some(path));
            }
            path
        }
    };
    let current_sessions = current_root.join("sessions");
    match path_presence(&current_sessions) {
        PathPresence::Present => {
            if !ordinary_directory(&current_sessions) {
                return selector_issue_report(spec, Some(current_sessions));
            }
            return DiscoveryReport {
                sources: vec![unsupported_source(
                    spec,
                    current_sessions,
                    "current Kiro ACP/v3 sessions are detected but are not supported by the Kiro SQLite importer",
                )],
                issues: Vec::new(),
            };
        }
        PathPresence::Missing => {}
        PathPresence::Unsupported | PathPresence::Unknown(_) => {
            return selector_issue_report(spec, Some(current_sessions))
        }
    }

    let legacy = match context.platform() {
        DiscoveryPlatform::Linux => {
            let data_root = absolute_xdg_or_default(
                context.env("XDG_DATA_HOME"),
                context.home().join(".local").join("share"),
            );
            Some(data_root.join("kiro-cli").join("data.sqlite3"))
        }
        DiscoveryPlatform::MacOS => Some(
            context
                .home()
                .join("Library")
                .join("Application Support")
                .join("kiro-cli")
                .join("data.sqlite3"),
        ),
        DiscoveryPlatform::Windows | DiscoveryPlatform::OtherUnix => None,
    };
    let mut report = DiscoveryReport::default();
    if let Some(path) = legacy {
        push_source_candidate(
            &mut report.sources,
            safe_native_source(context.data_root(), spec, path, KIRO_FORMAT),
        );
    }
    report
}

fn resolve_warp(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let sources = match resolve_warp_with_authority(context, spec) {
        Ok(sources) => sources
            .into_iter()
            .map(|candidate| candidate.into_parts().0)
            .collect(),
        Err(_) => Vec::new(),
    };
    DiscoveryReport {
        sources,
        issues: Vec::new(),
    }
}

pub(in crate::provider_sources) fn resolve_warp_with_authority(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> Result<Vec<DiscoveredWarpSource>, WarpDiscoveryUnavailable> {
    let platform = installed_platform(context.platform())?;
    let mut sources = Vec::new();
    match platform {
        WarpInstalledPlatform::Linux => {
            let state = absolute_xdg_or_default(
                context.env("XDG_STATE_HOME"),
                context
                    .platform_dirs()
                    .state
                    .clone()
                    .unwrap_or_else(|| context.home().join(".local").join("state")),
            );
            add_warp_channel(
                &mut sources,
                context.data_root(),
                spec,
                platform,
                WarpReleaseChannel::Stable,
                state.join("warp-terminal"),
            )?;
            add_warp_channel(
                &mut sources,
                context.data_root(),
                spec,
                platform,
                WarpReleaseChannel::Preview,
                state.join("warp-terminal-preview"),
            )?;
        }
        WarpInstalledPlatform::MacOS => {
            let group = context
                .home()
                .join("Library")
                .join("Group Containers")
                .join("2BBY89MBSN.dev.warp")
                .join("Library")
                .join("Application Support");
            let fallback = context.home().join("Library").join("Application Support");
            let stable = select_existing_precedence(
                group.join("dev.warp.Warp-Stable"),
                fallback.join("dev.warp.Warp-Stable"),
                true,
            );
            if let Some(root) = stable {
                add_warp_channel(
                    &mut sources,
                    context.data_root(),
                    spec,
                    platform,
                    WarpReleaseChannel::Stable,
                    root,
                )?;
            }
            let preview = select_existing_precedence(
                group.join("dev.warp.Warp-Preview"),
                fallback.join("dev.warp.Warp-Preview"),
                false,
            );
            if let Some(root) = preview {
                add_warp_channel(
                    &mut sources,
                    context.data_root(),
                    spec,
                    platform,
                    WarpReleaseChannel::Preview,
                    root,
                )?;
            }
        }
        WarpInstalledPlatform::Windows => {
            let local_data = context
                .platform_dirs()
                .local_data
                .as_ref()
                .ok_or(WarpDiscoveryUnavailable::WindowsLocalDataRootUnavailable)?;
            let warp = local_data.join("warp");
            add_warp_channel(
                &mut sources,
                context.data_root(),
                spec,
                platform,
                WarpReleaseChannel::Stable,
                warp.join("Warp").join("data"),
            )?;
            add_warp_channel(
                &mut sources,
                context.data_root(),
                spec,
                platform,
                WarpReleaseChannel::Preview,
                warp.join("WarpPreview").join("data"),
            )?;
        }
    }
    Ok(sources)
}

fn add_warp_channel(
    sources: &mut Vec<DiscoveredWarpSource>,
    data_root: Option<&Path>,
    spec: &ProviderSourceSpec,
    platform: WarpInstalledPlatform,
    channel: WarpReleaseChannel,
    root: PathBuf,
) -> Result<(), WarpDiscoveryUnavailable> {
    let gui = root.join("warp.sqlite");
    let installed = channel == WarpReleaseChannel::Stable
        || path_presence(&root).suppresses_fallback()
        || path_presence(&gui).suppresses_fallback();
    if !installed {
        return Ok(());
    }
    push_warp_source(
        sources,
        data_root,
        spec,
        gui,
        WarpInstalledSurfaceKey::new(platform, channel, WarpTerminalSurface::Gui),
    )?;
    let tui = root.join("tui").join("warp.sqlite");
    if path_presence(&tui).suppresses_fallback() {
        push_warp_source(
            sources,
            data_root,
            spec,
            tui,
            WarpInstalledSurfaceKey::new(platform, channel, WarpTerminalSurface::Tui),
        )?;
    }
    Ok(())
}

fn push_warp_source(
    sources: &mut Vec<DiscoveredWarpSource>,
    data_root: Option<&Path>,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    surface_key: WarpInstalledSurfaceKey,
) -> Result<(), WarpDiscoveryUnavailable> {
    if sources.len() >= MAX_SOURCE_CANDIDATES_PER_PROVIDER || !encoded_path_within_limit(&path) {
        return Err(WarpDiscoveryUnavailable::SourceCandidateRejected { surface_key });
    }
    sources.push(DiscoveredWarpSource::new(
        safe_native_source(data_root, spec, path, WARP_FORMAT),
        surface_key,
    ));
    Ok(())
}

fn resolve_codebuddy(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if !supported_desktop_platform(context.platform()) {
        return DiscoveryReport::default();
    }
    let mut report = DiscoveryReport::default();
    let cli_root = match context.env("CODEBUDDY_CONFIG_DIR") {
        None => context.home().join(".codebuddy"),
        Some(value) if os_str_is_blank(value) => context.home().join(".codebuddy"),
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else if let Some(cwd) = context.cwd() {
                cwd.join(path)
            } else {
                report.issues.push(issue(
                    spec.provider,
                    Some(path),
                    DiscoveryIssueKind::SelectorUnreconstructible,
                    SELECTOR_MANUAL_REASON,
                ));
                return report;
            }
        }
    };
    push_source_candidate(
        &mut report.sources,
        safe_native_source(context.data_root(), spec, cli_root, CODEBUDDY_FORMAT),
    );

    let ide = match context.platform() {
        DiscoveryPlatform::Linux => context.home().join(".local/share/CodeBuddyExtension/Data"),
        DiscoveryPlatform::MacOS => context
            .home()
            .join("Library/Application Support/CodeBuddyExtension/Data"),
        DiscoveryPlatform::Windows => context.home().join("AppData/Local/CodeBuddyExtension/Data"),
        DiscoveryPlatform::OtherUnix => return report,
    };
    if path_presence(&ide).suppresses_fallback() {
        push_source_candidate(
            &mut report.sources,
            safe_native_source(context.data_root(), spec, ide, CODEBUDDY_FORMAT),
        );
    }
    report
}

fn resolve_zed(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if !supported_desktop_platform(context.platform()) {
        return DiscoveryReport::default();
    }
    if context
        .env("ZED_STATELESS")
        .is_some_and(|value| !value.is_empty())
    {
        return DiscoveryReport {
            sources: Vec::new(),
            issues: vec![issue(
                spec.provider,
                None,
                DiscoveryIssueKind::NoDiskHistory,
                ZED_STATELESS_REASON,
            )],
        };
    }

    let data_root = match context.platform() {
        DiscoveryPlatform::Linux => {
            let base = match context.env("FLATPAK_XDG_DATA_HOME").and_then(OsStr::to_str) {
                Some(value) => {
                    let path = PathBuf::from(value);
                    if !path.is_absolute() {
                        return selector_issue_report(spec, Some(path));
                    }
                    path
                }
                None => absolute_xdg_or_default(
                    context.env("XDG_DATA_HOME"),
                    context
                        .platform_dirs()
                        .data
                        .clone()
                        .unwrap_or_else(|| context.home().join(".local").join("share")),
                ),
            };
            Some(base.join("zed"))
        }
        DiscoveryPlatform::MacOS => context
            .platform_dirs()
            .data
            .clone()
            .map(|path| path.join("Zed"))
            .or_else(|| Some(context.home().join("Library/Application Support/Zed"))),
        DiscoveryPlatform::Windows => context
            .platform_dirs()
            .local_data
            .as_ref()
            .map(|path| path.join("Zed")),
        DiscoveryPlatform::OtherUnix => None,
    };
    let mut report = DiscoveryReport::default();
    if let Some(root) = data_root {
        let path = root.join("threads").join("threads.db");
        push_source_candidate(
            &mut report.sources,
            safe_native_source(context.data_root(), spec, path, ZED_FORMAT),
        );
    }
    report
}

fn resolve_copilot(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if !supported_desktop_platform(context.platform()) {
        return DiscoveryReport::default();
    }
    let root = match context.env("COPILOT_HOME") {
        None => context.home().join(".copilot"),
        Some(value) if value.is_empty() => context.home().join(".copilot"),
        Some(value) => {
            let path = PathBuf::from(value);
            if !path_is_absolute_for_platform(&path, context.platform()) {
                return selector_issue_report(spec, Some(path));
            }
            path
        }
    };
    let path = root.join("session-state");
    DiscoveryReport {
        sources: vec![safe_native_source(
            context.data_root(),
            spec,
            path,
            COPILOT_FORMAT,
        )],
        issues: Vec::new(),
    }
}

fn resolve_trae(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let root = match context.platform() {
        DiscoveryPlatform::Linux => {
            let base = absolute_xdg_or_default(
                context.env("XDG_CONFIG_HOME"),
                context
                    .platform_dirs()
                    .config
                    .clone()
                    .unwrap_or_else(|| context.home().join(".config")),
            );
            Some(base.join("Trae"))
        }
        DiscoveryPlatform::MacOS => context
            .platform_dirs()
            .data
            .clone()
            .map(|path| path.join("Trae"))
            .or_else(|| Some(context.home().join("Library/Application Support/Trae"))),
        DiscoveryPlatform::Windows => context
            .platform_dirs()
            .data
            .clone()
            .or_else(|| {
                context.env("APPDATA").and_then(|value| {
                    let path = PathBuf::from(value);
                    path_is_absolute_for_platform(&path, DiscoveryPlatform::Windows).then_some(path)
                })
            })
            .map(|path| path.join("Trae")),
        DiscoveryPlatform::OtherUnix => None,
    };
    let mut report = DiscoveryReport::default();
    if let Some(root) = root {
        push_source_candidate(
            &mut report.sources,
            safe_native_source(
                context.data_root(),
                spec,
                root.join("ModularData")
                    .join("ai-agent")
                    .join("database.db"),
                TRAE_FORMAT,
            ),
        );
    }
    report
}

fn resolve_antigravity(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if !supported_desktop_platform(context.platform()) {
        return DiscoveryReport::default();
    }
    let mut report = DiscoveryReport::default();
    for product in ["antigravity-cli", "antigravity-ide"] {
        let root = context.home().join(".gemini").join(product).join("brain");
        push_source_candidate(
            &mut report.sources,
            exact_tree_source(spec, root, ANTIGRAVITY_FORMAT, ExactTree::Antigravity),
        );
    }
    report
}

fn resolve_windsurf(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if !supported_desktop_platform(context.platform()) {
        return DiscoveryReport::default();
    }
    let root = context.home().join(".windsurf").join("transcripts");
    DiscoveryReport {
        sources: vec![exact_tree_source(
            spec,
            root,
            WINDSURF_FORMAT,
            ExactTree::Windsurf,
        )],
        issues: Vec::new(),
    }
}

#[derive(Debug, Clone, Copy)]
enum ExactTree {
    Antigravity,
    Windsurf,
}

fn exact_tree_source(
    spec: &ProviderSourceSpec,
    path: PathBuf,
    format: &'static str,
    tree: ExactTree,
) -> ProviderSource {
    let (exists, status, reason) = match path_presence(&path) {
        PathPresence::Missing => (
            false,
            ProviderSourceStatus::Missing,
            spec.unsupported_reason,
        ),
        PathPresence::Unknown(_) => (
            true,
            ProviderSourceStatus::Unknown,
            Some("the fixed provider transcript root could not be inspected"),
        ),
        PathPresence::Unsupported => (
            true,
            ProviderSourceStatus::Unsupported,
            Some(UNSAFE_SOURCE_REASON),
        ),
        PathPresence::Present if !ordinary_directory(&path) => (
            true,
            ProviderSourceStatus::Unknown,
            Some(UNSAFE_SOURCE_REASON),
        ),
        PathPresence::Present => match exact_tree_has_history(&path, tree) {
            Ok(true) => (
                true,
                ProviderSourceStatus::Available,
                spec.unsupported_reason,
            ),
            Ok(false) => (
                true,
                ProviderSourceStatus::Empty,
                Some(match tree {
                    ExactTree::Antigravity => {
                        "path exists but no official Antigravity transcript.jsonl leaf was found"
                    }
                    ExactTree::Windsurf => {
                        "path exists but no direct Windsurf trajectory JSONL file was found"
                    }
                }),
            ),
            Err(SelectorReadError::DirectoryLimit) => (
                true,
                ProviderSourceStatus::Unknown,
                Some("the fixed provider transcript root exceeded its direct-entry limit"),
            ),
            Err(_) => (
                true,
                ProviderSourceStatus::Unknown,
                Some("the fixed provider transcript root could not be inspected safely"),
            ),
        },
    };
    ProviderSource {
        provider: spec.provider,
        path,
        exists,
        source_format: format,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: spec.import_support,
        catalog_support: spec.catalog_support,
        status,
        unsupported_reason: reason,
    }
}

fn exact_tree_has_history(root: &Path, tree: ExactTree) -> Result<bool, SelectorReadError> {
    for candidate in direct_entries(root)? {
        let leaf = match tree {
            ExactTree::Antigravity => candidate
                .join(".system_generated")
                .join("logs")
                .join("transcript.jsonl"),
            ExactTree::Windsurf => {
                if candidate.extension().and_then(OsStr::to_str) != Some("jsonl") {
                    continue;
                }
                candidate
            }
        };
        if ordinary_file(&leaf) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn safe_native_source(
    data_root: Option<&Path>,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    format: &'static str,
) -> ProviderSource {
    match path_presence(&path) {
        PathPresence::Missing | PathPresence::Present => {
            return source_from_parts_with_data_root(
                data_root,
                spec,
                path,
                format,
                ProviderSourceKind::NativeHistory,
            );
        }
        PathPresence::Unsupported => {
            return ProviderSource {
                provider: spec.provider,
                path,
                exists: true,
                source_format: format,
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: spec.import_support,
                catalog_support: spec.catalog_support,
                status: ProviderSourceStatus::Unsupported,
                unsupported_reason: Some(UNSAFE_SOURCE_REASON),
            };
        }
        PathPresence::Unknown(_) => {}
    }
    ProviderSource {
        provider: spec.provider,
        path,
        exists: true,
        source_format: format,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: spec.import_support,
        catalog_support: spec.catalog_support,
        status: ProviderSourceStatus::Unknown,
        unsupported_reason: Some(UNSAFE_SOURCE_REASON),
    }
}

fn absolute_xdg_or_default(value: Option<&OsStr>, default: PathBuf) -> PathBuf {
    let Some(value) = value else {
        return default;
    };
    let path = PathBuf::from(value);
    if !value.is_empty() && path.is_absolute() {
        path
    } else {
        default
    }
}

fn supported_desktop_platform(platform: DiscoveryPlatform) -> bool {
    matches!(
        platform,
        DiscoveryPlatform::Linux | DiscoveryPlatform::MacOS | DiscoveryPlatform::Windows
    )
}

fn select_existing_precedence(
    preferred: PathBuf,
    fallback: PathBuf,
    include_missing_preferred: bool,
) -> Option<PathBuf> {
    match path_presence(&preferred) {
        PathPresence::Present | PathPresence::Unsupported | PathPresence::Unknown(_) => {
            Some(preferred)
        }
        PathPresence::Missing => match path_presence(&fallback) {
            PathPresence::Present | PathPresence::Unsupported | PathPresence::Unknown(_) => {
                Some(fallback)
            }
            PathPresence::Missing if include_missing_preferred => Some(preferred),
            PathPresence::Missing => None,
        },
    }
}

fn os_str_is_blank(value: &OsStr) -> bool {
    value.to_str().is_some_and(|value| value.trim().is_empty())
}

fn path_is_absolute_for_platform(path: &Path, platform: DiscoveryPlatform) -> bool {
    if path.is_absolute() {
        return true;
    }
    if platform != DiscoveryPlatform::Windows {
        return false;
    }
    let Some(value) = path.to_str() else {
        return false;
    };
    let bytes = value.as_bytes();
    value.starts_with(r"\\")
        || value.starts_with("//")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
}

fn selector_issue_report(spec: &ProviderSourceSpec, path: Option<PathBuf>) -> DiscoveryReport {
    DiscoveryReport {
        sources: Vec::new(),
        issues: vec![issue(
            spec.provider,
            path,
            DiscoveryIssueKind::SelectorUnreconstructible,
            SELECTOR_MANUAL_REASON,
        )],
    }
}

#[cfg(test)]
#[path = "platform_tests.rs"]
mod tests;
