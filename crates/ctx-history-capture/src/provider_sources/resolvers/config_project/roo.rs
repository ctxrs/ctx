//! Roo provider resolution for the config/project resolver group.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crate::provider_sources::{
    context::{DiscoveryContext, DiscoveryPlatform},
    selectors::{
        direct_entries, SelectorFormat, SelectorReadError, SelectorReader,
        MAX_FINITE_SELECTOR_ENTRIES,
    },
    types::{DiscoveryReport, ProviderSourceSpec},
};

use super::super::{path_presence, PathPresence};
use super::{
    add_manual_issue, add_source, git_bounded_ancestors, is_within, lexical_normalize,
    local_absolute_path, read_optional, string_setting, structured, supported_desktop_platform,
    OptionalDocument, StringSetting, INVALID_SELECTOR_REASON, MANUAL_SELECTOR_REASON, ROO_FORMAT,
    SELECTOR_LIMIT_REASON, UNSAFE_SELECTOR_REASON,
};

// Roo Code -----------------------------------------------------------------

const ROO_EXTENSIONS: &[(&str, &str)] = &[
    ("roo-cline.customStoragePath", "rooveterinaryinc.roo-cline"),
    (
        "roo-code-nightly.customStoragePath",
        "rooveterinaryinc.roo-code-nightly",
    ),
];

pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !supported_desktop_platform(context) {
        return report;
    }
    let mut reader = SelectorReader::default();
    let mut workspace_blocked = false;
    let workspace = context.cwd().and_then(|cwd| {
        match read_roo_settings(&mut reader, &cwd.join(".vscode").join("settings.json")) {
            Ok(value) => value,
            Err(()) => {
                add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
                workspace_blocked = true;
                None
            }
        }
    });
    let workspace_boundary = context
        .cwd()
        .and_then(|cwd| git_bounded_ancestors(cwd).last().cloned());

    for user_data in roo_user_data_roots(context) {
        let user_settings_path = user_data.join("User").join("settings.json");
        let (user, user_blocked) = match read_roo_settings(&mut reader, &user_settings_path) {
            Ok(value) => (value, false),
            Err(()) => {
                add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
                (None, true)
            }
        };
        if !workspace_blocked && !user_blocked {
            for (key, extension_id) in ROO_EXTENSIONS {
                let fallback = user_data
                    .join("User")
                    .join("globalStorage")
                    .join(extension_id);
                add_roo_surface(
                    &mut report,
                    spec,
                    context,
                    workspace.as_ref(),
                    user.as_ref(),
                    key,
                    fallback,
                    workspace_boundary.as_deref(),
                );
            }
        }

        let profiles = user_data.join("User").join("profiles");
        let entries = match direct_entries(&profiles) {
            Ok(entries) => entries,
            Err(SelectorReadError::Unavailable) => Vec::new(),
            Err(_) => {
                add_manual_issue(&mut report, spec.provider, SELECTOR_LIMIT_REASON);
                Vec::new()
            }
        };
        if entries.len() > MAX_FINITE_SELECTOR_ENTRIES {
            add_manual_issue(&mut report, spec.provider, SELECTOR_LIMIT_REASON);
        }
        for profile in entries.into_iter().take(MAX_FINITE_SELECTOR_ENTRIES) {
            match path_presence(&profile) {
                PathPresence::Missing => continue,
                PathPresence::Unknown(_) => {
                    add_manual_issue(&mut report, spec.provider, UNSAFE_SELECTOR_REASON);
                    continue;
                }
                PathPresence::Present => {
                    if !fs::symlink_metadata(&profile)
                        .is_ok_and(|metadata| metadata.file_type().is_dir())
                    {
                        add_manual_issue(&mut report, spec.provider, UNSAFE_SELECTOR_REASON);
                        continue;
                    }
                }
            }
            let (profile_settings, profile_blocked) =
                match read_roo_settings(&mut reader, &profile.join("settings.json")) {
                    Ok(value) => (value, false),
                    Err(()) => {
                        add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
                        (None, true)
                    }
                };
            if workspace_blocked || profile_blocked {
                continue;
            }
            for (key, extension_id) in ROO_EXTENSIONS {
                add_roo_surface(
                    &mut report,
                    spec,
                    context,
                    workspace.as_ref(),
                    profile_settings.as_ref(),
                    key,
                    profile.join("globalStorage").join(extension_id),
                    workspace_boundary.as_deref(),
                );
            }
        }
    }
    add_source(
        &mut report,
        spec,
        context.home().join(".vscode-mock").join("global-storage"),
        ROO_FORMAT,
    );
    report
}

fn roo_user_data_roots(context: &DiscoveryContext) -> Vec<PathBuf> {
    let base = match context.platform() {
        DiscoveryPlatform::Linux => context
            .env("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| context.platform_dirs().config.clone())
            .unwrap_or_else(|| context.home().join(".config")),
        DiscoveryPlatform::MacOS => context
            .platform_dirs()
            .config
            .clone()
            .unwrap_or_else(|| context.home().join("Library").join("Application Support")),
        DiscoveryPlatform::Windows => context
            .env("APPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| context.platform_dirs().config.clone())
            .unwrap_or_else(|| context.home().join("AppData").join("Roaming")),
        DiscoveryPlatform::OtherUnix => return Vec::new(),
    };
    vec![base.join("Code"), base.join("Code - Insiders")]
}

fn read_roo_settings(
    reader: &mut SelectorReader,
    path: &Path,
) -> Result<Option<BTreeMap<String, StringSetting>>, ()> {
    let OptionalDocument::Present(document) =
        read_optional(reader, path, SelectorFormat::Jsonc).map_err(|_| ())?
    else {
        return Ok(None);
    };
    let value = structured(&document).ok_or(())?;
    let mut settings = BTreeMap::new();
    for (key, _) in ROO_EXTENSIONS {
        settings.insert((*key).to_owned(), string_setting(value, &[*key]));
    }
    Ok(Some(settings))
}

#[allow(clippy::too_many_arguments)]
fn add_roo_surface(
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    _context: &DiscoveryContext,
    workspace: Option<&BTreeMap<String, StringSetting>>,
    user: Option<&BTreeMap<String, StringSetting>>,
    key: &str,
    fallback: PathBuf,
    workspace_boundary: Option<&Path>,
) {
    let (setting, from_workspace) = workspace
        .and_then(|settings| settings.get(key))
        .filter(|setting| !matches!(setting, StringSetting::Missing))
        .map(|setting| (setting, true))
        .or_else(|| {
            user.and_then(|settings| settings.get(key))
                .map(|setting| (setting, false))
        })
        .unwrap_or((&StringSetting::Missing, false));
    match setting {
        StringSetting::Missing | StringSetting::Reset => {
            add_source(report, spec, fallback, ROO_FORMAT)
        }
        StringSetting::Invalid => add_manual_issue(report, spec.provider, INVALID_SELECTOR_REASON),
        StringSetting::Value(raw) => {
            let path = PathBuf::from(raw);
            if !local_absolute_path(&path) {
                add_manual_issue(report, spec.provider, MANUAL_SELECTOR_REASON);
                return;
            }
            let path = lexical_normalize(&path);
            if from_workspace
                && workspace_boundary.is_none_or(|boundary| !is_within(&path, boundary))
            {
                add_manual_issue(report, spec.provider, UNSAFE_SELECTOR_REASON);
                return;
            }
            add_source(report, spec, path, ROO_FORMAT);
        }
    }
}
