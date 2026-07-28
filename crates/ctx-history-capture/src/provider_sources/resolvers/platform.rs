use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;

use super::super::{
    context::{DiscoveryContext, DiscoveryPlatform},
    selectors::{
        direct_entries, ordinary_directory, ordinary_file, SelectorDocument, SelectorFormat,
        SelectorReadError, SelectorReader, MAX_SELECTOR_FILES_PER_PROVIDER,
    },
    types::{
        DiscoveryIssueKind, DiscoveryReport, ProviderSource, ProviderSourceKind,
        ProviderSourceSpec, ProviderSourceStatus,
    },
};
use super::{
    dedupe_report, issue, path_presence, push_source_candidate, select_current_or_legacy,
    source_from_parts, unsupported_source, PathPresence,
};

const KIRO_FORMAT: &str = "kiro_cli_sqlite";
const WARP_FORMAT: &str = "warp_sqlite";
const CODEBUDDY_FORMAT: &str = "codebuddy_history_json";
const LINGMA_FORMAT: &str = "lingma_sqlite";
const ZED_FORMAT: &str = "zed_threads_sqlite";
const COPILOT_FORMAT: &str = "copilot_cli_session_events_jsonl";
const ANTIGRAVITY_FORMAT: &str = "antigravity_cli_transcript_jsonl_tree";
const WINDSURF_FORMAT: &str = "windsurf_cascade_hook_transcript_jsonl_tree";

const SELECTOR_MANUAL_REASON: &str = "the provider selected a path whose location cannot be reconstructed safely; use an exact --path";
const UNSAFE_SOURCE_REASON: &str =
    "the selected provider history path contains a symlink or non-ordinary component";
const SELECTOR_READ_REASON: &str =
    "the provider selector could not be read safely within discovery limits; use an exact --path";
const ZED_STATELESS_REASON: &str =
    "Zed stateless mode selected an in-memory database, so there is no disk history root";

pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let report = match spec.provider {
        CaptureProvider::KiroCli => resolve_kiro(context, spec),
        CaptureProvider::Warp => resolve_warp(context, spec),
        CaptureProvider::CodeBuddy => resolve_codebuddy(context, spec),
        CaptureProvider::Lingma => resolve_lingma(context, spec),
        CaptureProvider::Zed => resolve_zed(context, spec),
        CaptureProvider::CopilotCli => resolve_copilot(context, spec),
        CaptureProvider::Trae => DiscoveryReport::default(),
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
            safe_native_source(spec, path, KIRO_FORMAT),
        );
    }
    report
}

fn resolve_warp(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    match context.platform() {
        DiscoveryPlatform::Linux => {
            let state = absolute_xdg_or_default(
                context.env("XDG_STATE_HOME"),
                context
                    .platform_dirs()
                    .state
                    .clone()
                    .unwrap_or_else(|| context.home().join(".local").join("state")),
            );
            add_warp_channel(&mut report, spec, state.join("warp-terminal"), true);
            add_warp_channel(
                &mut report,
                spec,
                state.join("warp-terminal-preview"),
                false,
            );
        }
        DiscoveryPlatform::MacOS => {
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
                add_warp_channel(&mut report, spec, root, true);
            }
            let preview = select_existing_precedence(
                group.join("dev.warp.Warp-Preview"),
                fallback.join("dev.warp.Warp-Preview"),
                false,
            );
            if let Some(root) = preview {
                add_warp_channel(&mut report, spec, root, false);
            }
        }
        DiscoveryPlatform::Windows => {
            if let Some(local_data) = &context.platform_dirs().local_data {
                let warp = local_data.join("warp");
                add_warp_channel(&mut report, spec, warp.join("Warp").join("data"), true);
                add_warp_channel(
                    &mut report,
                    spec,
                    warp.join("WarpPreview").join("data"),
                    false,
                );
            }
        }
        DiscoveryPlatform::OtherUnix => {}
    }
    report
}

fn add_warp_channel(
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    root: PathBuf,
    primary: bool,
) {
    let gui = root.join("warp.sqlite");
    let installed = primary
        || path_presence(&root).suppresses_fallback()
        || path_presence(&gui).suppresses_fallback();
    if !installed {
        return;
    }
    push_source_candidate(
        &mut report.sources,
        safe_native_source(spec, gui, WARP_FORMAT),
    );
    let tui = root.join("tui").join("warp.sqlite");
    if path_presence(&tui).suppresses_fallback() {
        push_source_candidate(
            &mut report.sources,
            safe_native_source(spec, tui, WARP_FORMAT),
        );
    }
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
        safe_native_source(spec, cli_root, CODEBUDDY_FORMAT),
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
            safe_native_source(spec, ide, CODEBUDDY_FORMAT),
        );
    }
    report
}

#[derive(Debug, Clone)]
enum LingmaRootChoice {
    Absent,
    Default,
    Selected(PathBuf),
    Unreconstructible(PathBuf),
}

fn resolve_lingma(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    if !supported_desktop_platform(context.platform()) {
        return DiscoveryReport::default();
    }
    let mut report = DiscoveryReport::default();
    let mut reader = SelectorReader::default();
    resolve_lingma_vscode(context, spec, &mut reader, &mut report);
    resolve_lingma_jetbrains(context, spec, &mut reader, &mut report);
    report
}

fn resolve_lingma_vscode(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    reader: &mut SelectorReader,
    report: &mut DiscoveryReport,
) {
    let default_db = context
        .home()
        .join(".lingma/vscode/sharedClientCache/cache/db/local.db");
    let user_roots = vscode_user_roots(context);
    let mut installed_settings_found = false;
    for user_root in user_roots {
        if matches!(path_presence(&user_root), PathPresence::Missing) {
            continue;
        }
        let base_settings = user_root.join("settings.json");
        let (base, base_allows_absent_profile_fallback) =
            read_vscode_lingma_choice(reader, &base_settings, context.platform(), report, spec);
        installed_settings_found |= base.is_some() || !base_allows_absent_profile_fallback;
        if base.is_some() {
            add_lingma_vscode_choice(report, spec, base.as_ref(), &default_db);
        }
        let profiles = user_root.join("profiles");
        let entries = match direct_entries(&profiles) {
            Ok(entries) => entries,
            Err(_) if path_presence(&profiles).suppresses_fallback() => {
                installed_settings_found = true;
                report.issues.push(issue(
                    spec.provider,
                    Some(profiles),
                    DiscoveryIssueKind::SelectorUnreconstructible,
                    SELECTOR_READ_REASON,
                ));
                continue;
            }
            Err(_) => continue,
        };
        for profile in entries {
            if reader.files_read() >= MAX_SELECTOR_FILES_PER_PROVIDER {
                report.issues.push(issue(
                    spec.provider,
                    Some(profiles.clone()),
                    DiscoveryIssueKind::SelectorUnreconstructible,
                    SELECTOR_READ_REASON,
                ));
                break;
            }
            let settings = profile.join("settings.json");
            if matches!(path_presence(&settings), PathPresence::Missing) {
                continue;
            }
            installed_settings_found = true;
            let (Some(profile_choice), _) =
                read_vscode_lingma_choice(reader, &settings, context.platform(), report, spec)
            else {
                continue;
            };
            let effective = match &profile_choice {
                LingmaRootChoice::Absent if base_allows_absent_profile_fallback => base.as_ref(),
                LingmaRootChoice::Absent => continue,
                selected => Some(selected),
            };
            add_lingma_vscode_choice(report, spec, effective, &default_db);
        }
    }

    if !installed_settings_found && path_presence(&default_db).suppresses_fallback() {
        push_source_candidate(
            &mut report.sources,
            safe_native_source(spec, default_db, LINGMA_FORMAT),
        );
    }
}

fn vscode_user_roots(context: &DiscoveryContext) -> Vec<PathBuf> {
    let base = match context.platform() {
        DiscoveryPlatform::Linux => context
            .platform_dirs()
            .config
            .clone()
            .unwrap_or_else(|| context.home().join(".config")),
        DiscoveryPlatform::MacOS => context.home().join("Library").join("Application Support"),
        DiscoveryPlatform::Windows => match &context.platform_dirs().config {
            Some(path) => path.clone(),
            None => return Vec::new(),
        },
        DiscoveryPlatform::OtherUnix => return Vec::new(),
    };
    vec![
        base.join("Code").join("User"),
        base.join("Code - Insiders").join("User"),
    ]
}

fn read_vscode_lingma_choice(
    reader: &mut SelectorReader,
    path: &Path,
    platform: DiscoveryPlatform,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
) -> (Option<LingmaRootChoice>, bool) {
    let document = match reader.read(path, SelectorFormat::Jsonc) {
        Ok(document) => document,
        Err(SelectorReadError::Unavailable)
            if matches!(path_presence(path), PathPresence::Missing) =>
        {
            return (None, true);
        }
        Err(_) => {
            report.issues.push(issue(
                spec.provider,
                Some(path.to_path_buf()),
                DiscoveryIssueKind::SelectorUnreconstructible,
                SELECTOR_READ_REASON,
            ));
            return (None, false);
        }
    };
    let SelectorDocument::Structured(value) = &document else {
        return (None, false);
    };
    let Some(settings) = value.as_object() else {
        return (Some(LingmaRootChoice::Absent), true);
    };
    let value = settings
        .get("QoderCN.LocalMachineStoragePath")
        .or_else(|| settings.get("Lingma.LocalMachineStoragePath"));
    (
        Some(match value {
            None => LingmaRootChoice::Absent,
            Some(value) if value.as_str().is_some_and(str::is_empty) => LingmaRootChoice::Default,
            Some(value) if value.as_str().is_none() => LingmaRootChoice::Default,
            Some(value) => {
                let Some(value) = value.as_str() else {
                    return (Some(LingmaRootChoice::Default), true);
                };
                let root = PathBuf::from(value);
                if path_is_local_absolute_for_platform(&root, platform) {
                    LingmaRootChoice::Selected(root)
                } else {
                    LingmaRootChoice::Unreconstructible(root)
                }
            }
        }),
        true,
    )
}

fn add_lingma_vscode_choice(
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    choice: Option<&LingmaRootChoice>,
    default_db: &Path,
) {
    let path = match choice.unwrap_or(&LingmaRootChoice::Default) {
        LingmaRootChoice::Absent | LingmaRootChoice::Default => default_db.to_path_buf(),
        LingmaRootChoice::Selected(root) => root.join("sharedClientCache/cache/db/local.db"),
        LingmaRootChoice::Unreconstructible(path) => {
            report.issues.push(issue(
                spec.provider,
                Some(path.clone()),
                DiscoveryIssueKind::SelectorUnreconstructible,
                SELECTOR_MANUAL_REASON,
            ));
            return;
        }
    };
    push_source_candidate(
        &mut report.sources,
        safe_native_source(spec, path, LINGMA_FORMAT),
    );
}

fn resolve_lingma_jetbrains(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    reader: &mut SelectorReader,
    report: &mut DiscoveryReport,
) {
    let config_root = match context.platform() {
        DiscoveryPlatform::Linux | DiscoveryPlatform::Windows => context
            .platform_dirs()
            .config
            .as_ref()
            .map(|path| path.join("JetBrains")),
        DiscoveryPlatform::MacOS => Some(
            context
                .home()
                .join("Library")
                .join("Application Support")
                .join("JetBrains"),
        ),
        DiscoveryPlatform::OtherUnix => None,
    };
    let mut settings_found = false;
    if let Some(config_root) = config_root {
        match direct_entries(&config_root) {
            Ok(entries) => {
                for product in entries {
                    if reader.files_read() >= MAX_SELECTOR_FILES_PER_PROVIDER {
                        report.issues.push(issue(
                            spec.provider,
                            Some(config_root.clone()),
                            DiscoveryIssueKind::SelectorUnreconstructible,
                            SELECTOR_READ_REASON,
                        ));
                        break;
                    }
                    let settings = product.join("options").join("cosy_setting.xml");
                    if matches!(path_presence(&settings), PathPresence::Missing) {
                        continue;
                    }
                    settings_found = true;
                    let Some(choice) = read_jetbrains_lingma_choice(
                        reader,
                        &settings,
                        context.platform(),
                        report,
                        spec,
                    ) else {
                        continue;
                    };
                    if let LingmaRootChoice::Unreconstructible(path) = &choice {
                        report.issues.push(issue(
                            spec.provider,
                            Some(path.clone()),
                            DiscoveryIssueKind::SelectorUnreconstructible,
                            SELECTOR_MANUAL_REASON,
                        ));
                    }
                    let Some(path) = jetbrains_lingma_db(context, Some(&choice)) else {
                        continue;
                    };
                    push_source_candidate(
                        &mut report.sources,
                        safe_native_source(spec, path, LINGMA_FORMAT),
                    );
                }
            }
            Err(_) if path_presence(&config_root).suppresses_fallback() => {
                settings_found = true;
                report.issues.push(issue(
                    spec.provider,
                    Some(config_root),
                    DiscoveryIssueKind::SelectorUnreconstructible,
                    SELECTOR_READ_REASON,
                ));
            }
            Err(_) => {}
        }
    }

    if !settings_found {
        let current = current_jetbrains_default(context.home());
        let legacy = legacy_jetbrains_default(context.home());
        let selected = if path_presence(&current).suppresses_fallback() {
            Some(current)
        } else if path_presence(&legacy).suppresses_fallback() {
            Some(legacy)
        } else {
            None
        };
        if let Some(path) = selected {
            push_source_candidate(
                &mut report.sources,
                safe_native_source(spec, path, LINGMA_FORMAT),
            );
        }
    }
}

fn read_jetbrains_lingma_choice(
    reader: &mut SelectorReader,
    path: &Path,
    platform: DiscoveryPlatform,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
) -> Option<LingmaRootChoice> {
    let document = match reader.read(path, SelectorFormat::Xml) {
        Ok(document) => document,
        Err(_) => {
            report.issues.push(issue(
                spec.provider,
                Some(path.to_path_buf()),
                DiscoveryIssueKind::SelectorUnreconstructible,
                SELECTOR_READ_REASON,
            ));
            return None;
        }
    };
    let xml = document.xml()?;
    let component_path = ["application", "component"];
    let component_names = xml.values(&component_path, Some("name"));
    if xml.values(&component_path, None).len() != 1 || component_names != ["CosySettings"] {
        return Some(LingmaRootChoice::Default);
    }
    let option_path = ["application", "component", "option"];
    let option_count = xml.values(&option_path, None).len();
    let names = xml.values(&option_path, Some("name"));
    let values = xml.values(&option_path, Some("value"));
    if names.len() != option_count || values.len() != option_count {
        return Some(LingmaRootChoice::Default);
    }
    let value = names
        .into_iter()
        .zip(values)
        .find_map(|(name, value)| (name == "localStoragePath").then_some(value));
    Some(match value {
        None | Some("") => LingmaRootChoice::Default,
        Some(value) => {
            let root = PathBuf::from(value);
            if path_is_local_absolute_for_platform(&root, platform) {
                LingmaRootChoice::Selected(root)
            } else {
                LingmaRootChoice::Unreconstructible(root)
            }
        }
    })
}

fn jetbrains_lingma_db(
    context: &DiscoveryContext,
    choice: Option<&LingmaRootChoice>,
) -> Option<PathBuf> {
    match choice.unwrap_or(&LingmaRootChoice::Default) {
        LingmaRootChoice::Absent | LingmaRootChoice::Default => {
            let current = current_jetbrains_default(context.home());
            let legacy = legacy_jetbrains_default(context.home());
            Some(select_current_or_legacy(current, legacy))
        }
        LingmaRootChoice::Selected(root) => {
            let current = root.join("qoder-cn/cache/db/local.db");
            let legacy = root.join("cache/db/local.db");
            Some(select_current_or_legacy(current, legacy))
        }
        LingmaRootChoice::Unreconstructible(_) => None,
    }
}

fn current_jetbrains_default(home: &Path) -> PathBuf {
    home.join(".qoder-cn/shared_client/cache/db/local.db")
}

fn legacy_jetbrains_default(home: &Path) -> PathBuf {
    home.join(".lingma/cache/db/local.db")
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
            safe_native_source(spec, path, ZED_FORMAT),
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
        sources: vec![safe_native_source(spec, path, COPILOT_FORMAT)],
        issues: Vec::new(),
    }
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
    spec: &ProviderSourceSpec,
    path: PathBuf,
    format: &'static str,
) -> ProviderSource {
    match path_presence(&path) {
        PathPresence::Missing | PathPresence::Present => {
            return source_from_parts(spec, path, format, ProviderSourceKind::NativeHistory);
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

fn path_is_local_absolute_for_platform(path: &Path, platform: DiscoveryPlatform) -> bool {
    if platform == DiscoveryPlatform::Windows {
        let value = path.to_string_lossy();
        if value.starts_with(r"\\") || value.starts_with("//") {
            return false;
        }
    }
    path_is_absolute_for_platform(path, platform)
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
