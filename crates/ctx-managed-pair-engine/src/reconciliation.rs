use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};

use super::{
    filesystem::{self, Entry, Layout, Slot},
    fix_forward::ContentIdentity,
    validate_sha256, MAX_INTEGRATION_BYTES,
};

/// Publishes opaque integration ownership as an immutable digest generation.
///
/// The caller holds the canonical installation lock. Marker authority stays
/// with the upgrade engine: this operation only makes the bytes available for
/// a later atomic marker update.
pub fn publish_managed_pair_integration_generation_under_installation_lock(
    install_root: &Path,
    integration_source: &Path,
) -> Result<(PathBuf, String)> {
    filesystem::validate_absolute_root(install_root, "managed-pair install root")?;
    let layout = Layout::open(install_root, false)?;
    let source = filesystem::external_entry(integration_source, "managed integration ownership")?;
    let observed = filesystem::read_regular(
        &source,
        MAX_INTEGRATION_BYTES,
        "managed integration ownership",
    )?;
    let expected = ContentIdentity::from_observed(&observed);
    let base = layout.target(Slot::Integration);
    let generation = base.sibling(
        format!(
            "{}.{}",
            base.file_name().unwrap().to_string_lossy(),
            expected.sha256
        )
        .into(),
    );
    publish_generation(&layout, &source, &generation, &expected)?;
    Ok((generation.as_ref().to_path_buf(), expected.sha256))
}

/// Removes only the exact prior marker-bound integration object.
///
/// The caller holds the canonical installation lock and has already replaced
/// marker authority with another generation.
pub fn remove_managed_pair_integration_binding_under_installation_lock(
    install_root: &Path,
    prior_path: &Path,
    prior_sha256: &str,
) -> Result<()> {
    filesystem::validate_absolute_root(install_root, "managed-pair install root")?;
    validate_sha256(prior_sha256, "managed integration ownership")?;
    let layout = Layout::open(install_root, false)?;
    let fixed = layout.target(Slot::Integration);
    let generation = fixed.sibling(
        format!(
            "{}.{}",
            fixed.file_name().unwrap().to_string_lossy(),
            prior_sha256
        )
        .into(),
    );
    let prior = if prior_path == fixed.as_ref() {
        fixed
    } else if prior_path == generation.as_ref() {
        generation
    } else {
        bail!("prior managed integration ownership path is not canonical")
    };
    let label = "prior managed integration ownership";
    let Some(stamp) = filesystem::stamp_optional(&prior, MAX_INTEGRATION_BYTES, label)? else {
        return Ok(());
    };
    if stamp.sha256 != prior_sha256 {
        bail!("prior managed integration ownership changed");
    }
    filesystem::remove_if_exact(&prior, &stamp, MAX_INTEGRATION_BYTES, label)?;
    layout.revalidate()
}

fn publish_generation(
    layout: &Layout,
    source: &Entry,
    generation: &Entry,
    expected: &ContentIdentity,
) -> Result<()> {
    let label = "managed integration ownership generation";
    if let Some(stamp) = filesystem::stamp_optional(generation, MAX_INTEGRATION_BYTES, label)? {
        if !expected.matches(&stamp) {
            bail!("managed integration ownership generation changed");
        }
        filesystem::protect_regular(generation, false, label)?;
        return layout.revalidate();
    }

    let temporary = generation
        .sibling(format!(".{}.new", generation.file_name().unwrap().to_string_lossy()).into());
    let stamp = match filesystem::stamp_temporary_optional(
        &temporary,
        MAX_INTEGRATION_BYTES,
        "managed integration ownership generation temporary",
    )? {
        Some(stamp) if expected.matches(&stamp) => stamp,
        Some(stamp) => {
            filesystem::remove_temporary_exact(
                &temporary,
                &stamp,
                MAX_INTEGRATION_BYTES,
                "managed integration ownership generation temporary",
            )?;
            copy_generation_source(source, &temporary, expected, label)?
        }
        None => copy_generation_source(source, &temporary, expected, label)?,
    };
    filesystem::durable_replace(&temporary, generation, &stamp, MAX_INTEGRATION_BYTES, label)?;
    filesystem::protect_regular(generation, false, label)?;
    if !filesystem::stamp_optional(generation, MAX_INTEGRATION_BYTES, label)?
        .as_ref()
        .is_some_and(|actual| expected.matches(actual))
    {
        bail!("published managed integration ownership generation changed");
    }
    layout.revalidate()
}

fn copy_generation_source(
    source: &Entry,
    temporary: &Entry,
    expected: &ContentIdentity,
    label: &str,
) -> Result<filesystem::FileStamp> {
    let source_stamp = filesystem::stamp_optional(
        source,
        MAX_INTEGRATION_BYTES,
        "managed integration ownership",
    )?
    .ok_or_else(|| anyhow!("managed integration ownership source disappeared"))?;
    if !expected.matches(&source_stamp) {
        bail!("managed integration ownership source changed");
    }
    filesystem::copy_exact(
        source,
        temporary,
        &source_stamp,
        MAX_INTEGRATION_BYTES,
        false,
        label,
    )
}
