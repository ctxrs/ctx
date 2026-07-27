//! Mistral Vibe provider resolution for the config/project resolver group.

use std::path::Path;

use serde_json::Value;

use crate::provider_sources::{
    context::DiscoveryContext,
    selectors::{
        SelectorFormat, SelectorReader, MAX_FINITE_SELECTOR_ENTRIES, MAX_PROJECT_ANCESTORS,
    },
    types::{DiscoveryReport, ProviderSourceSpec},
};

use super::super::path_presence;
use super::{
    add_manual_issue, add_source, canonical_comparison_path, git_bounded_ancestors,
    path_is_safe_for_automatic_read, read_optional, resolve_expand_user, string_setting,
    structured, supported_desktop_platform, OptionalDocument, StringSetting,
    INVALID_SELECTOR_REASON, MANUAL_SELECTOR_REASON, PROJECT_TRUST_REASON, UNSAFE_SELECTOR_REASON,
    VIBE_FORMAT,
};

// Mistral Vibe -------------------------------------------------------------

pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !supported_desktop_platform(context) {
        return report;
    }
    let vibe_home = match context.env("VIBE_HOME").filter(|value| !value.is_empty()) {
        Some(raw) => {
            let Some(raw) = raw.to_str() else {
                add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
                return report;
            };
            match resolve_expand_user(raw, context.home(), context.cwd(), false) {
                Ok(path) => path,
                Err(()) => {
                    add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
                    return report;
                }
            }
        }
        None => context.home().join(".vibe"),
    };
    if !path_is_safe_for_automatic_read(&vibe_home) {
        add_manual_issue(&mut report, spec.provider, UNSAFE_SELECTOR_REASON);
        return report;
    }

    let mut reader = SelectorReader::default();
    let project_config = context.cwd().and_then(|cwd| {
        git_bounded_ancestors(cwd)
            .into_iter()
            .map(|root| root.join(".vibe").join("config.toml"))
            .find(|path| path_presence(path).suppresses_fallback())
    });
    let mut project_manual = false;
    let selected_config = if let Some(path) = project_config {
        let trusted = path
            .parent()
            .is_some_and(|root| vibe_project_is_trusted(&mut reader, &vibe_home, root));
        if trusted {
            path
        } else {
            project_manual = true;
            vibe_home.join("config.toml")
        }
    } else {
        vibe_home.join("config.toml")
    };

    let mut selected = match read_vibe_save_dir(&mut reader, &selected_config) {
        Ok(setting) => setting,
        Err(()) => {
            add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
            return report;
        }
    };
    if project_manual {
        add_manual_issue(&mut report, spec.provider, PROJECT_TRUST_REASON);
    }

    if let Some(raw) = context
        .env("VIBE_SESSION_LOGGING")
        .filter(|value| !value.is_empty())
    {
        let Some(raw) = raw.to_str() else {
            add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
            return report;
        };
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
            return report;
        };
        selected = match string_setting(&value, &["save_dir"]) {
            StringSetting::Missing | StringSetting::Reset => StringSetting::Reset,
            setting => setting,
        };
    }
    if let Some(raw) = context
        .env("VIBE_SESSION_LOGGING__SAVE_DIR")
        .filter(|value| !value.is_empty())
    {
        let Some(raw) = raw.to_str() else {
            add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
            return report;
        };
        selected = StringSetting::Value(raw.to_owned());
    }

    let root = match selected {
        StringSetting::Missing | StringSetting::Reset => vibe_home.join("logs").join("session"),
        StringSetting::Value(raw) => {
            match resolve_expand_user(&raw, context.home(), context.cwd(), false) {
                Ok(path) => path,
                Err(()) => {
                    add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
                    return report;
                }
            }
        }
        StringSetting::Invalid => {
            add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
            return report;
        }
    };
    add_source(&mut report, spec, root, VIBE_FORMAT);
    report
}

fn read_vibe_save_dir(reader: &mut SelectorReader, path: &Path) -> Result<StringSetting, ()> {
    let OptionalDocument::Present(document) =
        read_optional(reader, path, SelectorFormat::Toml).map_err(|_| ())?
    else {
        return Ok(StringSetting::Missing);
    };
    Ok(string_setting(
        structured(&document).ok_or(())?,
        &["session_logging", "save_dir"],
    ))
}

fn vibe_project_is_trusted(
    reader: &mut SelectorReader,
    vibe_home: &Path,
    project_config_dir: &Path,
) -> bool {
    let Ok(OptionalDocument::Present(document)) = read_optional(
        reader,
        &vibe_home.join("trusted_folders.toml"),
        SelectorFormat::Toml,
    ) else {
        return false;
    };
    let Some(value) = structured(&document) else {
        return false;
    };
    let trusted = value
        .get("trusted")
        .and_then(Value::as_array)
        .filter(|values| values.len() <= MAX_FINITE_SELECTOR_ENTRIES);
    let untrusted = value
        .get("untrusted")
        .and_then(Value::as_array)
        .filter(|values| values.len() <= MAX_FINITE_SELECTOR_ENTRIES);
    let target = canonical_comparison_path(project_config_dir);
    for ancestor in target.ancestors().take(MAX_PROJECT_ANCESTORS) {
        if trusted.is_some_and(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .any(|path| canonical_comparison_path(Path::new(path)) == ancestor)
        }) {
            return true;
        }
        if untrusted.is_some_and(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .any(|path| canonical_comparison_path(Path::new(path)) == ancestor)
        }) {
            return false;
        }
    }
    false
}
