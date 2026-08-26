use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use quick_xml::{events::Event, Reader};
use serde_json::Value;

use super::super::{
    context::{DiscoveryContext, DiscoveryPlatform},
    probes::{has_openhands_v1_event_json, BoundedProbe},
    selectors::{
        direct_entries, direct_regular_files_matching, ordinary_directory, ordinary_file,
        ordinary_path, read_bounded_bytes, SelectorDocument, SelectorFormat, SelectorIncludeBudget,
        SelectorReadError, SelectorReader, MAX_FINITE_SELECTOR_ENTRIES, MAX_SELECTOR_FILE_BYTES,
    },
    types::{
        DiscoveryIssueKind, DiscoveryReport, ProviderSourceKind, ProviderSourceRouteProvenance,
        ProviderSourceSpec, ProviderSourceStatus,
    },
    StaticProviderProbeCatalog,
};
use super::{
    dedupe_report, issue, path_presence, push_source_candidate, select_current_or_legacy,
    source_from_parts, source_from_parts_with_data_root, unsupported_source, PathPresence,
};

mod openclaw;
mod validation;

use openclaw::resolve as resolve_openclaw;
pub(in crate::provider_sources) use openclaw::{
    openclaw_agent_ids_for_state_root, OpenClawConfigError,
};
use validation::{valid_hermes_profile_name, valid_uuid};

const OPENCLAW_UNSUPPORTED_REASON: &str =
    "OpenClaw openclaw-agent.sqlite does not satisfy the bounded current v17 schema and ownership contract";
const PATH_MANUAL_REASON: &str =
    "the selected provider path cannot be reconstructed safely; use an exact --path";
const SELECTOR_MANUAL_REASON: &str =
    "the bounded provider selector is invalid or unreadable; use an exact --path";
const SELECTOR_LIMIT_REASON: &str =
    "the finite provider registry exceeds discovery limits; use an exact --path for omitted roots";
const REMOTE_OPENHANDS_REASON: &str =
    "OpenHands selected remote event storage, so there is no local filesystem history root";
const NANOCLAW_SERVICE_REGISTRATION_REASON: &str =
    "the NanoClaw service registration is malformed, unsafe, or does not match the selected checkout; use an exact --path";
const NANOCLAW_SERVICE_REGISTRY_REASON: &str =
    "the official NanoClaw service registry directory is unsafe or unreadable; use an exact --path";

pub(super) fn resolve(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let report = match spec.provider {
        CaptureProvider::OpenClaw => resolve_openclaw(probes, context, spec),
        CaptureProvider::Hermes => resolve_hermes(probes, context, spec),
        CaptureProvider::NanoClaw => resolve_nanoclaw(probes, context, spec),
        CaptureProvider::AstrBot => resolve_astrbot(probes, context, spec),
        CaptureProvider::Shelley => resolve_shelley(probes, context, spec),
        CaptureProvider::OpenHands => resolve_openhands(probes, context, spec),
        _ => DiscoveryReport::default(),
    };
    dedupe_report(report)
}

fn supported_host(context: &DiscoveryContext) -> bool {
    !matches!(context.platform(), DiscoveryPlatform::OtherUnix)
}

fn supported_posix_host(context: &DiscoveryContext) -> bool {
    matches!(
        context.platform(),
        DiscoveryPlatform::Linux | DiscoveryPlatform::MacOS
    )
}

fn env_text<'a>(context: &'a DiscoveryContext, name: &str) -> Result<Option<&'a str>, ()> {
    match context.env(name) {
        Some(value) => value.to_str().map(Some).ok_or(()),
        None => Ok(None),
    }
}

fn absolute_from_cwd(context: &DiscoveryContext, path: PathBuf) -> Result<PathBuf, ()> {
    if path.is_absolute() {
        Ok(path)
    } else {
        context.cwd().map(|cwd| cwd.join(path)).ok_or(())
    }
}

fn expand_leading_tilde(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return home.join(rest);
    }
    PathBuf::from(value)
}

fn issue_manual(report: &mut DiscoveryReport, provider: CaptureProvider, path: Option<PathBuf>) {
    report.issues.push(issue(
        provider,
        path,
        DiscoveryIssueKind::SelectorUnreconstructible,
        PATH_MANUAL_REASON,
    ));
}

fn issue_selector(report: &mut DiscoveryReport, provider: CaptureProvider) {
    report.issues.push(issue(
        provider,
        None,
        DiscoveryIssueKind::SelectorUnreconstructible,
        SELECTOR_MANUAL_REASON,
    ));
}

fn issue_limit(report: &mut DiscoveryReport, provider: CaptureProvider, path: PathBuf) {
    report.issues.push(issue(
        provider,
        Some(path),
        DiscoveryIssueKind::SelectorUnreconstructible,
        SELECTOR_LIMIT_REASON,
    ));
}

fn safe_existing_path(path: &Path) -> bool {
    match path_presence(path) {
        PathPresence::Missing => true,
        PathPresence::Present => ordinary_path(path),
        PathPresence::Unsupported | PathPresence::Unknown(_) => false,
    }
}

fn selected_path_is_safe(path: &Path, directory: bool) -> bool {
    match path_presence(path) {
        PathPresence::Missing => safe_existing_path(path),
        PathPresence::Present if directory => ordinary_directory(path),
        PathPresence::Present => ordinary_file(path),
        PathPresence::Unsupported | PathPresence::Unknown(_) => false,
    }
}

fn push_selected_source(
    probes: &StaticProviderProbeCatalog,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    format: &'static str,
) {
    let directory = matches!(
        format,
        "nanoclaw_project"
            | "openclaw_session_jsonl_tree"
            | "openhands_file_events"
            | super::super::OPENHANDS_CURRENT_CLI_SOURCE_FORMAT
    );
    if !selected_path_is_safe(&path, directory) {
        issue_manual(report, spec.provider, Some(path));
        return;
    }
    let limit_path = path.clone();
    let source = source_from_parts(
        probes,
        spec,
        path,
        format,
        ProviderSourceKind::NativeHistory,
    );
    if !push_source_candidate(&mut report.sources, source) {
        issue_limit(report, spec.provider, limit_path);
    }
}

fn push_unsupported_existing(
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    reason: &'static str,
    route_provenance: ProviderSourceRouteProvenance,
) {
    if !ordinary_file(&path) {
        return;
    }
    let mut source = unsupported_source(spec, path.clone(), reason);
    source.route_provenance = route_provenance;
    if !push_source_candidate(&mut report.sources, source) {
        issue_limit(report, spec.provider, path);
    }
}

fn resolve_hermes(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !supported_host(context) {
        return report;
    }

    let env_home = match env_text(context, "HERMES_HOME") {
        Err(()) => {
            issue_manual(&mut report, spec.provider, None);
            return report;
        }
        Ok(value) => value.map(str::trim).filter(|value| !value.is_empty()),
    };
    let selected_home = match env_home {
        Some(value) => match absolute_from_cwd(context, PathBuf::from(value)) {
            Ok(path) => path,
            Err(()) => {
                issue_manual(&mut report, spec.provider, None);
                return report;
            }
        },
        None => hermes_platform_root(context),
    };
    let (global_root, direct_profile) = if selected_home
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name.eq_ignore_ascii_case("profiles"))
    {
        (
            selected_home
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .unwrap_or_else(|| selected_home.clone()),
            Some(selected_home),
        )
    } else {
        (selected_home.clone(), None)
    };

    if !selected_path_is_safe(&global_root, true) {
        issue_manual(&mut report, spec.provider, Some(global_root));
        return report;
    }
    if let Some(profile_root) = direct_profile {
        push_selected_source(
            probes,
            &mut report,
            spec,
            profile_root.join("state.db"),
            "hermes_state_sqlite",
        );
        return report;
    }
    let Some(multiplex) = hermes_multiplex_enabled(&global_root, &mut report, spec.provider) else {
        return report;
    };
    if multiplex {
        push_selected_source(
            probes,
            &mut report,
            spec,
            global_root.join("state.db"),
            "hermes_state_sqlite",
        );
        let profiles_root = global_root.join("profiles");
        match path_presence(&profiles_root) {
            PathPresence::Present => match direct_entries(&profiles_root) {
                Ok(entries) => {
                    let mut admitted = 0usize;
                    for entry in entries {
                        let Some(name) = entry.file_name().and_then(OsStr::to_str) else {
                            continue;
                        };
                        if name == "default"
                            || !valid_hermes_profile_name(name)
                            || !ordinary_directory(&entry)
                        {
                            continue;
                        }
                        if admitted >= MAX_FINITE_SELECTOR_ENTRIES {
                            issue_limit(&mut report, spec.provider, profiles_root.clone());
                            break;
                        }
                        admitted += 1;
                        push_selected_source(
                            probes,
                            &mut report,
                            spec,
                            entry.join("state.db"),
                            "hermes_state_sqlite",
                        );
                    }
                }
                Err(_) => issue_selector(&mut report, spec.provider),
            },
            PathPresence::Unsupported | PathPresence::Unknown(_) => {
                issue_selector(&mut report, spec.provider)
            }
            PathPresence::Missing => {}
        }
        return report;
    }

    let active_profile = match hermes_sticky_profile(&global_root, &mut report, spec.provider) {
        Ok(profile) => profile,
        Err(()) => return report,
    };
    let active_root = active_profile.map_or_else(
        || global_root.clone(),
        |name| global_root.join("profiles").join(name),
    );
    push_selected_source(
        probes,
        &mut report,
        spec,
        active_root.join("state.db"),
        "hermes_state_sqlite",
    );
    report
}

fn hermes_platform_root(context: &DiscoveryContext) -> PathBuf {
    match context.platform() {
        DiscoveryPlatform::Windows => {
            let current = context
                .platform_dirs()
                .local_data
                .clone()
                .unwrap_or_else(|| context.home().join("AppData").join("Local"))
                .join("hermes");
            let legacy = context.home().join(".hermes");
            select_current_or_legacy(current, legacy)
        }
        _ => context.home().join(".hermes"),
    }
}

fn hermes_multiplex_enabled(
    root: &Path,
    report: &mut DiscoveryReport,
    provider: CaptureProvider,
) -> Option<bool> {
    let path = root.join("config.yaml");
    match path_presence(&path) {
        PathPresence::Missing => return Some(false),
        PathPresence::Present => {}
        PathPresence::Unsupported | PathPresence::Unknown(_) => {
            issue_selector(report, provider);
            return None;
        }
    }
    let document = match SelectorReader::default().read(&path, SelectorFormat::Yaml) {
        Ok(SelectorDocument::Structured(value)) => value,
        _ => {
            issue_selector(report, provider);
            return None;
        }
    };
    Some(
        document
            .get("gateway")
            .and_then(|gateway| gateway.get("multiplex_profiles"))
            .and_then(Value::as_bool)
            .or_else(|| document.get("multiplex_profiles").and_then(Value::as_bool))
            .unwrap_or(false),
    )
}

fn hermes_sticky_profile(
    root: &Path,
    report: &mut DiscoveryReport,
    provider: CaptureProvider,
) -> Result<Option<String>, ()> {
    let path = root.join("active_profile");
    match path_presence(&path) {
        PathPresence::Missing => return Ok(None),
        PathPresence::Present => {}
        PathPresence::Unsupported | PathPresence::Unknown(_) => {
            issue_selector(report, provider);
            return Err(());
        }
    }
    let bytes = match read_bounded_bytes(&path, 1024) {
        Ok(bytes) => bytes,
        Err(_) => {
            issue_selector(report, provider);
            return Err(());
        }
    };
    let Ok(name) = std::str::from_utf8(&bytes).map(str::trim) else {
        issue_selector(report, provider);
        return Err(());
    };
    if name == "default" || name.is_empty() {
        return Ok(None);
    }
    let profile = root.join("profiles").join(name);
    if valid_hermes_profile_name(name) && ordinary_directory(&profile) {
        Ok(Some(name.to_owned()))
    } else {
        issue_selector(report, provider);
        Err(())
    }
}

mod nanoclaw;
use nanoclaw::resolve_nanoclaw;
#[cfg(test)]
use nanoclaw::{nanoclaw_sha1_slug, nanoclaw_systemd_registry_dirs};
fn resolve_astrbot(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !supported_host(context) {
        return report;
    }

    let cwd = context.cwd();
    let cli_root = cwd.filter(|cwd| path_presence(&cwd.join(".astrbot")).suppresses_fallback());
    let selected_root = if let Some(root) = cli_root {
        Some(root.to_path_buf())
    } else {
        match env_text(context, "ASTRBOT_ROOT") {
            Err(()) => {
                issue_manual(&mut report, spec.provider, None);
                return report;
            }
            Ok(Some(value)) if !value.is_empty() => {
                match absolute_from_cwd(context, PathBuf::from(value)) {
                    Ok(path) => Some(path),
                    Err(()) => {
                        issue_manual(&mut report, spec.provider, None);
                        return report;
                    }
                }
            }
            _ => cwd
                .filter(|cwd| {
                    path_presence(&cwd.join("data").join("data_v4.db")).suppresses_fallback()
                })
                .map(Path::to_path_buf)
                .or_else(|| Some(context.home().join(".astrbot"))),
        }
    };
    if let Some(root) = selected_root {
        push_selected_source(
            probes,
            &mut report,
            spec,
            root.join("data").join("data_v4.db"),
            "astrbot_data_v4_sqlite",
        );
    }

    let instances = context.home().join(".astrbot_launcher").join("instances");
    match path_presence(&instances) {
        PathPresence::Present => match direct_entries(&instances) {
            Ok(entries) => {
                let mut admitted = 0usize;
                for entry in entries {
                    let Some(name) = entry.file_name().and_then(OsStr::to_str) else {
                        continue;
                    };
                    if !valid_uuid(name) || !ordinary_directory(&entry.join("core")) {
                        continue;
                    }
                    if admitted >= MAX_FINITE_SELECTOR_ENTRIES {
                        issue_limit(&mut report, spec.provider, instances.clone());
                        break;
                    }
                    admitted += 1;
                    push_selected_source(
                        probes,
                        &mut report,
                        spec,
                        entry.join("core").join("data").join("data_v4.db"),
                        "astrbot_data_v4_sqlite",
                    );
                }
            }
            Err(_) => issue_selector(&mut report, spec.provider),
        },
        PathPresence::Unsupported | PathPresence::Unknown(_) => {
            issue_selector(&mut report, spec.provider)
        }
        PathPresence::Missing => {}
    }
    report
}

fn resolve_shelley(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !supported_posix_host(context) {
        return report;
    }
    let Some(cwd) = context.cwd() else {
        issue_manual(&mut report, spec.provider, None);
        return report;
    };
    push_selected_source(
        probes,
        &mut report,
        spec,
        cwd.join("shelley.db"),
        "shelley_sqlite",
    );
    report
}

fn resolve_openhands(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !supported_host(context) {
        return report;
    }

    let backend = match openhands_backend(context) {
        Ok(backend) => backend,
        Err(()) => {
            issue_manual(&mut report, spec.provider, None);
            return report;
        }
    };
    let remote = backend.is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "aws" | "s3" | "gcp" | "google_cloud"
        )
    });
    let legacy_root = if remote {
        report.issues.push(issue(
            spec.provider,
            None,
            DiscoveryIssueKind::NoDiskHistory,
            REMOTE_OPENHANDS_REASON,
        ));
        None
    } else {
        let root = match openhands_v1_root(context) {
            Ok(path) => path,
            Err(()) => {
                issue_manual(&mut report, spec.provider, None);
                return report;
            }
        };
        let root = match openhands_user_partition(context, root) {
            Ok(path) => path,
            Err(()) => {
                issue_manual(&mut report, spec.provider, None);
                return report;
            }
        };
        Some(root)
    };

    let cli_root = match openhands_cli_root(context) {
        Ok(path) => path,
        Err(()) => {
            issue_manual(&mut report, spec.provider, None);
            return report;
        }
    };
    let cli_present = match path_presence(&cli_root) {
        PathPresence::Missing => false,
        PathPresence::Present if ordinary_directory(&cli_root) => {
            if openhands_cli_event_roots(&cli_root).is_err() {
                issue_selector(&mut report, spec.provider);
                return report;
            }
            true
        }
        PathPresence::Present | PathPresence::Unsupported | PathPresence::Unknown(_) => {
            issue_selector(&mut report, spec.provider);
            return report;
        }
    };

    match legacy_root {
        Some(root) if cli_root.starts_with(&root) => {
            match has_openhands_v1_event_json(&root, MAX_FINITE_SELECTOR_ENTRIES) {
                BoundedProbe::Found => {
                    // One umbrella route deterministically owns a mixed profile,
                    // so provider-native IDs cannot be imported twice.
                    push_selected_source(probes, &mut report, spec, root, "openhands_file_events");
                }
                BoundedProbe::NotFound if cli_present => push_selected_source(
                    probes,
                    &mut report,
                    spec,
                    cli_root.clone(),
                    super::super::OPENHANDS_CURRENT_CLI_SOURCE_FORMAT,
                ),
                BoundedProbe::NotFound => {
                    push_selected_source(probes, &mut report, spec, root, "openhands_file_events");
                }
                BoundedProbe::BudgetExhausted | BoundedProbe::IoError => {
                    issue_selector(&mut report, spec.provider);
                    push_selected_source(probes, &mut report, spec, root, "openhands_file_events");
                }
            }
            if !cli_present {
                // Discovery has no persistent memory of which overlapping
                // layout owned the prior generation. Keep both exact missing
                // route identities selected so either can age out through the
                // bounded automatic deletion protocol without an uncovered
                // base route.
                push_selected_source(
                    probes,
                    &mut report,
                    spec,
                    cli_root,
                    super::super::OPENHANDS_CURRENT_CLI_SOURCE_FORMAT,
                );
            }
        }
        Some(root) => {
            let disjoint_cli_root = !cli_root.starts_with(&root);
            push_selected_source(probes, &mut report, spec, root, "openhands_file_events");
            if cli_present || disjoint_cli_root {
                push_selected_source(
                    probes,
                    &mut report,
                    spec,
                    cli_root,
                    super::super::OPENHANDS_CURRENT_CLI_SOURCE_FORMAT,
                );
            }
        }
        None => push_selected_source(
            probes,
            &mut report,
            spec,
            cli_root,
            super::super::OPENHANDS_CURRENT_CLI_SOURCE_FORMAT,
        ),
    }
    report
}

fn openhands_backend(context: &DiscoveryContext) -> Result<Option<&str>, ()> {
    let primary = env_text(context, "SHARED_EVENT_STORAGE_PROVIDER")?
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if primary.is_some() {
        return Ok(primary);
    }
    Ok(env_text(context, "FILE_STORE")?
        .map(str::trim)
        .filter(|value| !value.is_empty()))
}

fn openhands_v1_root(context: &DiscoveryContext) -> Result<PathBuf, ()> {
    if let Some(value) = env_text(context, "OH_PERSISTENCE_DIR")? {
        if value.is_empty() {
            return context.cwd().map(Path::to_path_buf).ok_or(());
        }
        return absolute_from_cwd(context, PathBuf::from(value));
    }
    if let Some(value) = env_text(context, "FILE_STORE_PATH")? {
        if !value.is_empty() {
            return absolute_from_cwd(context, PathBuf::from(value));
        }
    }
    Ok(context.home().join(".openhands"))
}

fn openhands_user_partition(context: &DiscoveryContext, root: PathBuf) -> Result<PathBuf, ()> {
    let Some(value) = env_text(context, "OPENHANDS_USER_ID")? else {
        return Ok(root);
    };
    if value.is_empty() {
        return Ok(root);
    }
    let path = Path::new(value);
    if path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(());
    }
    Ok(root.join(path))
}

fn openhands_cli_root(context: &DiscoveryContext) -> Result<PathBuf, ()> {
    if let Some(value) = env_text(context, "OPENHANDS_CONVERSATIONS_DIR")? {
        return if value.is_empty() {
            context.cwd().map(Path::to_path_buf).ok_or(())
        } else {
            absolute_from_cwd(context, PathBuf::from(value))
        };
    }
    if let Some(value) = env_text(context, "OPENHANDS_PERSISTENCE_DIR")? {
        let persistence = if value.is_empty() {
            context.cwd().map(Path::to_path_buf).ok_or(())?
        } else {
            absolute_from_cwd(context, PathBuf::from(value))?
        };
        return Ok(persistence.join("conversations"));
    }
    Ok(context.home().join(".openhands").join("conversations"))
}

/// Resolves the official current CLI direct conversation root with the same
/// environment precedence used by automatic discovery.
///
/// `None` means the configured value cannot be represented safely or a
/// relative/empty override requires an unavailable process CWD.
pub fn resolve_openhands_conversations_root(context: &DiscoveryContext) -> Option<PathBuf> {
    openhands_cli_root(context).ok()
}

fn openhands_cli_event_roots(root: &Path) -> Result<Vec<PathBuf>, SelectorReadError> {
    match path_presence(root) {
        PathPresence::Missing => return Ok(Vec::new()),
        PathPresence::Present => {}
        PathPresence::Unsupported => return Err(SelectorReadError::UnsupportedRoot),
        PathPresence::Unknown(_) => return Err(SelectorReadError::Unavailable),
    }
    if !ordinary_directory(root) {
        return Err(SelectorReadError::Unavailable);
    }
    let conversations = direct_entries(root)?
        .into_iter()
        .filter(|path| ordinary_directory(path))
        .collect::<Vec<_>>();
    if conversations.len() > MAX_FINITE_SELECTOR_ENTRIES {
        return Err(SelectorReadError::EntryLimit);
    }
    let mut event_roots = Vec::new();
    for conversation in conversations {
        let events = conversation.join("events");
        match path_presence(&events) {
            PathPresence::Missing => continue,
            PathPresence::Present => {}
            PathPresence::Unsupported => return Err(SelectorReadError::UnsupportedRoot),
            PathPresence::Unknown(_) => return Err(SelectorReadError::Unavailable),
        }
        if direct_entries(&events)?.into_iter().any(|path| {
            ordinary_file(&path)
                && path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.starts_with("event-") && name.ends_with(".json"))
        }) {
            event_roots.push(events);
        }
    }
    Ok(event_roots)
}

#[cfg(test)]
#[path = "profile_project_tests.rs"]
mod tests;
