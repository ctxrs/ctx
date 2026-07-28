use std::{
    collections::{BTreeSet, HashSet},
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use serde_json::{Map, Value};

use super::super::{
    context::{DiscoveryContext, DiscoveryPlatform},
    selectors::{
        direct_entries, ordinary_directory, ordinary_file, ordinary_path, read_bounded_bytes,
        SelectorDocument, SelectorFormat, SelectorIncludeBudget, SelectorReadError, SelectorReader,
        MAX_FINITE_SELECTOR_ENTRIES,
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
    let Some(cwd) = context.cwd() else {
        issue_manual(&mut report, spec.provider, None);
        return report;
    };
    if ordinary_file(&cwd.join("data").join("v2.db"))
        && ordinary_directory(&cwd.join("data").join("v2-sessions"))
    {
        push_selected_source(&mut report, spec, cwd.to_path_buf(), "nanoclaw_project");
    }
    report
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
