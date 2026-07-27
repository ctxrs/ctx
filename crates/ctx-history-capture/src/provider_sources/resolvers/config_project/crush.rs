//! Crush provider resolution for the config/project resolver group.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::provider_sources::{
    context::{DiscoveryContext, DiscoveryPlatform},
    selectors::{sort_paths, SelectorFormat, SelectorReader, MAX_FINITE_SELECTOR_ENTRIES},
    types::{DiscoveryReport, ProviderSourceSpec},
};

use super::super::{path_presence, PathPresence};
use super::{
    add_manual_issue, add_source, git_bounded_ancestors, is_within, lexical_normalize,
    local_absolute_path, path_is_safe_for_automatic_read, read_optional, resolve_os_path,
    string_setting, structured, OptionalDocument, StringSetting, CRUSH_FORMAT,
    INVALID_SELECTOR_REASON, MANUAL_SELECTOR_REASON, SELECTOR_LIMIT_REASON, UNSAFE_SELECTOR_REASON,
};

// Crush --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CrushOrigin {
    User,
    Project,
}

pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    let mut reader = SelectorReader::default();

    let user_config_dir = match crush_directory_selector(
        context,
        "CRUSH_GLOBAL_CONFIG",
        "XDG_CONFIG_HOME",
        context.home().join(".config").join("crush"),
        true,
    ) {
        Ok(path) => path,
        Err(()) => {
            add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
            return report;
        }
    };
    let data_config_dir = match crush_data_directory(context) {
        Ok(path) => path,
        Err(()) => {
            add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
            return report;
        }
    };

    let mut config_paths = vec![
        (user_config_dir.join("crush.json"), CrushOrigin::User),
        (data_config_dir.join("crush.json"), CrushOrigin::User),
    ];
    let project_ancestors = context.cwd().map(git_bounded_ancestors).unwrap_or_default();
    for ancestor in project_ancestors.iter().rev() {
        config_paths.push((ancestor.join(".crush.json"), CrushOrigin::Project));
        config_paths.push((ancestor.join("crush.json"), CrushOrigin::Project));
    }

    let mut selected = None::<(String, CrushOrigin)>;
    let mut config_blocked = false;
    let mut seen = HashSet::new();
    for (path, origin) in config_paths {
        if !seen.insert(path.clone()) {
            continue;
        }
        match read_optional(&mut reader, &path, SelectorFormat::Json) {
            Ok(OptionalDocument::Missing | OptionalDocument::Empty) => {}
            Ok(OptionalDocument::Present(document)) => {
                let Some(value) = structured(&document) else {
                    config_blocked = true;
                    break;
                };
                match string_setting(value, &["options", "data_directory"]) {
                    StringSetting::Missing => {}
                    StringSetting::Reset => {
                        selected = None;
                    }
                    StringSetting::Value(value) => {
                        selected = Some((value, origin));
                    }
                    StringSetting::Invalid => {
                        config_blocked = true;
                        break;
                    }
                }
            }
            Err(_) => {
                config_blocked = true;
                break;
            }
        }
    }

    if config_blocked {
        add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
    } else if let Some((raw, origin)) = selected {
        let Some(cwd) = context.cwd() else {
            add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
            add_crush_registry(&mut report, spec, &mut reader, &data_config_dir);
            return report;
        };
        let root = lexical_normalize(&cwd.join(raw));
        let boundary = project_ancestors
            .last()
            .unwrap_or(&cwd.to_path_buf())
            .clone();
        if origin == CrushOrigin::Project && !is_within(&root, &boundary) {
            add_manual_issue(&mut report, spec.provider, UNSAFE_SELECTOR_REASON);
        } else {
            add_source(&mut report, spec, root.join("crush.db"), CRUSH_FORMAT);
        }
    } else if context.cwd().is_none() {
        add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
    } else {
        let cwd = context.cwd().expect("checked above");
        let Ok(root) = default_crush_root(cwd, &project_ancestors) else {
            add_manual_issue(&mut report, spec.provider, UNSAFE_SELECTOR_REASON);
            add_crush_registry(&mut report, spec, &mut reader, &data_config_dir);
            return report;
        };
        add_source(&mut report, spec, root.join("crush.db"), CRUSH_FORMAT);
    }

    add_crush_registry(&mut report, spec, &mut reader, &data_config_dir);
    report
}

fn crush_directory_selector(
    context: &DiscoveryContext,
    primary: &str,
    secondary: &str,
    fallback: PathBuf,
    secondary_adds_crush: bool,
) -> Result<PathBuf, ()> {
    if let Some(raw) = context.env(primary).filter(|value| !value.is_empty()) {
        return resolve_os_path(raw, context.cwd());
    }
    if let Some(raw) = context.env(secondary).filter(|value| !value.is_empty()) {
        let path = resolve_os_path(raw, context.cwd())?;
        return Ok(if secondary_adds_crush {
            path.join("crush")
        } else {
            path
        });
    }
    Ok(fallback)
}

fn crush_data_directory(context: &DiscoveryContext) -> Result<PathBuf, ()> {
    let fallback = match context.platform() {
        DiscoveryPlatform::Windows => context
            .platform_dirs()
            .local_data
            .clone()
            .unwrap_or_else(|| context.home().join("AppData").join("Local"))
            .join("crush"),
        _ => context.home().join(".local").join("share").join("crush"),
    };
    crush_directory_selector(
        context,
        "CRUSH_GLOBAL_DATA",
        "XDG_DATA_HOME",
        fallback,
        true,
    )
}

fn default_crush_root(cwd: &Path, ancestors: &[PathBuf]) -> Result<PathBuf, ()> {
    for ancestor in ancestors {
        let candidate = ancestor.join(".crush");
        match path_presence(&candidate) {
            PathPresence::Missing => {}
            PathPresence::Present if path_is_safe_for_automatic_read(&candidate) => {
                let metadata = fs::symlink_metadata(&candidate).map_err(|_| ())?;
                return metadata.file_type().is_dir().then_some(candidate).ok_or(());
            }
            PathPresence::Present | PathPresence::Unknown(_) => return Err(()),
        }
    }
    Ok(cwd.join(".crush"))
}

fn add_crush_registry(
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    reader: &mut SelectorReader,
    data_config_dir: &Path,
) {
    let path = data_config_dir.join("projects.json");
    let document = match read_optional(reader, &path, SelectorFormat::Json) {
        Ok(OptionalDocument::Missing | OptionalDocument::Empty) => return,
        Ok(OptionalDocument::Present(document)) => document,
        Err(_) => {
            add_manual_issue(report, spec.provider, INVALID_SELECTOR_REASON);
            return;
        }
    };
    let Some(projects) = structured(&document)
        .and_then(|value| value.get("projects"))
        .and_then(Value::as_array)
    else {
        add_manual_issue(report, spec.provider, INVALID_SELECTOR_REASON);
        return;
    };
    if projects.len() > MAX_FINITE_SELECTOR_ENTRIES {
        add_manual_issue(report, spec.provider, SELECTOR_LIMIT_REASON);
        return;
    }
    let mut paths = Vec::new();
    for project in projects {
        let Some(raw) = project.get("data_dir").and_then(Value::as_str) else {
            continue;
        };
        let root = PathBuf::from(raw);
        if local_absolute_path(&root) {
            paths.push(lexical_normalize(&root).join("crush.db"));
        } else {
            add_manual_issue(report, spec.provider, MANUAL_SELECTOR_REASON);
        }
    }
    sort_paths(&mut paths);
    for path in paths {
        add_source(report, spec, path, CRUSH_FORMAT);
    }
}
