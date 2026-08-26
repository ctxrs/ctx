use std::{
    collections::{BTreeSet, HashSet},
    path::{Component, Path, PathBuf},
};

use serde_json::{Map, Value};

use super::super::automatic_roles::{
    automatic_route_provenance, AUTOMATIC_ROUTE_ROLE_UNAVAILABLE_REASON,
};
use super::{
    absolute_from_cwd, env_text, expand_leading_tilde, issue_limit, issue_manual, issue_selector,
    ordinary_file, path_presence, push_source_candidate, push_unsupported_existing,
    select_current_or_legacy, selected_path_is_safe, source_from_parts,
    source_from_parts_with_data_root, DiscoveryContext, DiscoveryReport, ProviderSourceKind,
    ProviderSourceSpec, ProviderSourceStatus, SelectorDocument, SelectorFormat,
    SelectorIncludeBudget, SelectorReadError, SelectorReader, StaticProviderProbeCatalog,
    MAX_FINITE_SELECTOR_ENTRIES, OPENCLAW_UNSUPPORTED_REASON,
};
use crate::provider_sources::probes::{has_openclaw_agent_sqlite_v17, BoundedProbe};

const OPENCLAW_JSONL_SOURCE_FORMAT: &str = "openclaw_session_jsonl_tree";

pub(super) fn resolve(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !super::supported_host(context) {
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

    let config_path = selected_openclaw_config_path(&state_root);
    let (agent_ids, truncated) = match config_path.as_deref() {
        Some(path) => match read_openclaw_agent_ids(path) {
            Ok(ids) => ids,
            Err(OpenClawConfigError::Limit) => {
                issue_limit(&mut report, spec.provider, path.to_path_buf());
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
        // A bounded prefix cannot stand in for complete agent membership.
        // Leave automatic discovery route-less rather than publishing a
        // selector that would silently omit the remaining configured agents.
        return report;
    }

    for agent_id in agent_ids {
        let agent_root = state_root.join("agents").join(&agent_id);
        let route_provenance =
            match automatic_route_provenance([b"agent".as_slice(), agent_id.as_bytes()]) {
                Ok(route_provenance) => route_provenance,
                Err(_) => {
                    report.issues.push(super::issue(
                        spec.provider,
                        Some(agent_root),
                        super::DiscoveryIssueKind::SelectorUnreconstructible,
                        AUTOMATIC_ROUTE_ROLE_UNAVAILABLE_REASON,
                    ));
                    continue;
                }
            };
        let sqlite = agent_root.join("agent/openclaw-agent.sqlite");
        let sqlite_probe = has_openclaw_agent_sqlite_v17(context.data_root(), &sqlite);
        if sqlite_probe == BoundedProbe::Found {
            let mut source = source_from_parts_with_data_root(
                probes,
                context.data_root(),
                spec,
                sqlite.clone(),
                ctx_history_openclaw_schema::OPENCLAW_AGENT_SQLITE_SOURCE_FORMAT,
                ProviderSourceKind::NativeHistory,
            );
            source.route_provenance = route_provenance;
            if !push_source_candidate(&mut report.sources, source) {
                issue_limit(&mut report, spec.provider, sqlite);
            }
            continue;
        }

        let sessions = agent_root.join("sessions");
        let mut jsonl = source_from_parts(
            probes,
            spec,
            sessions.clone(),
            OPENCLAW_JSONL_SOURCE_FORMAT,
            ProviderSourceKind::NativeHistory,
        );
        jsonl.route_provenance = route_provenance.clone();
        if jsonl.status == ProviderSourceStatus::Available {
            if !push_source_candidate(&mut report.sources, jsonl) {
                issue_limit(&mut report, spec.provider, sessions);
            }
            continue;
        }

        if path_presence(&sqlite).suppresses_fallback() {
            match sqlite_probe {
                BoundedProbe::NotFound | BoundedProbe::BudgetExhausted | BoundedProbe::IoError => {
                    push_unsupported_existing(
                        &mut report,
                        spec,
                        sqlite,
                        OPENCLAW_UNSUPPORTED_REASON,
                        route_provenance,
                    )
                }
                BoundedProbe::Found => {}
            }
        }
    }
    report
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::provider_sources) enum OpenClawConfigError {
    Invalid,
    Limit,
}

pub(in crate::provider_sources) fn openclaw_agent_ids_for_state_root(
    state_root: &Path,
) -> Result<(Vec<String>, bool), OpenClawConfigError> {
    selected_openclaw_config_path(state_root).map_or_else(
        || Ok((vec!["main".to_owned()], false)),
        |path| read_openclaw_agent_ids(&path),
    )
}

fn selected_openclaw_config_path(state_root: &Path) -> Option<PathBuf> {
    ["openclaw.json", "clawdbot.json"]
        .into_iter()
        .map(|name| state_root.join(name))
        .find(|path| path_presence(path).suppresses_fallback())
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
