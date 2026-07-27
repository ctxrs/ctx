//! Pi provider resolution for the config/project resolver group.

use std::path::Path;

use serde_json::Value;

use crate::provider_sources::{
    context::DiscoveryContext,
    selectors::{
        SelectorFormat, SelectorReader, MAX_FINITE_SELECTOR_ENTRIES, MAX_PROJECT_ANCESTORS,
    },
    types::{DiscoveryReport, ProviderSourceSpec},
};

use super::{
    add_manual_issue, add_source, canonical_comparison_path, is_within,
    path_is_safe_for_automatic_read, read_optional, resolve_expand_user, string_setting,
    structured, supported_desktop_platform, OptionalDocument, StringSetting,
    MANUAL_SELECTOR_REASON, PI_FORMAT, PROJECT_TRUST_REASON, UNSAFE_SELECTOR_REASON,
};

// Pi -----------------------------------------------------------------------

pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !supported_desktop_platform(context) {
        return report;
    }

    if let Some(raw) = context
        .env("PI_CODING_AGENT_SESSION_DIR")
        .filter(|value| !value.is_empty())
    {
        let Some(raw) = raw.to_str() else {
            add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
            return report;
        };
        match resolve_expand_user(raw, context.home(), context.cwd(), true) {
            Ok(path) => add_source(&mut report, spec, path, PI_FORMAT),
            Err(()) => add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON),
        }
        return report;
    }

    let agent_dir = match context
        .env("PI_CODING_AGENT_DIR")
        .filter(|value| !value.is_empty())
    {
        Some(raw) => match raw
            .to_str()
            .and_then(|raw| resolve_expand_user(raw, context.home(), context.cwd(), true).ok())
        {
            Some(path) => path,
            None => {
                add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
                return report;
            }
        },
        None => context.home().join(".pi").join("agent"),
    };

    if !path_is_safe_for_automatic_read(&agent_dir) {
        add_manual_issue(&mut report, spec.provider, UNSAFE_SELECTOR_REASON);
        return report;
    }

    let mut reader = SelectorReader::default();
    let global_path = agent_dir.join("settings.json");
    let (global_session, default_trust) =
        read_pi_settings(&mut reader, &global_path).unwrap_or_default();

    let mut selected = global_session;
    if let Some(cwd) = context.cwd() {
        let project_path = cwd.join(".pi").join("settings.json");
        if let Ok((Some(project_session), _)) = read_pi_settings(&mut reader, &project_path) {
            if pi_project_is_trusted(&mut reader, &agent_dir, cwd, default_trust) {
                let Ok(project_root) =
                    resolve_expand_user(&project_session, context.home(), context.cwd(), true)
                else {
                    add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
                    return report;
                };
                if !is_within(&project_root, cwd) {
                    add_manual_issue(&mut report, spec.provider, UNSAFE_SELECTOR_REASON);
                    return report;
                }
                selected = Some(project_session);
            } else {
                add_manual_issue(&mut report, spec.provider, PROJECT_TRUST_REASON);
            }
        }
    }

    let path = match selected {
        Some(raw) => match resolve_expand_user(&raw, context.home(), context.cwd(), true) {
            Ok(path) => path,
            Err(()) => {
                add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
                return report;
            }
        },
        None => agent_dir.join("sessions"),
    };
    add_source(&mut report, spec, path, PI_FORMAT);
    report
}

fn read_pi_settings(
    reader: &mut SelectorReader,
    path: &Path,
) -> Result<(Option<String>, Option<&'static str>), ()> {
    let OptionalDocument::Present(document) =
        read_optional(reader, path, SelectorFormat::Json).map_err(|_| ())?
    else {
        return Ok((None, None));
    };
    let Some(value) = structured(&document) else {
        return Err(());
    };
    let session = match string_setting(value, &["sessionDir"]) {
        StringSetting::Value(value) => Some(value),
        StringSetting::Missing | StringSetting::Reset | StringSetting::Invalid => None,
    };
    let default_trust = value
        .get("defaultProjectTrust")
        .and_then(Value::as_str)
        .and_then(|value| match value {
            "always" => Some("always"),
            "never" => Some("never"),
            "ask" => Some("ask"),
            _ => None,
        });
    Ok((session, default_trust))
}

fn pi_project_is_trusted(
    reader: &mut SelectorReader,
    agent_dir: &Path,
    cwd: &Path,
    default_trust: Option<&str>,
) -> bool {
    let trust_path = agent_dir.join("trust.json");
    if let Ok(OptionalDocument::Present(document)) =
        read_optional(reader, &trust_path, SelectorFormat::Json)
    {
        if let Some(map) = structured(&document).and_then(Value::as_object) {
            if map.len() <= MAX_FINITE_SELECTOR_ENTRIES {
                let cwd = canonical_comparison_path(cwd);
                for ancestor in cwd.ancestors().take(MAX_PROJECT_ANCESTORS) {
                    for (path, decision) in map {
                        if canonical_comparison_path(Path::new(path)) == ancestor {
                            if let Some(decision) = decision.as_bool() {
                                return decision;
                            }
                        }
                    }
                }
            }
        }
    }
    default_trust == Some("always")
}
