use std::path::{Path, PathBuf};

use super::super::super::{
    context::{DiscoveryContext, DiscoveryPlatform},
    lingma::{
        DiscoveredLingmaDatabase, LingmaDatabaseCatalogLineage, LingmaVscodeClient,
        LingmaVscodeProfile,
    },
    selectors::{
        direct_entries, SelectorDocument, SelectorFormat, SelectorReadError, SelectorReader,
        MAX_SELECTOR_FILES_PER_PROVIDER,
    },
    types::{DiscoveryIssueKind, DiscoveryReport, ProviderSourceSpec},
};
use super::super::{
    issue, path_presence, push_source_candidate, select_current_or_legacy, PathPresence,
};
use super::{
    path_is_absolute_for_platform, safe_native_source, supported_desktop_platform,
    SELECTOR_MANUAL_REASON,
};

const LINGMA_FORMAT: &str = "lingma_sqlite";
const SELECTOR_READ_REASON: &str =
    "the provider selector could not be read safely within discovery limits; use an exact --path";

#[derive(Debug, Clone)]
enum LingmaRootChoice {
    Absent,
    Default,
    Selected(PathBuf),
    Unreconstructible(PathBuf),
}

pub(super) fn resolve_lingma(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    resolve_lingma_with_authority(context, spec).0
}

pub(in crate::provider_sources) fn resolve_lingma_with_authority(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> (DiscoveryReport, Vec<DiscoveredLingmaDatabase>) {
    if !supported_desktop_platform(context.platform()) {
        return (DiscoveryReport::default(), Vec::new());
    }
    let mut report = DiscoveryReport::default();
    let mut discovered = Vec::new();
    let mut reader = SelectorReader::default();
    resolve_lingma_vscode(context, spec, &mut reader, &mut report, &mut discovered);
    resolve_lingma_jetbrains(context, spec, &mut reader, &mut report, &mut discovered);
    (report, discovered)
}

fn resolve_lingma_vscode(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    reader: &mut SelectorReader,
    report: &mut DiscoveryReport,
    discovered: &mut Vec<DiscoveredLingmaDatabase>,
) {
    let default_db = context
        .home()
        .join(".lingma/vscode/sharedClientCache/cache/db/local.db");
    let user_roots = vscode_user_roots(context);
    let mut installed_settings_found = false;
    for (client, user_root) in user_roots {
        if matches!(path_presence(&user_root), PathPresence::Missing) {
            continue;
        }
        let base_settings = user_root.join("settings.json");
        let (base, base_allows_absent_profile_fallback) =
            read_vscode_lingma_choice(reader, &base_settings, context.platform(), report, spec);
        installed_settings_found |= base.is_some() || !base_allows_absent_profile_fallback;
        if base.is_some() {
            add_lingma_vscode_choice(
                context.data_root(),
                report,
                discovered,
                spec,
                base.as_ref(),
                &default_db,
                client,
                LingmaVscodeProfile::Base,
            );
        }
        let profiles = user_root.join("profiles");
        let entries = match direct_entries(&profiles) {
            Ok(entries) => entries,
            Err(_) if path_presence(&profiles).suppresses_fallback() => {
                installed_settings_found = true;
                report.issues.push(issue(
                    spec.provider,
                    Some(profiles),
                    DiscoveryIssueKind::SelectorUnreconstructible,
                    SELECTOR_READ_REASON,
                ));
                continue;
            }
            Err(_) => continue,
        };
        for profile in entries {
            if reader.files_read() >= MAX_SELECTOR_FILES_PER_PROVIDER {
                report.issues.push(issue(
                    spec.provider,
                    Some(profiles.clone()),
                    DiscoveryIssueKind::SelectorUnreconstructible,
                    SELECTOR_READ_REASON,
                ));
                break;
            }
            let settings = profile.join("settings.json");
            if matches!(path_presence(&settings), PathPresence::Missing) {
                continue;
            }
            installed_settings_found = true;
            let (Some(profile_choice), _) =
                read_vscode_lingma_choice(reader, &settings, context.platform(), report, spec)
            else {
                continue;
            };
            let (effective, profile_key) = match &profile_choice {
                LingmaRootChoice::Absent if base_allows_absent_profile_fallback => {
                    (base.as_ref(), LingmaVscodeProfile::Base)
                }
                LingmaRootChoice::Absent => continue,
                selected => {
                    let Some(name) = profile.file_name() else {
                        report.issues.push(issue(
                            spec.provider,
                            Some(profile.clone()),
                            DiscoveryIssueKind::SelectorUnreconstructible,
                            SELECTOR_READ_REASON,
                        ));
                        continue;
                    };
                    (
                        Some(selected),
                        LingmaVscodeProfile::Named(name.as_encoded_bytes().to_vec()),
                    )
                }
            };
            add_lingma_vscode_choice(
                context.data_root(),
                report,
                discovered,
                spec,
                effective,
                &default_db,
                client,
                profile_key,
            );
        }
    }

    if !installed_settings_found && path_presence(&default_db).suppresses_fallback() {
        push_lingma_source(
            context.data_root(),
            report,
            discovered,
            spec,
            default_db,
            LingmaDatabaseCatalogLineage::VscodeSharedDefault,
        );
    }
}

fn vscode_user_roots(context: &DiscoveryContext) -> Vec<(LingmaVscodeClient, PathBuf)> {
    let base = match context.platform() {
        DiscoveryPlatform::Linux => context
            .platform_dirs()
            .config
            .clone()
            .unwrap_or_else(|| context.home().join(".config")),
        DiscoveryPlatform::MacOS => context.home().join("Library").join("Application Support"),
        DiscoveryPlatform::Windows => match &context.platform_dirs().config {
            Some(path) => path.clone(),
            None => return Vec::new(),
        },
        DiscoveryPlatform::OtherUnix => return Vec::new(),
    };
    vec![
        (LingmaVscodeClient::Stable, base.join("Code").join("User")),
        (
            LingmaVscodeClient::Insiders,
            base.join("Code - Insiders").join("User"),
        ),
    ]
}

fn read_vscode_lingma_choice(
    reader: &mut SelectorReader,
    path: &Path,
    platform: DiscoveryPlatform,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
) -> (Option<LingmaRootChoice>, bool) {
    let document = match reader.read(path, SelectorFormat::Jsonc) {
        Ok(document) => document,
        Err(SelectorReadError::Unavailable)
            if matches!(path_presence(path), PathPresence::Missing) =>
        {
            return (None, true);
        }
        Err(_) => {
            report.issues.push(issue(
                spec.provider,
                Some(path.to_path_buf()),
                DiscoveryIssueKind::SelectorUnreconstructible,
                SELECTOR_READ_REASON,
            ));
            return (None, false);
        }
    };
    let SelectorDocument::Structured(value) = &document else {
        return (None, false);
    };
    let Some(settings) = value.as_object() else {
        return (Some(LingmaRootChoice::Absent), true);
    };
    let value = settings
        .get("QoderCN.LocalMachineStoragePath")
        .or_else(|| settings.get("Lingma.LocalMachineStoragePath"));
    (
        Some(match value {
            None => LingmaRootChoice::Absent,
            Some(value) if value.as_str().is_some_and(str::is_empty) => LingmaRootChoice::Default,
            Some(value) if value.as_str().is_none() => LingmaRootChoice::Default,
            Some(value) => {
                let Some(value) = value.as_str() else {
                    return (Some(LingmaRootChoice::Default), true);
                };
                let root = PathBuf::from(value);
                if path_is_local_absolute_for_platform(&root, platform) {
                    LingmaRootChoice::Selected(root)
                } else {
                    LingmaRootChoice::Unreconstructible(root)
                }
            }
        }),
        true,
    )
}

fn add_lingma_vscode_choice(
    data_root: Option<&Path>,
    report: &mut DiscoveryReport,
    discovered: &mut Vec<DiscoveredLingmaDatabase>,
    spec: &ProviderSourceSpec,
    choice: Option<&LingmaRootChoice>,
    default_db: &Path,
    client: LingmaVscodeClient,
    profile: LingmaVscodeProfile,
) {
    let (path, lineage) = match choice.unwrap_or(&LingmaRootChoice::Default) {
        LingmaRootChoice::Absent | LingmaRootChoice::Default => (
            default_db.to_path_buf(),
            LingmaDatabaseCatalogLineage::VscodeSharedDefault,
        ),
        LingmaRootChoice::Selected(root) => (
            root.join("sharedClientCache/cache/db/local.db"),
            LingmaDatabaseCatalogLineage::VscodeSelected { client, profile },
        ),
        LingmaRootChoice::Unreconstructible(path) => {
            report.issues.push(issue(
                spec.provider,
                Some(path.clone()),
                DiscoveryIssueKind::SelectorUnreconstructible,
                SELECTOR_MANUAL_REASON,
            ));
            return;
        }
    };
    push_lingma_source(data_root, report, discovered, spec, path, lineage);
}

fn resolve_lingma_jetbrains(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
    reader: &mut SelectorReader,
    report: &mut DiscoveryReport,
    discovered: &mut Vec<DiscoveredLingmaDatabase>,
) {
    let config_root = match context.platform() {
        DiscoveryPlatform::Linux | DiscoveryPlatform::Windows => context
            .platform_dirs()
            .config
            .as_ref()
            .map(|path| path.join("JetBrains")),
        DiscoveryPlatform::MacOS => Some(
            context
                .home()
                .join("Library")
                .join("Application Support")
                .join("JetBrains"),
        ),
        DiscoveryPlatform::OtherUnix => None,
    };
    let mut settings_found = false;
    if let Some(config_root) = config_root {
        match direct_entries(&config_root) {
            Ok(entries) => {
                for product in entries {
                    if reader.files_read() >= MAX_SELECTOR_FILES_PER_PROVIDER {
                        report.issues.push(issue(
                            spec.provider,
                            Some(config_root.clone()),
                            DiscoveryIssueKind::SelectorUnreconstructible,
                            SELECTOR_READ_REASON,
                        ));
                        break;
                    }
                    let settings = product.join("options").join("cosy_setting.xml");
                    if matches!(path_presence(&settings), PathPresence::Missing) {
                        continue;
                    }
                    settings_found = true;
                    let Some(choice) = read_jetbrains_lingma_choice(
                        reader,
                        &settings,
                        context.platform(),
                        report,
                        spec,
                    ) else {
                        continue;
                    };
                    if let LingmaRootChoice::Unreconstructible(path) = &choice {
                        report.issues.push(issue(
                            spec.provider,
                            Some(path.clone()),
                            DiscoveryIssueKind::SelectorUnreconstructible,
                            SELECTOR_MANUAL_REASON,
                        ));
                    }
                    let Some(path) = jetbrains_lingma_db(context, Some(&choice)) else {
                        continue;
                    };
                    let lineage = match choice {
                        LingmaRootChoice::Absent | LingmaRootChoice::Default => {
                            LingmaDatabaseCatalogLineage::JetBrainsSharedDefault
                        }
                        LingmaRootChoice::Selected(_) => {
                            let Some(product_name) = product.file_name() else {
                                report.issues.push(issue(
                                    spec.provider,
                                    Some(product.clone()),
                                    DiscoveryIssueKind::SelectorUnreconstructible,
                                    SELECTOR_READ_REASON,
                                ));
                                continue;
                            };
                            LingmaDatabaseCatalogLineage::JetBrainsSelected {
                                product: product_name.as_encoded_bytes().to_vec(),
                            }
                        }
                        LingmaRootChoice::Unreconstructible(_) => continue,
                    };
                    push_lingma_source(
                        context.data_root(),
                        report,
                        discovered,
                        spec,
                        path,
                        lineage,
                    );
                }
            }
            Err(_) if path_presence(&config_root).suppresses_fallback() => {
                settings_found = true;
                report.issues.push(issue(
                    spec.provider,
                    Some(config_root),
                    DiscoveryIssueKind::SelectorUnreconstructible,
                    SELECTOR_READ_REASON,
                ));
            }
            Err(_) => {}
        }
    }

    if !settings_found {
        let current = current_jetbrains_default(context.home());
        let legacy = legacy_jetbrains_default(context.home());
        let selected = if path_presence(&current).suppresses_fallback() {
            Some(current)
        } else if path_presence(&legacy).suppresses_fallback() {
            Some(legacy)
        } else {
            None
        };
        if let Some(path) = selected {
            push_lingma_source(
                context.data_root(),
                report,
                discovered,
                spec,
                path,
                LingmaDatabaseCatalogLineage::JetBrainsSharedDefault,
            );
        }
    }
}

fn push_lingma_source(
    data_root: Option<&Path>,
    report: &mut DiscoveryReport,
    discovered: &mut Vec<DiscoveredLingmaDatabase>,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    lineage: LingmaDatabaseCatalogLineage,
) {
    let source = safe_native_source(data_root, spec, path, LINGMA_FORMAT);
    if push_source_candidate(&mut report.sources, source.clone()) {
        discovered.push(DiscoveredLingmaDatabase::new(source, lineage));
    } else {
        report.issues.push(issue(
            spec.provider,
            None,
            DiscoveryIssueKind::SelectorUnreconstructible,
            SELECTOR_READ_REASON,
        ));
    }
}

fn read_jetbrains_lingma_choice(
    reader: &mut SelectorReader,
    path: &Path,
    platform: DiscoveryPlatform,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
) -> Option<LingmaRootChoice> {
    let document = match reader.read(path, SelectorFormat::Xml) {
        Ok(document) => document,
        Err(_) => {
            report.issues.push(issue(
                spec.provider,
                Some(path.to_path_buf()),
                DiscoveryIssueKind::SelectorUnreconstructible,
                SELECTOR_READ_REASON,
            ));
            return None;
        }
    };
    let xml = document.xml()?;
    let component_path = ["application", "component"];
    let component_names = xml.values(&component_path, Some("name"));
    if xml.values(&component_path, None).len() != 1 || component_names != ["CosySettings"] {
        return Some(LingmaRootChoice::Default);
    }
    let option_path = ["application", "component", "option"];
    let option_count = xml.values(&option_path, None).len();
    let names = xml.values(&option_path, Some("name"));
    let values = xml.values(&option_path, Some("value"));
    if names.len() != option_count || values.len() != option_count {
        return Some(LingmaRootChoice::Default);
    }
    let value = names
        .into_iter()
        .zip(values)
        .find_map(|(name, value)| (name == "localStoragePath").then_some(value));
    Some(match value {
        None | Some("") => LingmaRootChoice::Default,
        Some(value) => {
            let root = PathBuf::from(value);
            if path_is_local_absolute_for_platform(&root, platform) {
                LingmaRootChoice::Selected(root)
            } else {
                LingmaRootChoice::Unreconstructible(root)
            }
        }
    })
}

fn jetbrains_lingma_db(
    context: &DiscoveryContext,
    choice: Option<&LingmaRootChoice>,
) -> Option<PathBuf> {
    match choice.unwrap_or(&LingmaRootChoice::Default) {
        LingmaRootChoice::Absent | LingmaRootChoice::Default => {
            let current = current_jetbrains_default(context.home());
            let legacy = legacy_jetbrains_default(context.home());
            Some(select_current_or_legacy(current, legacy))
        }
        LingmaRootChoice::Selected(root) => {
            let current = root.join("qoder-cn/cache/db/local.db");
            let legacy = root.join("cache/db/local.db");
            Some(select_current_or_legacy(current, legacy))
        }
        LingmaRootChoice::Unreconstructible(_) => None,
    }
}

fn current_jetbrains_default(home: &Path) -> PathBuf {
    home.join(".qoder-cn/shared_client/cache/db/local.db")
}

fn legacy_jetbrains_default(home: &Path) -> PathBuf {
    home.join(".lingma/cache/db/local.db")
}

fn path_is_local_absolute_for_platform(path: &Path, platform: DiscoveryPlatform) -> bool {
    if platform == DiscoveryPlatform::Windows {
        let value = path.to_string_lossy();
        if value.starts_with(r"\\") || value.starts_with("//") {
            return false;
        }
    }
    path_is_absolute_for_platform(path, platform)
}
