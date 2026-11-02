//! Prime Agent CLI selectors (upstream pa-cli/config.rs and lib.rs) and
//! pa-core/settings/{storage,manager,merge}.rs. No ancestor/project-tree scan.

use std::path::{Path, PathBuf};

use crate::provider_sources::{
    context::{DiscoveryContext, DiscoveryPlatform},
    prime_agent::{path_without_links, probe_for_role, FORMAT},
    selectors::{SelectorFormat, SelectorReader},
    types::{DiscoveryReport, ProviderDefaultLocation, ProviderSourceKind, ProviderSourceSpec},
    StaticProviderProbeCatalog,
};

use super::{
    add_manual_issue, path_presence, read_optional, structured, OptionalDocument, PathPresence,
    INVALID_SELECTOR_REASON, MANUAL_SELECTOR_REASON, UNSAFE_SELECTOR_REASON,
};
use ctx_history_capture_model::{ProviderRouteRole, ProviderSourceRouteProvenance};

pub(super) fn resolve(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    let agent_dir = agent_dir(context);
    match session_dir(context, agent_dir.as_deref().map_err(|reason| *reason)) {
        Ok(sessions) => add_source(probes, &mut report, spec, sessions, "selected-sessions"),
        Err(reason) => add_manual_issue(&mut report, spec.provider, reason),
    }

    // RLM storage is rooted in agent_dir, NOT in the selected sessionDir.
    // rlm_child_session_dir writes parent-id/child-id/session-id.jsonl; nested
    // children use the same artifacts root keyed by their immediate parent.
    match agent_dir {
        Ok(agent_dir) => add_existing(
            probes,
            &mut report,
            spec,
            agent_dir.join("session-artifacts"),
            "rlm-artifacts",
        ),
        Err(reason) if report.issues.is_empty() => {
            add_manual_issue(&mut report, spec.provider, reason)
        }
        Err(_) => {}
    }

    // T3's current per-instance/per-thread store coexists with the CLI store.
    // One-shot --session-dir elsewhere remains an explicit-path input.
    add_existing(
        probes,
        &mut report,
        spec,
        context.home().join(".t3/userdata/prime-agent"),
        "t3-store",
    );
    report
}

fn add_existing(
    probes: &StaticProviderProbeCatalog,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    role: &'static str,
) {
    if !matches!(path_presence(&path), PathPresence::Missing) {
        add_source(probes, report, spec, path, role);
    }
}

fn add_source(
    probes: &StaticProviderProbeCatalog,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    role: &'static str,
) {
    if path_without_links(&path) {
        let location = ProviderDefaultLocation {
            path_components: &[],
            source_format: FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
        };
        let selected_probe = probe_for_role(&path, Some(role.as_bytes()));
        let mut source = super::super::source_from_location_with_probe(
            probes,
            None,
            spec,
            &location,
            path,
            Some(selected_probe),
        );
        source.route_provenance = ProviderSourceRouteProvenance::Automatic {
            route_role: ProviderRouteRole::from_static(role),
        };
        // Prime has exactly three independently bounded automatic roles.
        report.sources.push(source);
    } else {
        add_manual_issue(report, spec.provider, UNSAFE_SELECTOR_REASON);
    }
}

fn agent_dir(context: &DiscoveryContext) -> Result<PathBuf, &'static str> {
    match context.env("PRIME_AGENT_CODING_AGENT_DIR") {
        Some(raw) if !raw.is_empty() => {
            expand(context, raw.to_str().ok_or(MANUAL_SELECTOR_REASON)?, true)
        }
        _ => Ok(context.home().join(".prime/agent")),
    }
}

fn session_dir(
    context: &DiscoveryContext,
    agent_dir: Result<&Path, &'static str>,
) -> Result<PathBuf, &'static str> {
    // Upstream checks presence BEFORE emptiness: an empty current variable
    // suppresses the legacy variable, then settings/defaults apply.
    if let Some(raw) = context
        .env("PRIME_AGENT_SESSION_DIR")
        .or_else(|| context.env("PRIME_AGENT_CODING_AGENT_SESSION_DIR"))
        .filter(|raw| !raw.is_empty())
    {
        return expand(context, raw.to_str().ok_or(MANUAL_SELECTOR_REASON)?, true);
    }

    let mut reader = SelectorReader::default();
    // The project scope is exact cwd/.prime/agent/settings.json. Unlike Pi,
    // Prime has no persisted trust selector for sessionDir.
    if let Some(cwd) = context.cwd() {
        if let Some(raw) =
            read_session_setting(&mut reader, &cwd.join(".prime/agent/settings.json"))?
        {
            return expand(context, &raw, false);
        }
    }
    let agent_dir = agent_dir?;
    if let Some(raw) = read_session_setting(&mut reader, &agent_dir.join("settings.json"))? {
        return expand(context, &raw, false);
    }
    Ok(agent_dir.join("sessions"))
}

fn read_session_setting(
    reader: &mut SelectorReader,
    path: &Path,
) -> Result<Option<String>, &'static str> {
    if !path_without_links(path) {
        return Err(UNSAFE_SELECTOR_REASON);
    }
    let document =
        read_optional(reader, path, SelectorFormat::Json).map_err(|_| INVALID_SELECTOR_REASON)?;
    if !path_without_links(path) {
        return Err(UNSAFE_SELECTOR_REASON);
    }
    match document {
        OptionalDocument::Missing => Ok(None),
        OptionalDocument::Empty => Err(INVALID_SELECTOR_REASON),
        OptionalDocument::Present(document) => {
            let value = structured(&document).ok_or(INVALID_SELECTOR_REASON)?;
            // Prime's lenient settings loader treats null/wrong types as unset;
            // an empty STRING is a present relative path, not an unset value.
            Ok(value
                .get("sessionDir")
                .and_then(|value| value.as_str())
                .map(str::to_owned))
        }
    }
}

fn expand(context: &DiscoveryContext, raw: &str, cli: bool) -> Result<PathBuf, &'static str> {
    // CLI env expansion supports ~\ on Windows; SettingsManager only ~/.
    let windows_tilde = cli && context.platform() == DiscoveryPlatform::Windows;
    let path = if raw == "~" {
        context.home().to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        context.home().join(rest)
    } else if let Some(rest) = raw.strip_prefix("~\\").filter(|_| windows_tilde) {
        context.home().join(rest.replace('\\', "/"))
    } else {
        PathBuf::from(raw)
    };
    // Do not lexically erase link/../ selectors before the no-follow check.
    let path = if path.is_absolute() {
        path
    } else {
        context.cwd().ok_or(MANUAL_SELECTOR_REASON)?.join(path)
    };
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(MANUAL_SELECTOR_REASON);
    }
    Ok(path)
}

#[cfg(test)]
#[path = "prime_agent_tests.rs"]
mod tests;
