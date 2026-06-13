use crate::desktop_runtime::DesktopBuildIdentity;
use anyhow::{anyhow, Context, Result};
use std::path::Path;

mod models;
mod paths;
mod targets;
#[cfg(test)]
mod tests;

pub(super) use models::{
    allowed_source_types_for_profile, avf_helper_metadata_complete, avf_helper_names_and_paths,
    find_required_component, required_component_has_managed_source, DesktopBundledAssetsManifest,
    DesktopBundledProviderManifest, RuntimeLockV2,
};
pub(super) use paths::{
    bundle_manifest_path, bundled_artifact_identity_path, bundled_provider_manifest_path,
};
pub(super) use targets::{
    host_default_image_targets, host_default_machine_cache_targets, host_default_provider_targets,
    host_default_runtime_targets, host_relevant_targets, parse_target, required_targets_or_default,
    RuntimeTarget,
};

fn parity_profile_enabled() -> bool {
    matches!(
        std::env::var("CTX_RUNTIME_PROFILE")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref(),
        None | Some("") | Some("parity")
    )
}

fn active_runtime_profile() -> &'static str {
    match std::env::var("CTX_RUNTIME_PROFILE")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("override") => "override",
        Some("source-all") => "source-all",
        _ => "parity",
    }
}

pub(super) fn enforce_desktop_parity_bundle_preflight(bundle_dir: Option<&Path>) -> Result<()> {
    if !parity_profile_enabled() {
        return Ok(());
    }
    let channel = std::env::var("CTX_DESKTOP_CHANNEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "dev".to_string());
    let surface = std::env::var("CTX_LAUNCH_SURFACE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "desktop".to_string());

    let bundle_dir = bundle_dir.ok_or_else(|| anyhow!("bundle dir not found"))?;
    let artifact_identity_path = bundled_artifact_identity_path(bundle_dir);
    let artifact_identity_raw = std::fs::read_to_string(&artifact_identity_path)
        .with_context(|| format!("reading {}", artifact_identity_path.display()))?;
    let artifact_identity: DesktopBuildIdentity = serde_json::from_str(&artifact_identity_raw)
        .with_context(|| format!("parsing {}", artifact_identity_path.display()))?;
    if artifact_identity.schema_version != 1 {
        anyhow::bail!(
            "unsupported artifact identity schema {} at {}",
            artifact_identity.schema_version,
            artifact_identity_path.display()
        );
    }
    if artifact_identity.exact_version.trim().is_empty()
        || artifact_identity.build_id.trim().is_empty()
        || artifact_identity.compatibility_token.trim().is_empty()
    {
        anyhow::bail!(
            "artifact identity must contain exactVersion, buildId, and compatibilityToken: {}",
            artifact_identity_path.display()
        );
    }

    let bundled_provider_manifest_path = bundled_provider_manifest_path(bundle_dir);
    let bundled_provider_manifest_raw = std::fs::read_to_string(&bundled_provider_manifest_path)
        .with_context(|| format!("reading {}", bundled_provider_manifest_path.display()))?;
    let bundled_provider_manifest: DesktopBundledProviderManifest =
        serde_json::from_str(&bundled_provider_manifest_raw)
            .with_context(|| format!("parsing {}", bundled_provider_manifest_path.display()))?;
    if bundled_provider_manifest.version == 0 {
        anyhow::bail!(
            "bundled provider manifest must declare a non-zero schema version: {}",
            bundled_provider_manifest_path.display()
        );
    }
    if bundled_provider_manifest.providers.is_empty() {
        anyhow::bail!(
            "bundled provider manifest must contain at least one provider entry: {}",
            bundled_provider_manifest_path.display()
        );
    }

    let manifest_path = bundle_manifest_path(bundle_dir);
    let manifest_parent = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| bundle_dir.to_path_buf());
    let manifest_sibling_lock = manifest_parent.join("runtime_lock.v2.json");
    let lock_path = if manifest_sibling_lock.exists() {
        manifest_sibling_lock
    } else {
        bundle_dir.join("runtime_lock.v2.json")
    };
    let lock_raw = std::fs::read_to_string(&lock_path)
        .with_context(|| format!("reading {}", lock_path.display()))?;
    let lock: RuntimeLockV2 = serde_json::from_str(&lock_raw)
        .with_context(|| format!("parsing {}", lock_path.display()))?;
    if lock.version != 2 {
        anyhow::bail!(
            "unsupported runtime lock version {} at {}",
            lock.version,
            lock_path.display()
        );
    }

    let manifest_raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: DesktopBundledAssetsManifest = serde_json::from_str(&manifest_raw)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;

    let provider_default_targets = host_default_provider_targets();
    let runtime_default_targets = host_default_runtime_targets();
    let image_default_targets = host_default_image_targets();
    let machine_cache_default_targets = host_default_machine_cache_targets();
    let host_os = std::env::consts::OS;
    let host_arch = std::env::consts::ARCH;
    let provider_targets = host_relevant_targets(
        &required_targets_or_default(
            &lock.required.targets.provider,
            &provider_default_targets,
            host_os,
            host_arch,
        ),
        &provider_default_targets,
    );
    let runtime_targets = host_relevant_targets(
        &required_targets_or_default(
            &lock.required.targets.runtime,
            &runtime_default_targets,
            host_os,
            host_arch,
        ),
        &runtime_default_targets,
    );
    let image_targets = host_relevant_targets(
        &required_targets_or_default(
            &lock.required.targets.image,
            &image_default_targets,
            host_os,
            host_arch,
        ),
        &image_default_targets,
    );
    let machine_cache_targets = host_relevant_targets(
        &required_targets_or_default(
            &lock.required.targets.machine_cache,
            &machine_cache_default_targets,
            host_os,
            host_arch,
        ),
        &machine_cache_default_targets,
    );
    let allowed_managed_sources = allowed_source_types_for_profile(&lock, active_runtime_profile());

    let mut failures = Vec::<String>::new();

    for provider_id in &lock.required.provider_ids {
        for target in &provider_targets {
            let Some(entry) = manifest.providers.iter().find(|entry| {
                entry.id == *provider_id && entry.os == target.os && entry.arch == target.arch
            }) else {
                failures.push(format!(
                    "missing provider entry: {} ({}/{})",
                    provider_id, target.os, target.arch
                ));
                continue;
            };
            let command_path = bundle_dir.join(&entry.command);
            if !command_path.exists() {
                failures.push(format!(
                    "missing provider command file: {} ({}/{}) at {}",
                    provider_id,
                    target.os,
                    target.arch,
                    command_path.display()
                ));
            }
        }
    }

    for runtime_id in &lock.required.runtime_ids {
        for target in &runtime_targets {
            let runtime_component = find_required_component(&lock, "runtime", runtime_id, target);
            let managed_source_available = required_component_has_managed_source(
                &lock,
                "runtime",
                runtime_id,
                target,
                &allowed_managed_sources,
            );
            if *runtime_id == "avf-linux-guest"
                && managed_source_available
                && runtime_component
                    .map(|component| !avf_helper_metadata_complete(component))
                    .unwrap_or(true)
            {
                failures.push(format!(
                    "runtime lock missing AVF helper metadata: {} ({}/{})",
                    runtime_id, target.os, target.arch
                ));
            }
            let Some(entry) = manifest.runtimes.iter().find(|entry| {
                entry.id == *runtime_id && entry.os == target.os && entry.arch == target.arch
            }) else {
                if !managed_source_available {
                    failures.push(format!(
                        "missing runtime entry: {} ({}/{})",
                        runtime_id, target.os, target.arch
                    ));
                }
                continue;
            };
            let root_path = bundle_dir.join(&entry.root);
            if !root_path.exists() {
                if managed_source_available {
                    continue;
                }
                failures.push(format!(
                    "missing runtime root dir: {} ({}/{}) at {}",
                    runtime_id,
                    target.os,
                    target.arch,
                    root_path.display()
                ));
                continue;
            }
            let bin_path = root_path.join(&entry.bin);
            if !bin_path.exists() {
                failures.push(format!(
                    "missing runtime binary file: {} ({}/{}) at {}",
                    runtime_id,
                    target.os,
                    target.arch,
                    bin_path.display()
                ));
            }
            if *runtime_id == "avf-linux-guest" {
                for (helper_name, helper_rel) in avf_helper_names_and_paths() {
                    let helper_path = root_path.join(helper_rel);
                    if !helper_path.exists() {
                        failures.push(format!(
                            "missing AVF runtime helper file: {} {} ({}/{}) at {}",
                            runtime_id,
                            helper_name,
                            target.os,
                            target.arch,
                            helper_path.display()
                        ));
                    }
                }
            }
        }
    }

    for image_id in &lock.required.image_ids {
        for target in &image_targets {
            let managed_source_available = required_component_has_managed_source(
                &lock,
                "image",
                image_id,
                target,
                &allowed_managed_sources,
            );
            let Some(entry) = manifest.images.iter().find(|entry| {
                entry.id == *image_id && entry.os == target.os && entry.arch == target.arch
            }) else {
                if !managed_source_available {
                    failures.push(format!(
                        "missing image entry: {} ({}/{})",
                        image_id, target.os, target.arch
                    ));
                }
                continue;
            };
            let tar_path = bundle_dir.join(&entry.tar);
            if !tar_path.exists() && !managed_source_available {
                failures.push(format!(
                    "missing image tar file: {} ({}/{}) at {}",
                    image_id,
                    target.os,
                    target.arch,
                    tar_path.display()
                ));
            }
        }
    }

    for machine_cache_id in &lock.required.machine_cache_ids {
        for target in &machine_cache_targets {
            if !required_component_has_managed_source(
                &lock,
                "machine_cache",
                machine_cache_id,
                target,
                &allowed_managed_sources,
            ) {
                failures.push(format!(
                    "missing machine-cache managed source: {} ({}/{})",
                    machine_cache_id, target.os, target.arch
                ));
            }
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "desktop parity preflight failed (channel={channel} profile=parity surface={surface}): {}",
            failures.join("; ")
        );
    }
    Ok(())
}
