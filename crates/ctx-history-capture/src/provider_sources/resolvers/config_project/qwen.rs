//! Qwen provider resolution for the config/project resolver group.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::provider_sources::{
    context::{DiscoveryContext, DiscoveryPlatform, DISCOVERY_ENV_ALLOWLIST},
    selectors::{SelectorFormat, SelectorReader, MAX_FINITE_SELECTOR_ENTRIES},
    types::{DiscoveryReport, ProviderSourceSpec},
};

use super::{
    add_manual_issue, add_source, bool_setting, canonical_comparison_path, git_bounded_ancestors,
    is_within, read_optional, resolve_expand_user, resolve_os_path, string_setting, structured,
    supported_desktop_platform, OptionalDocument, StringSetting, INVALID_SELECTOR_REASON,
    MANUAL_SELECTOR_REASON, PROJECT_TRUST_REASON, QWEN_FORMAT, UNSAFE_SELECTOR_REASON,
};

// Qwen Code ----------------------------------------------------------------

#[derive(Debug, Default)]
struct QwenScope {
    runtime_root: Option<StringSetting>,
    folder_trust: Option<bool>,
}

pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !supported_desktop_platform(context) {
        return report;
    }

    if let Some(raw) = context
        .env("QWEN_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
    {
        let Some(raw) = raw.to_str() else {
            add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
            return report;
        };
        match resolve_expand_user(raw, context.home(), context.cwd(), true) {
            Ok(root) => add_source(&mut report, spec, root.join("projects"), QWEN_FORMAT),
            Err(()) => add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON),
        }
        return report;
    }

    let qwen_home = match context.env("QWEN_HOME").filter(|value| !value.is_empty()) {
        Some(raw) => {
            let Some(raw) = raw.to_str() else {
                add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
                return report;
            };
            match resolve_expand_user(raw, context.home(), context.cwd(), true) {
                Ok(path) => path,
                Err(()) => {
                    add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
                    return report;
                }
            }
        }
        None => context.home().join(".qwen"),
    };

    let Some((system_defaults_path, system_path)) = qwen_system_paths(context) else {
        return report;
    };
    let mut reader = SelectorReader::default();
    let system_defaults = match read_qwen_scope(&mut reader, &system_defaults_path) {
        Ok(scope) => scope,
        Err(()) => {
            add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
            return report;
        }
    };
    let user = match read_qwen_scope(&mut reader, &qwen_home.join("settings.json")) {
        Ok(scope) => scope,
        Err(()) => {
            add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
            return report;
        }
    };
    let system = match read_qwen_scope(&mut reader, &system_path) {
        Ok(scope) => scope,
        Err(()) => {
            add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
            return report;
        }
    };
    let project = if let Some(cwd) = context.cwd() {
        match read_qwen_scope(&mut reader, &cwd.join(".qwen").join("settings.json")) {
            Ok(scope) => scope,
            Err(()) => {
                add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
                return report;
            }
        }
    } else {
        QwenScope::default()
    };

    let folder_trust = system
        .folder_trust
        .or(user.folder_trust)
        .or(system_defaults.folder_trust)
        .unwrap_or(false);
    let project_trusted = !folder_trust
        || context
            .cwd()
            .is_some_and(|cwd| qwen_project_is_trusted(&mut reader, context, &qwen_home, cwd));

    let mut selected = StringSetting::Missing;
    let mut selected_from_project = false;
    for (setting, from_project) in [
        (system_defaults.runtime_root.as_ref(), false),
        (user.runtime_root.as_ref(), false),
        (
            project_trusted
                .then_some(project.runtime_root.as_ref())
                .flatten(),
            true,
        ),
        (system.runtime_root.as_ref(), false),
    ] {
        if let Some(setting) = setting {
            selected = setting.clone();
            selected_from_project = from_project;
        }
    }

    if !project_trusted && matches!(project.runtime_root, Some(StringSetting::Value(_))) {
        add_manual_issue(&mut report, spec.provider, PROJECT_TRUST_REASON);
    }
    let root = match selected {
        StringSetting::Missing | StringSetting::Reset => qwen_home,
        StringSetting::Invalid => {
            add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
            return report;
        }
        StringSetting::Value(raw) => {
            let raw = match interpolate_qwen(context, &raw) {
                Ok(raw) => raw,
                Err(()) => {
                    add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
                    return report;
                }
            };
            let root = match resolve_expand_user(&raw, context.home(), context.cwd(), true) {
                Ok(path) => path,
                Err(()) => {
                    add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
                    return report;
                }
            };
            if selected_from_project
                && context.cwd().is_some_and(|cwd| {
                    let boundary = git_bounded_ancestors(cwd)
                        .last()
                        .cloned()
                        .unwrap_or_else(|| cwd.to_path_buf());
                    !is_within(&root, &boundary)
                })
            {
                add_manual_issue(&mut report, spec.provider, UNSAFE_SELECTOR_REASON);
                return report;
            }
            root
        }
    };
    add_source(&mut report, spec, root.join("projects"), QWEN_FORMAT);
    report
}

fn qwen_system_paths(context: &DiscoveryContext) -> Option<(PathBuf, PathBuf)> {
    let default_system = match context.platform() {
        DiscoveryPlatform::Linux => PathBuf::from("/etc/qwen-code"),
        DiscoveryPlatform::MacOS => PathBuf::from("/Library/Application Support/QwenCode"),
        DiscoveryPlatform::Windows => PathBuf::from(r"C:\ProgramData\qwen-code"),
        DiscoveryPlatform::OtherUnix => return None,
    };
    let system_path = context
        .env("QWEN_CODE_SYSTEM_SETTINGS_PATH")
        .filter(|value| !value.is_empty())
        .and_then(|value| resolve_os_path(value, context.cwd()).ok())
        .unwrap_or_else(|| default_system.join("settings.json"));
    let defaults_path = context
        .env("QWEN_CODE_SYSTEM_DEFAULTS_PATH")
        .filter(|value| !value.is_empty())
        .and_then(|value| resolve_os_path(value, context.cwd()).ok())
        .unwrap_or_else(|| {
            if context.env("QWEN_CODE_SYSTEM_SETTINGS_PATH").is_some() {
                system_path
                    .parent()
                    .unwrap_or(&default_system)
                    .join("system-defaults.json")
            } else {
                default_system.join("system-defaults.json")
            }
        });
    Some((defaults_path, system_path))
}

fn read_qwen_scope(reader: &mut SelectorReader, path: &Path) -> Result<QwenScope, ()> {
    let OptionalDocument::Present(document) =
        read_optional(reader, path, SelectorFormat::Jsonc).map_err(|_| ())?
    else {
        return Ok(QwenScope::default());
    };
    let value = structured(&document).ok_or(())?;
    let runtime_root = match string_setting(value, &["advanced", "runtimeOutputDir"]) {
        StringSetting::Missing => None,
        setting => Some(setting),
    };
    let folder_trust =
        bool_setting(value, &["security", "folderTrust", "enabled"]).map_err(|_| ())?;
    Ok(QwenScope {
        runtime_root,
        folder_trust,
    })
}

fn qwen_project_is_trusted(
    reader: &mut SelectorReader,
    context: &DiscoveryContext,
    qwen_home: &Path,
    cwd: &Path,
) -> bool {
    let path = context
        .env("QWEN_CODE_TRUSTED_FOLDERS_PATH")
        .filter(|value| !value.is_empty())
        .and_then(|value| resolve_os_path(value, context.cwd()).ok())
        .unwrap_or_else(|| qwen_home.join("trustedFolders.json"));
    let Ok(OptionalDocument::Present(document)) =
        read_optional(reader, &path, SelectorFormat::Jsonc)
    else {
        return false;
    };
    let Some(map) = structured(&document).and_then(Value::as_object) else {
        return false;
    };
    if map.len() > MAX_FINITE_SELECTOR_ENTRIES {
        return false;
    }
    let cwd = canonical_comparison_path(cwd);
    for (path, level) in map {
        let comparison = canonical_comparison_path(Path::new(path));
        match level.as_str() {
            Some("TRUST_FOLDER") if is_within(&cwd, &comparison) => return true,
            Some("TRUST_PARENT")
                if comparison
                    .parent()
                    .is_some_and(|parent| is_within(&cwd, parent)) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn interpolate_qwen(context: &DiscoveryContext, raw: &str) -> Result<String, ()> {
    let bytes = raw.as_bytes();
    let mut output = String::with_capacity(raw.len());
    let mut index = 0;
    while let Some(offset) = raw[index..].find('$') {
        let dollar = index + offset;
        output.push_str(&raw[index..dollar]);
        index = dollar;
        let (name, end, original) = if bytes.get(index + 1) == Some(&b'{') {
            let Some(close) = raw[index + 2..].find('}') else {
                output.push('$');
                index += 1;
                continue;
            };
            let end = index + 2 + close;
            (&raw[index + 2..end], end + 1, &raw[index..=end])
        } else {
            let mut end = index + 1;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end == index + 1 {
                output.push('$');
                index += 1;
                continue;
            }
            (&raw[index + 1..end], end, &raw[index..end])
        };
        if !DISCOVERY_ENV_ALLOWLIST.contains(&name) {
            return Err(());
        }
        match context.env(name).and_then(OsStr::to_str) {
            Some(value) => output.push_str(value),
            None => output.push_str(original),
        }
        index = end;
    }
    output.push_str(&raw[index..]);
    Ok(output)
}
