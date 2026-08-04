use std::{
    collections::{BTreeSet, HashSet},
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use quick_xml::{events::Event, Reader};
use serde_json::{Map, Value};

use super::super::{
    context::{DiscoveryContext, DiscoveryPlatform},
    selectors::{
        direct_entries, direct_regular_files_matching, ordinary_directory, ordinary_file,
        ordinary_path, read_bounded_bytes, SelectorDocument, SelectorFormat, SelectorIncludeBudget,
        SelectorReadError, SelectorReader, MAX_FINITE_SELECTOR_ENTRIES, MAX_SELECTOR_FILE_BYTES,
    },
    types::{DiscoveryIssueKind, DiscoveryReport, ProviderSourceKind, ProviderSourceSpec},
};
use super::{
    dedupe_report, issue, path_presence, push_source_candidate, select_current_or_legacy,
    source_from_parts, unsupported_source, PathPresence,
};

mod validation;
use validation::{valid_hermes_profile_name, valid_uuid};

const OPENCLAW_UNSUPPORTED_REASON: &str =
    "OpenClaw openclaw-agent.sqlite history is detected but unsupported";
const OPENHANDS_CLI_UNSUPPORTED_REASON: &str =
    "OpenHands CLI events/event-*.json history is detected but unsupported";
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

pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let report = match spec.provider {
        CaptureProvider::OpenClaw => resolve_openclaw(context, spec),
        CaptureProvider::Hermes => resolve_hermes(context, spec),
        CaptureProvider::NanoClaw => resolve_nanoclaw(context, spec),
        CaptureProvider::AstrBot => resolve_astrbot(context, spec),
        CaptureProvider::Shelley => resolve_shelley(context, spec),
        CaptureProvider::OpenHands => resolve_openhands(context, spec),
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
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    format: &'static str,
) {
    let directory = matches!(format, "nanoclaw_project" | "openhands_file_events");
    if !selected_path_is_safe(&path, directory) {
        issue_manual(report, spec.provider, Some(path));
        return;
    }
    let limit_path = path.clone();
    let source = source_from_parts(spec, path, format, ProviderSourceKind::NativeHistory);
    if !push_source_candidate(&mut report.sources, source) {
        issue_limit(report, spec.provider, limit_path);
    }
}

fn push_unsupported_existing(
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    reason: &'static str,
) {
    if !ordinary_file(&path) {
        return;
    }
    if !push_source_candidate(
        &mut report.sources,
        unsupported_source(spec, path.clone(), reason),
    ) {
        issue_limit(report, spec.provider, path);
    }
}

fn resolve_openclaw(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !supported_host(context) {
        return report;
    }

    let effective_home = match env_text(context, "OPENCLAW_HOME") {
        Err(()) => {
            issue_manual(&mut report, spec.provider, None);
            return report;
        }
        Ok(Some(value)) => {
            let value = value.trim();
            if value.is_empty() || matches!(value, "undefined" | "null") {
                context.home().to_path_buf()
            } else {
                match absolute_from_cwd(context, expand_leading_tilde(value, context.home())) {
                    Ok(path) => path,
                    Err(()) => {
                        issue_manual(&mut report, spec.provider, None);
                        return report;
                    }
                }
            }
        }
        Ok(None) => context.home().to_path_buf(),
    };

    let state_root = match env_text(context, "OPENCLAW_STATE_DIR") {
        Err(()) => {
            issue_manual(&mut report, spec.provider, None);
            return report;
        }
        Ok(Some(value)) if !value.trim().is_empty() => {
            match absolute_from_cwd(context, expand_leading_tilde(value.trim(), &effective_home)) {
                Ok(path) => path,
                Err(()) => {
                    issue_manual(&mut report, spec.provider, None);
                    return report;
                }
            }
        }
        _ => {
            let current = effective_home.join(".openclaw");
            let legacy = effective_home.join(".clawdbot");
            select_current_or_legacy(current, legacy)
        }
    };

    if !selected_path_is_safe(&state_root, true) {
        issue_manual(&mut report, spec.provider, Some(state_root));
        return report;
    }

    let config_path = ["openclaw.json", "clawdbot.json"]
        .into_iter()
        .map(|name| state_root.join(name))
        .find(|path| path_presence(path).suppresses_fallback());
    let (agent_ids, truncated) = match config_path {
        Some(path) => match read_openclaw_agent_ids(&path) {
            Ok(ids) => ids,
            Err(OpenClawConfigError::Limit) => {
                issue_limit(&mut report, spec.provider, path);
                return report;
            }
            Err(OpenClawConfigError::Invalid) => {
                issue_selector(&mut report, spec.provider);
                return report;
            }
        },
        None => (vec!["main".to_owned()], false),
    };
    if truncated {
        issue_limit(&mut report, spec.provider, state_root.join("openclaw.json"));
    }

    for agent_id in agent_ids {
        push_unsupported_existing(
            &mut report,
            spec,
            state_root
                .join("agents")
                .join(agent_id)
                .join("agent")
                .join("openclaw-agent.sqlite"),
            OPENCLAW_UNSUPPORTED_REASON,
        );
    }
    report
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenClawConfigError {
    Invalid,
    Limit,
}

fn read_openclaw_agent_ids(path: &Path) -> Result<(Vec<String>, bool), OpenClawConfigError> {
    let mut reader = SelectorReader::default();
    let document = reader
        .read(path, SelectorFormat::Json5)
        .map_err(map_openclaw_error)?;
    let SelectorDocument::Structured(value) = document else {
        return Err(OpenClawConfigError::Invalid);
    };
    let config_root = path.parent().ok_or(OpenClawConfigError::Invalid)?;
    let mut budget = SelectorIncludeBudget::default();
    let canonical_path = lexical_normalize(path);
    let mut visited = HashSet::from([canonical_path]);
    let value = resolve_openclaw_includes(
        value,
        path,
        config_root,
        &mut reader,
        &mut budget,
        &mut visited,
        0,
    )?;

    let agents = value
        .get("agents")
        .and_then(|value| value.get("list"))
        .and_then(Value::as_array);
    let bindings = value.get("bindings").and_then(Value::as_array);
    let configured_count = agents.map_or(0, Vec::len) + bindings.map_or(0, Vec::len);
    let truncated = configured_count > MAX_FINITE_SELECTOR_ENTRIES;

    let mut ids = BTreeSet::new();
    let mut examined = 0usize;
    if let Some(agents) = agents {
        for agent in agents {
            if examined >= MAX_FINITE_SELECTOR_ENTRIES {
                break;
            }
            examined += 1;
            if let Some(id) = agent.get("id").and_then(Value::as_str) {
                ids.insert(normalize_openclaw_agent_id(id)?);
            }
        }
    }
    if let Some(bindings) = bindings {
        for binding in bindings {
            if examined >= MAX_FINITE_SELECTOR_ENTRIES {
                break;
            }
            examined += 1;
            if let Some(id) = binding.get("agentId").and_then(Value::as_str) {
                ids.insert(normalize_openclaw_agent_id(id)?);
            }
        }
    }
    if ids.is_empty() {
        ids.insert("main".to_owned());
    }
    Ok((ids.into_iter().collect(), truncated))
}

fn map_openclaw_error(error: SelectorReadError) -> OpenClawConfigError {
    match error {
        SelectorReadError::FileLimit
        | SelectorReadError::NestingDepth
        | SelectorReadError::EntryLimit
        | SelectorReadError::DirectoryLimit
        | SelectorReadError::FileTooLarge => OpenClawConfigError::Limit,
        _ => OpenClawConfigError::Invalid,
    }
}

fn normalize_openclaw_agent_id(value: &str) -> Result<String, OpenClawConfigError> {
    if value.contains('$') {
        return Err(OpenClawConfigError::Invalid);
    }
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok("main".to_owned());
    }
    let mut normalized = String::new();
    let mut previous_dash = false;
    for character in trimmed.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
            normalized.push(character);
            previous_dash = false;
        } else if !previous_dash {
            normalized.push('-');
            previous_dash = true;
        }
        if normalized.len() >= 64 {
            break;
        }
    }
    let normalized = normalized.trim_matches('-');
    Ok(if normalized.is_empty() {
        "main".to_owned()
    } else {
        normalized.to_owned()
    })
}

fn lexical_normalize(path: impl AsRef<Path>) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.as_ref().components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() {
                    output.push(component.as_os_str());
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                output.push(component.as_os_str());
            }
        }
    }
    output
}

fn resolve_openclaw_includes(
    value: Value,
    current_path: &Path,
    config_root: &Path,
    reader: &mut SelectorReader,
    budget: &mut SelectorIncludeBudget,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) -> Result<Value, OpenClawConfigError> {
    match value {
        Value::Array(values) => values
            .into_iter()
            .map(|value| {
                resolve_openclaw_includes(
                    value,
                    current_path,
                    config_root,
                    reader,
                    budget,
                    visited,
                    depth,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(mut values) => {
            let included = values.remove("$include");
            let mut merged = Value::Object(Map::new());
            if let Some(included) = included {
                let include_paths = match included {
                    Value::String(path) => vec![path],
                    Value::Array(paths) => paths
                        .into_iter()
                        .map(|path| path.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                        .ok_or(OpenClawConfigError::Invalid)?,
                    _ => return Err(OpenClawConfigError::Invalid),
                };
                for include in include_paths {
                    budget
                        .admit(depth.saturating_add(1))
                        .map_err(map_openclaw_error)?;
                    let parent = current_path.parent().ok_or(OpenClawConfigError::Invalid)?;
                    let include_path = lexical_normalize(if Path::new(&include).is_absolute() {
                        PathBuf::from(include)
                    } else {
                        parent.join(include)
                    });
                    if !ordinary_file(&include_path) {
                        return Err(OpenClawConfigError::Invalid);
                    }
                    let canonical_root = lexical_normalize(config_root);
                    let canonical_include = include_path;
                    if !canonical_include.starts_with(&canonical_root)
                        || !visited.insert(canonical_include.clone())
                    {
                        return Err(OpenClawConfigError::Invalid);
                    }
                    let document = reader
                        .read(&canonical_include, SelectorFormat::Json5)
                        .map_err(map_openclaw_error)?;
                    let SelectorDocument::Structured(included_value) = document else {
                        return Err(OpenClawConfigError::Invalid);
                    };
                    let included_value = resolve_openclaw_includes(
                        included_value,
                        &canonical_include,
                        config_root,
                        reader,
                        budget,
                        visited,
                        depth.saturating_add(1),
                    )?;
                    visited.remove(&canonical_include);
                    deep_merge(&mut merged, included_value);
                }
            }

            let mut siblings = Map::new();
            for (key, value) in values {
                siblings.insert(
                    key,
                    resolve_openclaw_includes(
                        value,
                        current_path,
                        config_root,
                        reader,
                        budget,
                        visited,
                        depth,
                    )?,
                );
            }
            deep_merge(&mut merged, Value::Object(siblings));
            Ok(merged)
        }
        scalar => Ok(scalar),
    }
}

fn deep_merge(target: &mut Value, source: Value) {
    match (target, source) {
        (Value::Object(target), Value::Object(source)) => {
            for (key, value) in source {
                if let Some(existing) = target.get_mut(&key) {
                    deep_merge(existing, value);
                } else {
                    target.insert(key, value);
                }
            }
        }
        (Value::Array(target), Value::Array(mut source)) => target.append(&mut source),
        (target, source) => *target = source,
    }
}

fn resolve_hermes(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
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

fn resolve_nanoclaw(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !supported_posix_host(context) {
        return report;
    }

    if let Some(cwd) = context.cwd().and_then(canonical_nanoclaw_project_store) {
        push_selected_source(&mut report, spec, cwd, "nanoclaw_project");
    }

    for registration in nanoclaw_service_registrations(context) {
        match registration {
            Ok(project) => push_selected_source(&mut report, spec, project, "nanoclaw_project"),
            Err(NanoClawServiceRegistrationError::Registration(path)) => {
                report.issues.push(issue(
                    spec.provider,
                    Some(path),
                    DiscoveryIssueKind::SelectorUnreconstructible,
                    NANOCLAW_SERVICE_REGISTRATION_REASON,
                ));
            }
            Err(NanoClawServiceRegistrationError::Registry(path)) => {
                report.issues.push(issue(
                    spec.provider,
                    Some(path),
                    DiscoveryIssueKind::SelectorUnreconstructible,
                    NANOCLAW_SERVICE_REGISTRY_REASON,
                ));
            }
            Err(NanoClawServiceRegistrationError::RegistryLimit(path)) => {
                issue_limit(&mut report, spec.provider, path);
            }
        }
    }
    report
}

fn nanoclaw_supported_project_store(project: &Path) -> bool {
    ordinary_file(&project.join("data").join("v2.db"))
        && ordinary_directory(&project.join("data").join("v2-sessions"))
}

fn canonical_nanoclaw_project_store(project: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(project).ok()?;
    (selected_path_is_safe(&canonical, true) && nanoclaw_supported_project_store(&canonical))
        .then_some(canonical)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NanoClawServiceRegistrationError {
    Registration(PathBuf),
    Registry(PathBuf),
    RegistryLimit(PathBuf),
}

type NanoClawServiceRegistration = Result<PathBuf, NanoClawServiceRegistrationError>;

fn nanoclaw_service_registrations(context: &DiscoveryContext) -> Vec<NanoClawServiceRegistration> {
    match context.platform() {
        DiscoveryPlatform::MacOS => {
            nanoclaw_launchd_registrations(&context.home().join("Library").join("LaunchAgents"))
        }
        DiscoveryPlatform::Linux => {
            let mut registrations = Vec::new();
            for registry_dir in nanoclaw_systemd_registry_dirs(context) {
                registrations.extend(nanoclaw_systemd_registrations(&registry_dir));
            }
            registrations
        }
        DiscoveryPlatform::Windows | DiscoveryPlatform::OtherUnix => Vec::new(),
    }
}

fn nanoclaw_systemd_registry_dirs(context: &DiscoveryContext) -> Vec<PathBuf> {
    let mut registry_dirs = vec![context.home().join(".config/systemd/user")];
    if context.effective_uid() == Some(0) {
        registry_dirs.push(PathBuf::from("/etc/systemd/system"));
    }
    registry_dirs
}

fn nanoclaw_launchd_registrations(registry_dir: &Path) -> Vec<NanoClawServiceRegistration> {
    let mut registrations = Vec::new();
    let entries = match nanoclaw_registration_entries(registry_dir, nanoclaw_launchd_plist_name) {
        Ok(entries) => entries,
        Err(error) => return vec![Err(error)],
    };
    for path in entries {
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if !nanoclaw_launchd_plist_name(name) {
            continue;
        }
        registrations.push(
            parse_nanoclaw_launchd_plist(&path)
                .and_then(|project| validate_nanoclaw_registered_project(&path, project))
                .map_err(NanoClawServiceRegistrationError::Registration),
        );
    }
    registrations
}

fn nanoclaw_systemd_registrations(registry_dir: &Path) -> Vec<NanoClawServiceRegistration> {
    let mut registrations = Vec::new();
    let entries = match nanoclaw_registration_entries(registry_dir, nanoclaw_systemd_unit_name) {
        Ok(entries) => entries,
        Err(error) => return vec![Err(error)],
    };
    for path in entries {
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if !nanoclaw_systemd_unit_name(name) {
            continue;
        }
        registrations.push(
            parse_nanoclaw_systemd_unit(&path)
                .and_then(|project| validate_nanoclaw_registered_project(&path, project))
                .map_err(NanoClawServiceRegistrationError::Registration),
        );
    }
    registrations
}

fn nanoclaw_registration_entries(
    registry_dir: &Path,
    official_name: fn(&str) -> bool,
) -> Result<Vec<PathBuf>, NanoClawServiceRegistrationError> {
    match path_presence(registry_dir) {
        PathPresence::Missing => Ok(Vec::new()),
        PathPresence::Present if ordinary_directory(registry_dir) => {
            direct_regular_files_matching(registry_dir, |name| {
                name.to_str().is_some_and(official_name)
            })
            .map_err(|error| match error {
                SelectorReadError::DirectoryLimit => {
                    NanoClawServiceRegistrationError::RegistryLimit(registry_dir.to_path_buf())
                }
                _ => NanoClawServiceRegistrationError::Registry(registry_dir.to_path_buf()),
            })
        }
        _ => Err(NanoClawServiceRegistrationError::Registry(
            registry_dir.to_path_buf(),
        )),
    }
}

fn nanoclaw_launchd_plist_name(name: &str) -> bool {
    name.strip_prefix("com.nanoclaw-v2-")
        .and_then(|rest| rest.strip_suffix(".plist"))
        .is_some_and(nanoclaw_slug)
}

fn nanoclaw_systemd_unit_name(name: &str) -> bool {
    name.strip_prefix("nanoclaw-v2-")
        .and_then(|rest| rest.strip_suffix(".service"))
        .is_some_and(nanoclaw_slug)
}

fn nanoclaw_slug(value: &str) -> bool {
    value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_nanoclaw_systemd_unit(path: &Path) -> Result<PathBuf, PathBuf> {
    let bytes =
        read_bounded_bytes(path, MAX_SELECTOR_FILE_BYTES).map_err(|_| path.to_path_buf())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| path.to_path_buf())?;
    let mut section = "";
    let mut service_sections = 0_usize;
    let mut working_directory = None::<String>;
    let mut exec_start = None::<String>;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len().saturating_sub(1)];
            if section == "Service" {
                service_sections = service_sections.saturating_add(1);
                if service_sections > 1 {
                    return Err(path.to_path_buf());
                }
            }
            continue;
        }
        if section != "Service" {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(path.to_path_buf());
        };
        let value = value.trim();
        match key.trim() {
            "WorkingDirectory" => set_unique_string(&mut working_directory, value, path)?,
            "ExecStart" => set_unique_string(&mut exec_start, value, path)?,
            _ => {}
        }
    }

    if service_sections != 1 {
        return Err(path.to_path_buf());
    }
    let working_directory = working_directory.ok_or_else(|| path.to_path_buf())?;
    let project = parse_nanoclaw_systemd_working_directory(&working_directory)
        .map_err(|_| path.to_path_buf())?;
    reject_untrusted_service_path(&project).map_err(|_| path.to_path_buf())?;
    let exec_start = exec_start.ok_or_else(|| path.to_path_buf())?;
    let (node, script) =
        parse_nanoclaw_systemd_exec_start(&exec_start, &project).map_err(|_| path.to_path_buf())?;
    reject_untrusted_service_path(&node).map_err(|_| path.to_path_buf())?;
    if script != project.join("dist/index.js") {
        return Err(path.to_path_buf());
    }
    Ok(project)
}

fn parse_nanoclaw_systemd_working_directory(value: &str) -> Result<PathBuf, ()> {
    // NanoClaw currently interpolates WorkingDirectory verbatim. Decode a quoted or
    // escaped systemd item when one is present, while retaining that exact raw form.
    if value.starts_with(['\'', '"']) || value.contains('\\') {
        let mut words = parse_systemd_words(value)?;
        if words.len() != 1 {
            return Err(());
        }
        Ok(PathBuf::from(words.pop().ok_or(())?))
    } else {
        Ok(PathBuf::from(value))
    }
}

fn parse_nanoclaw_systemd_exec_start(
    value: &str,
    project: &Path,
) -> Result<(PathBuf, PathBuf), ()> {
    let expected_script = project.join("dist/index.js");
    if let Ok(mut words) = parse_systemd_words(value) {
        if words.len() == 2 && Path::new(&words[1]) == expected_script {
            let script = PathBuf::from(words.pop().ok_or(())?);
            let node = PathBuf::from(words.pop().ok_or(())?);
            return Ok((node, script));
        }
    }

    // Current upstream writes exactly `${nodePath} ${projectRoot}/dist/index.js`.
    // Match that suffix directly so an unquoted checkout path containing spaces is
    // still reconstructible without interpreting a general command language.
    let script = expected_script.to_str().ok_or(())?;
    let node = value
        .strip_suffix(script)
        .and_then(|prefix| prefix.strip_suffix(' '))
        .ok_or(())?;
    if node.is_empty() || node.chars().any(char::is_whitespace) {
        return Err(());
    }
    Ok((PathBuf::from(node), expected_script))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SystemdQuote {
    Single,
    Double,
}

fn parse_systemd_words(value: &str) -> Result<Vec<String>, ()> {
    let mut characters = value.chars().peekable();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut word_started = false;
    let mut quote = None::<SystemdQuote>;
    let mut just_closed_quote = false;

    while let Some(character) = characters.next() {
        if let Some(active_quote) = quote {
            if matches!(
                (active_quote, character),
                (SystemdQuote::Single, '\'') | (SystemdQuote::Double, '"')
            ) {
                quote = None;
                just_closed_quote = true;
            } else if character == '\\' {
                word.push(parse_systemd_escape(&mut characters)?);
            } else {
                word.push(character);
            }
            continue;
        }

        if character.is_whitespace() {
            if word_started {
                words.push(std::mem::take(&mut word));
                word_started = false;
            }
            just_closed_quote = false;
            continue;
        }
        if just_closed_quote {
            return Err(());
        }
        match character {
            '\'' => {
                if word_started {
                    return Err(());
                }
                quote = Some(SystemdQuote::Single);
                word_started = true;
            }
            '"' => {
                if word_started {
                    return Err(());
                }
                quote = Some(SystemdQuote::Double);
                word_started = true;
            }
            '\\' => {
                word.push(parse_systemd_escape(&mut characters)?);
                word_started = true;
            }
            _ => {
                word.push(character);
                word_started = true;
            }
        }
    }

    if quote.is_some() {
        return Err(());
    }
    if word_started {
        words.push(word);
    }
    Ok(words)
}

fn parse_systemd_escape<I>(characters: &mut std::iter::Peekable<I>) -> Result<char, ()>
where
    I: Iterator<Item = char>,
{
    let escaped = characters.next().ok_or(())?;
    match escaped {
        'a' => Ok('\u{0007}'),
        'b' => Ok('\u{0008}'),
        'f' => Ok('\u{000c}'),
        'n' => Ok('\n'),
        'r' => Ok('\r'),
        't' => Ok('\t'),
        'v' => Ok('\u{000b}'),
        '\\' => Ok('\\'),
        '"' => Ok('"'),
        '\'' => Ok('\''),
        's' => Ok(' '),
        'x' => parse_systemd_numeric_escape(characters, 2, 16),
        'u' => parse_systemd_numeric_escape(characters, 4, 16),
        'U' => parse_systemd_numeric_escape(characters, 8, 16),
        first @ '0'..='7' => {
            let mut value = first.to_digit(8).ok_or(())?;
            for _ in 0..2 {
                value = value
                    .checked_mul(8)
                    .and_then(|value| {
                        characters
                            .next()
                            .and_then(|character| character.to_digit(8))
                            .and_then(|digit| value.checked_add(digit))
                    })
                    .ok_or(())?;
            }
            char::from_u32(value).ok_or(())
        }
        _ => Err(()),
    }
}

fn parse_systemd_numeric_escape<I>(
    characters: &mut std::iter::Peekable<I>,
    digits: usize,
    radix: u32,
) -> Result<char, ()>
where
    I: Iterator<Item = char>,
{
    let mut value = 0_u32;
    for _ in 0..digits {
        value = value
            .checked_mul(radix)
            .and_then(|value| {
                characters
                    .next()
                    .and_then(|character| character.to_digit(radix))
                    .and_then(|digit| value.checked_add(digit))
            })
            .ok_or(())?;
    }
    char::from_u32(value).ok_or(())
}

fn parse_nanoclaw_launchd_plist(path: &Path) -> Result<PathBuf, PathBuf> {
    let bytes =
        read_bounded_bytes(path, MAX_SELECTOR_FILE_BYTES).map_err(|_| path.to_path_buf())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| path.to_path_buf())?;
    let values = parse_launchd_plist_values(text).map_err(|_| path.to_path_buf())?;
    let label = values.label.ok_or_else(|| path.to_path_buf())?;
    let expected_label = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| path.to_path_buf())?;
    if label != expected_label || !label.starts_with("com.nanoclaw-v2-") {
        return Err(path.to_path_buf());
    }
    let project = PathBuf::from(values.working_directory.ok_or_else(|| path.to_path_buf())?);
    reject_untrusted_service_path(&project).map_err(|_| path.to_path_buf())?;
    if values.program_arguments.len() != 2 {
        return Err(path.to_path_buf());
    }
    reject_untrusted_service_path(Path::new(&values.program_arguments[0]))
        .map_err(|_| path.to_path_buf())?;
    if Path::new(&values.program_arguments[1]) != project.join("dist/index.js") {
        return Err(path.to_path_buf());
    }
    Ok(project)
}

struct NanoClawLaunchdValues {
    label: Option<String>,
    working_directory: Option<String>,
    program_arguments: Vec<String>,
}

enum NanoClawPlistValue {
    String(String),
    Array(Vec<NanoClawPlistValue>),
    Dict(Vec<(String, NanoClawPlistValue)>),
    Boolean,
}

const NANOCLAW_PLIST_MAX_DEPTH: usize = 32;

fn parse_launchd_plist_values(text: &str) -> Result<NanoClawLaunchdValues, ()> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut declaration_seen = false;
    let mut doctype_seen = false;
    loop {
        match next_nanoclaw_plist_event(&mut reader)? {
            Event::Decl(_) if !declaration_seen && !doctype_seen => declaration_seen = true,
            Event::DocType(_) if !doctype_seen => doctype_seen = true,
            Event::Start(element) if element.name().as_ref() == b"plist" => {
                if !nanoclaw_plist_root_attributes(&element) {
                    return Err(());
                }
                let mut budget = 0_usize;
                let root = parse_nanoclaw_plist_value(&mut reader, 0, &mut budget)?;
                if !matches!(
                    next_nanoclaw_plist_event(&mut reader)?,
                    Event::End(element) if element.name().as_ref() == b"plist"
                ) {
                    return Err(());
                }
                if !matches!(next_nanoclaw_plist_event(&mut reader)?, Event::Eof) {
                    return Err(());
                }
                return nanoclaw_launchd_values_from_root(root);
            }
            Event::Eof => return Err(()),
            _ => return Err(()),
        }
    }
}

fn next_nanoclaw_plist_event<'a>(reader: &mut Reader<&'a [u8]>) -> Result<Event<'a>, ()> {
    loop {
        match reader.read_event().map_err(|_| ())? {
            Event::Text(text)
                if text
                    .decode()
                    .map_err(|_| ())?
                    .chars()
                    .all(|character| matches!(character, ' ' | '\t' | '\r' | '\n')) => {}
            Event::Comment(_) => {}
            event => return Ok(event),
        }
    }
}

fn nanoclaw_plist_root_attributes(element: &quick_xml::events::BytesStart<'_>) -> bool {
    let mut attributes = element.attributes();
    matches!(
        (attributes.next(), attributes.next()),
        (Some(Ok(attribute)), None)
            if attribute.key.as_ref() == b"version" && attribute.value.as_ref() == b"1.0"
    )
}

fn nanoclaw_plist_no_attributes(element: &quick_xml::events::BytesStart<'_>) -> bool {
    element.attributes().next().is_none()
}

fn parse_nanoclaw_plist_value(
    reader: &mut Reader<&[u8]>,
    depth: usize,
    budget: &mut usize,
) -> Result<NanoClawPlistValue, ()> {
    if depth > NANOCLAW_PLIST_MAX_DEPTH {
        return Err(());
    }
    let event = next_nanoclaw_plist_event(reader)?;
    match event {
        Event::Start(element) => {
            if !nanoclaw_plist_no_attributes(&element) {
                return Err(());
            }
            match element.name().as_ref() {
                b"string" => {
                    parse_nanoclaw_plist_text(reader, b"string").map(NanoClawPlistValue::String)
                }
                b"array" => parse_nanoclaw_plist_array(reader, depth + 1, budget),
                b"dict" => parse_nanoclaw_plist_dict(reader, depth + 1, budget),
                _ => Err(()),
            }
        }
        Event::Empty(element)
            if nanoclaw_plist_no_attributes(&element)
                && matches!(element.name().as_ref(), b"true" | b"false") =>
        {
            Ok(NanoClawPlistValue::Boolean)
        }
        _ => Err(()),
    }
}

fn parse_nanoclaw_plist_text(reader: &mut Reader<&[u8]>, end_name: &[u8]) -> Result<String, ()> {
    let mut value = String::new();
    loop {
        match reader.read_event().map_err(|_| ())? {
            Event::Text(text) => {
                value.push_str(&text.decode().map_err(|_| ())?);
            }
            Event::GeneralRef(reference) => {
                append_nanoclaw_plist_reference(&mut value, &reference)?;
            }
            Event::End(element) if element.name().as_ref() == end_name => return Ok(value),
            _ => return Err(()),
        }
    }
}

fn append_nanoclaw_plist_reference(
    value: &mut String,
    reference: &quick_xml::events::BytesRef<'_>,
) -> Result<(), ()> {
    if let Some(character) = reference.resolve_char_ref().map_err(|_| ())? {
        value.push(character);
        return Ok(());
    }
    let name = reference.decode().map_err(|_| ())?;
    let resolved = quick_xml::escape::resolve_predefined_entity(&name).ok_or(())?;
    value.push_str(resolved);
    Ok(())
}

fn parse_nanoclaw_plist_array(
    reader: &mut Reader<&[u8]>,
    depth: usize,
    budget: &mut usize,
) -> Result<NanoClawPlistValue, ()> {
    let mut values = Vec::new();
    loop {
        let event = next_nanoclaw_plist_event(reader)?;
        if matches!(event, Event::End(ref element) if element.name().as_ref() == b"array") {
            return Ok(NanoClawPlistValue::Array(values));
        }
        account_nanoclaw_plist_value(budget)?;
        values.push(parse_nanoclaw_plist_value_from_event(
            reader, event, depth, budget,
        )?);
    }
}

fn parse_nanoclaw_plist_dict(
    reader: &mut Reader<&[u8]>,
    depth: usize,
    budget: &mut usize,
) -> Result<NanoClawPlistValue, ()> {
    let mut entries = Vec::new();
    loop {
        let event = next_nanoclaw_plist_event(reader)?;
        if matches!(event, Event::End(ref element) if element.name().as_ref() == b"dict") {
            return Ok(NanoClawPlistValue::Dict(entries));
        }
        let Event::Start(element) = event else {
            return Err(());
        };
        if element.name().as_ref() != b"key" || !nanoclaw_plist_no_attributes(&element) {
            return Err(());
        }
        let key = parse_nanoclaw_plist_text(reader, b"key")?;
        account_nanoclaw_plist_value(budget)?;
        let value = parse_nanoclaw_plist_value(reader, depth, budget)?;
        entries.push((key, value));
    }
}

fn parse_nanoclaw_plist_value_from_event<'a>(
    reader: &mut Reader<&'a [u8]>,
    event: Event<'a>,
    depth: usize,
    budget: &mut usize,
) -> Result<NanoClawPlistValue, ()> {
    if depth > NANOCLAW_PLIST_MAX_DEPTH {
        return Err(());
    }
    match event {
        Event::Start(element) => {
            if !nanoclaw_plist_no_attributes(&element) {
                return Err(());
            }
            match element.name().as_ref() {
                b"string" => {
                    parse_nanoclaw_plist_text(reader, b"string").map(NanoClawPlistValue::String)
                }
                b"array" => parse_nanoclaw_plist_array(reader, depth + 1, budget),
                b"dict" => parse_nanoclaw_plist_dict(reader, depth + 1, budget),
                _ => Err(()),
            }
        }
        Event::Empty(element)
            if nanoclaw_plist_no_attributes(&element)
                && matches!(element.name().as_ref(), b"true" | b"false") =>
        {
            Ok(NanoClawPlistValue::Boolean)
        }
        _ => Err(()),
    }
}

fn account_nanoclaw_plist_value(budget: &mut usize) -> Result<(), ()> {
    *budget = budget.saturating_add(1);
    if *budget > MAX_FINITE_SELECTOR_ENTRIES {
        Err(())
    } else {
        Ok(())
    }
}

fn nanoclaw_launchd_values_from_root(
    root: NanoClawPlistValue,
) -> Result<NanoClawLaunchdValues, ()> {
    let NanoClawPlistValue::Dict(entries) = root else {
        return Err(());
    };
    let mut label = None;
    let mut working_directory = None;
    let mut program_arguments = None;
    for (key, value) in entries {
        match key.as_str() {
            "Label" => {
                let NanoClawPlistValue::String(value) = value else {
                    return Err(());
                };
                set_unique_plist_value(&mut label, value)?;
            }
            "WorkingDirectory" => {
                let NanoClawPlistValue::String(value) = value else {
                    return Err(());
                };
                set_unique_plist_value(&mut working_directory, value)?;
            }
            "ProgramArguments" => {
                let NanoClawPlistValue::Array(arguments) = value else {
                    return Err(());
                };
                let arguments = arguments
                    .into_iter()
                    .map(|argument| match argument {
                        NanoClawPlistValue::String(value) => Ok(value),
                        _ => Err(()),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                set_unique_plist_value(&mut program_arguments, arguments)?;
            }
            _ => reject_nested_nanoclaw_registration_fields(&value)?,
        }
    }
    Ok(NanoClawLaunchdValues {
        label,
        working_directory,
        program_arguments: program_arguments.ok_or(())?,
    })
}

fn set_unique_plist_value<T>(slot: &mut Option<T>, value: T) -> Result<(), ()> {
    if slot.replace(value).is_some() {
        Err(())
    } else {
        Ok(())
    }
}

fn reject_nested_nanoclaw_registration_fields(value: &NanoClawPlistValue) -> Result<(), ()> {
    match value {
        NanoClawPlistValue::Array(values) => {
            for value in values {
                reject_nested_nanoclaw_registration_fields(value)?;
            }
        }
        NanoClawPlistValue::Dict(entries) => {
            for (key, value) in entries {
                if matches!(
                    key.as_str(),
                    "Label" | "WorkingDirectory" | "ProgramArguments"
                ) {
                    return Err(());
                }
                reject_nested_nanoclaw_registration_fields(value)?;
            }
        }
        NanoClawPlistValue::String(_) | NanoClawPlistValue::Boolean => {}
    }
    Ok(())
}

fn set_unique_string(slot: &mut Option<String>, value: &str, path: &Path) -> Result<(), PathBuf> {
    if slot.replace(value.to_owned()).is_some() {
        return Err(path.to_path_buf());
    }
    Ok(())
}

fn reject_untrusted_service_path(path: &Path) -> Result<(), ()> {
    if !path.is_absolute() {
        return Err(());
    }
    let text = path.to_string_lossy();
    if text.is_empty()
        || text.contains('$')
        || text.contains('{')
        || text.contains('}')
        || text.contains('%')
        || text.contains(['\'', '"'])
        || text.starts_with('~')
        || text.chars().any(char::is_control)
    {
        return Err(());
    }
    Ok(())
}

fn validate_nanoclaw_registered_project(
    registration: &Path,
    project: PathBuf,
) -> Result<PathBuf, PathBuf> {
    if !selected_path_is_safe(&project, true) || !nanoclaw_supported_project_store(&project) {
        return Err(registration.to_path_buf());
    }
    let slug = nanoclaw_sha1_slug(project.to_string_lossy().as_bytes());
    let expected_launchd = format!("com.nanoclaw-v2-{slug}.plist");
    let expected_systemd = format!("nanoclaw-v2-{slug}.service");
    let Some(file_name) = registration.file_name().and_then(OsStr::to_str) else {
        return Err(registration.to_path_buf());
    };
    if file_name != expected_launchd && file_name != expected_systemd {
        return Err(registration.to_path_buf());
    }
    canonical_nanoclaw_project_store(&project).ok_or_else(|| registration.to_path_buf())
}

fn nanoclaw_sha1_slug(input: &[u8]) -> String {
    let mut message = input.to_vec();
    let bit_len = (message.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    let mut h0 = 0x6745_2301_u32;
    let mut h1 = 0xefcd_ab89_u32;
    let mut h2 = 0x98ba_dcfe_u32;
    let mut h3 = 0x1032_5476_u32;
    let mut h4 = 0xc3d2_e1f0_u32;

    for chunk in message.chunks_exact(64) {
        let mut w = [0_u32; 80];
        for (index, word) in w.iter_mut().take(16).enumerate() {
            let start = index * 4;
            *word = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for index in 16..80 {
            w[index] = (w[index - 3] ^ w[index - 8] ^ w[index - 14] ^ w[index - 16]).rotate_left(1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;

        for (index, word) in w.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let bytes = h0.to_be_bytes();
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

fn resolve_astrbot(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
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

fn resolve_shelley(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !supported_posix_host(context) {
        return report;
    }
    let Some(cwd) = context.cwd() else {
        issue_manual(&mut report, spec.provider, None);
        return report;
    };
    push_selected_source(&mut report, spec, cwd.join("shelley.db"), "shelley_sqlite");
    report
}

fn resolve_openhands(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
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
    if remote {
        report.issues.push(issue(
            spec.provider,
            None,
            DiscoveryIssueKind::NoDiskHistory,
            REMOTE_OPENHANDS_REASON,
        ));
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
        push_selected_source(&mut report, spec, root, "openhands_file_events");
    }

    let cli_root = match openhands_cli_root(context) {
        Ok(path) => path,
        Err(()) => {
            issue_manual(&mut report, spec.provider, None);
            return report;
        }
    };
    match openhands_cli_event_roots(&cli_root) {
        Ok(roots) => {
            for root in roots {
                if !push_source_candidate(
                    &mut report.sources,
                    unsupported_source(spec, root.clone(), OPENHANDS_CLI_UNSUPPORTED_REASON),
                ) {
                    issue_limit(&mut report, spec.provider, root);
                    break;
                }
            }
        }
        Err(_) => issue_selector(&mut report, spec.provider),
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
            event_roots.push(conversation);
        }
    }
    Ok(event_roots)
}

#[cfg(test)]
#[path = "profile_project_tests.rs"]
mod tests;
