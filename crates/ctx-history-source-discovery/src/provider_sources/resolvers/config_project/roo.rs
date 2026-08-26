//! Roo provider resolution for the config/project resolver group.

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::provider_sources::StaticProviderProbeCatalog;
use crate::provider_sources::{
    context::{DiscoveryContext, DiscoveryPlatform},
    selectors::{
        direct_entries, source_path_kind, SelectorFormat, SelectorReadError, SelectorReader,
        SourcePathKind, MAX_FINITE_SELECTOR_ENTRIES,
    },
    types::{DiscoveryReport, ProviderSourceSpec},
};

use super::super::automatic_roles::{
    automatic_route_provenance, automatic_route_provenance_with_native_os_str_id,
    AUTOMATIC_ROUTE_ROLE_UNAVAILABLE_REASON,
};
use super::super::{path_presence, PathPresence};
use super::{
    add_manual_issue, add_source_with_route_provenance, git_bounded_ancestors, is_within,
    lexical_normalize, local_absolute_path, read_optional, string_setting, structured,
    supported_desktop_platform, OptionalDocument, StringSetting, INVALID_SELECTOR_REASON,
    MANUAL_SELECTOR_REASON, ROO_FORMAT, SELECTOR_LIMIT_REASON, UNSAFE_SELECTOR_REASON,
};

// Roo Code -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RooVscodeClient {
    Stable,
    Insiders,
}

impl RooVscodeClient {
    const fn directory_name(self) -> &'static str {
        match self {
            Self::Stable => "Code",
            Self::Insiders => "Code - Insiders",
        }
    }

    const fn role_component(self) -> &'static [u8] {
        match self {
            Self::Stable => b"stable",
            Self::Insiders => b"insiders",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum RooProfileSlot<'a> {
    Base,
    Named(&'a OsStr),
}

#[derive(Debug, Clone, Copy)]
struct RooExtensionSlot {
    setting_key: &'static str,
    storage_id: &'static str,
    role_component: &'static [u8],
}

const ROO_EXTENSIONS: &[RooExtensionSlot] = &[
    RooExtensionSlot {
        setting_key: "roo-cline.customStoragePath",
        storage_id: "rooveterinaryinc.roo-cline",
        role_component: b"stable",
    },
    RooExtensionSlot {
        setting_key: "roo-code-nightly.customStoragePath",
        storage_id: "rooveterinaryinc.roo-code-nightly",
        role_component: b"nightly",
    },
];

pub(super) fn resolve(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
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

    for (client, user_data) in roo_user_data_roots(context) {
        let user_settings_path = user_data.join("User").join("settings.json");
        let (user, user_blocked) = match read_roo_settings(&mut reader, &user_settings_path) {
            Ok(value) => (value, false),
            Err(()) => {
                add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
                (None, true)
            }
        };
        if !workspace_blocked && !user_blocked {
            for extension in ROO_EXTENSIONS {
                let fallback = user_data
                    .join("User")
                    .join("globalStorage")
                    .join(extension.storage_id);
                add_roo_surface(
                    probes,
                    &mut report,
                    spec,
                    context,
                    workspace.as_ref(),
                    user.as_ref(),
                    client,
                    RooProfileSlot::Base,
                    extension,
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
                PathPresence::Unsupported | PathPresence::Unknown(_) => {
                    add_manual_issue(&mut report, spec.provider, UNSAFE_SELECTOR_REASON);
                    continue;
                }
                PathPresence::Present => {
                    if source_path_kind(&profile) != Ok(SourcePathKind::Directory) {
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
            let Some(profile_id) = profile.file_name() else {
                add_manual_issue(
                    &mut report,
                    spec.provider,
                    AUTOMATIC_ROUTE_ROLE_UNAVAILABLE_REASON,
                );
                continue;
            };
            for extension in ROO_EXTENSIONS {
                add_roo_surface(
                    probes,
                    &mut report,
                    spec,
                    context,
                    workspace.as_ref(),
                    profile_settings.as_ref(),
                    client,
                    RooProfileSlot::Named(profile_id),
                    extension,
                    profile.join("globalStorage").join(extension.storage_id),
                    workspace_boundary.as_deref(),
                );
            }
        }
    }
    let cli_route = automatic_route_provenance([b"installation".as_slice(), b"cli".as_slice()]);
    match cli_route {
        Ok(route_provenance) => add_source_with_route_provenance(
            probes,
            &mut report,
            spec,
            context.home().join(".vscode-mock").join("global-storage"),
            ROO_FORMAT,
            route_provenance,
        ),
        Err(_) => add_manual_issue(
            &mut report,
            spec.provider,
            AUTOMATIC_ROUTE_ROLE_UNAVAILABLE_REASON,
        ),
    }
    report
}

fn roo_user_data_roots(context: &DiscoveryContext) -> Vec<(RooVscodeClient, PathBuf)> {
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
    vec![
        (
            RooVscodeClient::Stable,
            base.join(RooVscodeClient::Stable.directory_name()),
        ),
        (
            RooVscodeClient::Insiders,
            base.join(RooVscodeClient::Insiders.directory_name()),
        ),
    ]
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
    for extension in ROO_EXTENSIONS {
        settings.insert(
            extension.setting_key.to_owned(),
            string_setting(value, &[extension.setting_key]),
        );
    }
    Ok(Some(settings))
}

#[allow(clippy::too_many_arguments)]
fn add_roo_surface(
    probes: &StaticProviderProbeCatalog,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    _context: &DiscoveryContext,
    workspace: Option<&BTreeMap<String, StringSetting>>,
    user: Option<&BTreeMap<String, StringSetting>>,
    client: RooVscodeClient,
    profile: RooProfileSlot<'_>,
    extension: &RooExtensionSlot,
    fallback: PathBuf,
    workspace_boundary: Option<&Path>,
) {
    let (setting, from_workspace) = workspace
        .and_then(|settings| settings.get(extension.setting_key))
        .filter(|setting| !matches!(setting, StringSetting::Missing))
        .map(|setting| (setting, true))
        .or_else(|| {
            user.and_then(|settings| settings.get(extension.setting_key))
                .map(|setting| (setting, false))
        })
        .unwrap_or((&StringSetting::Missing, false));
    match setting {
        StringSetting::Missing | StringSetting::Reset => {
            add_roo_source(probes, report, spec, fallback, client, profile, extension)
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
            add_roo_source(probes, report, spec, path, client, profile, extension);
        }
    }
}

fn add_roo_source(
    probes: &StaticProviderProbeCatalog,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    client: RooVscodeClient,
    profile: RooProfileSlot<'_>,
    extension: &RooExtensionSlot,
) {
    let route_provenance = match profile {
        RooProfileSlot::Base => automatic_route_provenance([
            b"installation".as_slice(),
            b"vscode".as_slice(),
            client.role_component(),
            b"base".as_slice(),
            extension.role_component,
        ]),
        RooProfileSlot::Named(profile_id) => automatic_route_provenance_with_native_os_str_id(
            &[
                b"installation",
                b"vscode",
                client.role_component(),
                b"profile",
            ],
            profile_id,
            &[extension.role_component],
        ),
    };
    match route_provenance {
        Ok(route_provenance) => add_source_with_route_provenance(
            probes,
            report,
            spec,
            path,
            ROO_FORMAT,
            route_provenance,
        ),
        Err(_) => add_manual_issue(
            report,
            spec.provider,
            AUTOMATIC_ROUTE_ROLE_UNAVAILABLE_REASON,
        ),
    }
}
