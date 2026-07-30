use super::*;
use std::{collections::HashSet, path::Component};

pub(super) fn validate_approved_path(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!(
            "explicit source catalog paths must be absolute: {}",
            path.display()
        );
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!(
            "explicit source catalog paths must be normalized: {}",
            path.display()
        );
    }
    let text = path.to_str().ok_or_else(|| {
        anyhow!(
            "explicit source catalog paths must be valid UTF-8: {}",
            path.display()
        )
    })?;
    if text.len() > CATALOG_MAX_PATH_BYTES {
        bail!(
            "explicit source catalog path exceeds {CATALOG_MAX_PATH_BYTES} bytes: {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn sort_and_validate_entries(entries: &mut [CatalogEntry]) -> Result<()> {
    if entries.len() > CATALOG_MAX_ENTRIES {
        bail!("explicit source catalog exceeds its {CATALOG_MAX_ENTRIES}-entry bound");
    }
    entries.sort_by(|left, right| {
        (
            left.provider.as_str(),
            left.source_format.as_str(),
            left.catalog_lineage.as_str(),
        )
            .cmp(&(
                right.provider.as_str(),
                right.source_format.as_str(),
                right.catalog_lineage.as_str(),
            ))
    });
    let mut lineages = HashSet::new();
    let mut authorities = HashSet::new();
    for entry in entries.iter() {
        validate_approved_path(&entry.path)?;
        let provider = entry.provider()?;
        let lineage = entry.lineage()?;
        if !lineages.insert(lineage) {
            bail!("explicit source catalog contains duplicate catalog lineage");
        }
        let metadata = entry.route_metadata()?;
        if !metadata.explicit_manual || metadata.unsupported_reason.is_some() {
            bail!(
                "{} source format `{}` is not an enabled explicit source-backed contract",
                provider.as_str(),
                entry.source_format
            );
        }
        if !authorities.insert((provider, metadata.certified_source_format)) {
            bail!(
                "explicit source catalog contains duplicate {}/{} authority",
                provider.as_str(),
                metadata.certified_source_format
            );
        }
    }
    Ok(())
}

pub(super) fn catalog_root(data_root: &Path) -> PathBuf {
    data_root.join(CATALOG_DIRECTORY)
}

pub(super) fn load_catalog(data_root: &Path) -> Result<ExplicitSourceCatalogSnapshot> {
    let root = catalog_root(data_root);
    if !root
        .try_exists()
        .with_context(|| format!("check explicit source catalog directory {}", root.display()))?
    {
        return ExplicitSourceCatalogSnapshot::empty();
    }
    let lock = open_catalog_lock(&root, false)?;
    FileExt::lock_shared(&lock).context("lock explicit source catalog for read")?;
    load_catalog_unlocked(&root)
}

pub(super) fn load_catalog_for_authority(
    data_root: &Path,
    authority: &ExplicitSourceCatalogAuthority,
) -> Result<ExplicitSourceCatalogSnapshot> {
    let root = catalog_root(data_root);
    if !root
        .try_exists()
        .with_context(|| format!("check explicit source catalog directory {}", root.display()))?
    {
        let empty = ExplicitSourceCatalogSnapshot::empty()?;
        if &empty.authority == authority {
            return Ok(empty);
        }
        bail!(
            "explicit source catalog authority revision {} is unavailable",
            authority.revision
        );
    }
    let lock = open_catalog_lock(&root, false)?;
    FileExt::lock_shared(&lock).context("lock explicit source catalog for authority read")?;
    let path = root.join(catalog_revision_filename(authority.revision));
    let snapshot = load_catalog_revision(&path, authority.revision)?;
    if &snapshot.authority != authority {
        bail!(
            "explicit source catalog revision {} does not match the requested integrity authority",
            authority.revision
        );
    }
    Ok(snapshot)
}

pub(super) fn load_catalog_unlocked(root: &Path) -> Result<ExplicitSourceCatalogSnapshot> {
    if !root.exists() {
        return ExplicitSourceCatalogSnapshot::empty();
    }
    let mut revisions = Vec::new();
    for item in fs::read_dir(root)
        .with_context(|| format!("read explicit source catalog {}", root.display()))?
    {
        let item = item?;
        let name = item.file_name();
        let name = name.to_str().ok_or_else(|| {
            anyhow!("explicit source catalog contains a non-UTF-8 state filename")
        })?;
        if name == CATALOG_LOCK_FILE
            || (name.starts_with(CATALOG_STAGING_PREFIX) && name.ends_with(CATALOG_STAGING_SUFFIX))
        {
            continue;
        }
        let revision = parse_catalog_revision_filename(name)
            .ok_or_else(|| anyhow!("unexpected explicit source catalog state file `{name}`"))?;
        revisions.push((revision, item.path()));
    }
    let Some((filename_revision, path)) =
        revisions.into_iter().max_by_key(|(revision, _)| *revision)
    else {
        return ExplicitSourceCatalogSnapshot::empty();
    };
    load_catalog_revision(&path, filename_revision)
}

fn load_catalog_revision(
    path: &Path,
    filename_revision: u64,
) -> Result<ExplicitSourceCatalogSnapshot> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect explicit source catalog {}", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!(
            "explicit source catalog revision is not a regular file: {}",
            path.display()
        );
    }
    if metadata.len() > CATALOG_MAX_BYTES {
        bail!(
            "explicit source catalog {} exceeds its {CATALOG_MAX_BYTES}-byte bound",
            path.display()
        );
    }
    let file = File::open(&path)
        .with_context(|| format!("open explicit source catalog {}", path.display()))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(CATALOG_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read explicit source catalog {}", path.display()))?;
    if bytes.len() as u64 > CATALOG_MAX_BYTES {
        bail!(
            "explicit source catalog {} exceeds its {CATALOG_MAX_BYTES}-byte bound",
            path.display()
        );
    }
    let mut wire: CatalogFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("decode explicit source catalog {}", path.display()))?;
    if wire.schema_version != CATALOG_SCHEMA_VERSION {
        bail!(
            "unsupported explicit source catalog schema {}",
            wire.schema_version
        );
    }
    if wire.revision != filename_revision {
        bail!(
            "explicit source catalog filename revision {filename_revision} does not match body revision {}",
            wire.revision
        );
    }
    if wire.integrity.algorithm != CATALOG_INTEGRITY_ALGORITHM {
        bail!(
            "unsupported explicit source catalog integrity algorithm `{}`",
            wire.integrity.algorithm
        );
    }
    let expected = authority_for(wire.revision, &wire.entries)?;
    if decode_digest(&wire.integrity.digest)? != expected.integrity_sha256 {
        bail!(
            "explicit source catalog integrity check failed for {}",
            path.display()
        );
    }
    let original = wire.entries.clone();
    sort_and_validate_entries(&mut wire.entries)?;
    if wire.entries != original {
        bail!("explicit source catalog entries are not in canonical order");
    }
    Ok(ExplicitSourceCatalogSnapshot {
        authority: expected,
        entries: wire.entries,
    })
}

pub(super) fn open_catalog_lock(root: &Path, create: bool) -> Result<File> {
    let path = root.join(CATALOG_LOCK_FILE);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&path)
        .with_context(|| format!("open explicit source catalog lock {}", path.display()))
}

pub(super) fn write_catalog_snapshot(
    root: &Path,
    snapshot: &ExplicitSourceCatalogSnapshot,
) -> Result<()> {
    let path = root.join(catalog_revision_filename(snapshot.authority.revision));
    if path.exists() {
        bail!(
            "explicit source catalog revision already exists: {}",
            path.display()
        );
    }
    let wire = CatalogFile {
        schema_version: CATALOG_SCHEMA_VERSION,
        revision: snapshot.authority.revision,
        entries: snapshot.entries.clone(),
        integrity: CatalogIntegrity {
            algorithm: CATALOG_INTEGRITY_ALGORITHM.to_owned(),
            digest: snapshot.authority.integrity_hex(),
        },
    };
    let mut bytes = serde_json::to_vec_pretty(&wire)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > CATALOG_MAX_BYTES {
        bail!("explicit source catalog revision would exceed its {CATALOG_MAX_BYTES}-byte bound");
    }
    let staged = root.join(format!(
        "{CATALOG_STAGING_PREFIX}{}{CATALOG_STAGING_SUFFIX}",
        Uuid::new_v4()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&staged)
        .with_context(|| format!("create staged explicit source catalog {}", staged.display()))?;
    let write_result = (|| -> Result<()> {
        file.write_all(&bytes)
            .context("write staged explicit source catalog")?;
        file.sync_all()
            .context("sync staged explicit source catalog")?;
        fs::rename(&staged, &path).with_context(|| {
            format!(
                "publish explicit source catalog revision {}",
                snapshot.authority.revision
            )
        })?;
        sync_catalog_directory(root)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    write_result
}

#[cfg(unix)]
fn sync_catalog_directory(root: &Path) -> Result<()> {
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync explicit source catalog directory {}", root.display()))
}

#[cfg(not(unix))]
fn sync_catalog_directory(_root: &Path) -> Result<()> {
    Ok(())
}

pub(super) fn authority_for(
    revision: u64,
    entries: &[CatalogEntry],
) -> Result<ExplicitSourceCatalogAuthority> {
    let payload = serde_json::to_vec(&CatalogPayload {
        schema_version: CATALOG_SCHEMA_VERSION,
        revision,
        entries,
    })?;
    let mut digest = Sha256::new();
    digest.update(b"ctx.explicit-source-catalog-v1\0");
    digest.update(payload);
    Ok(ExplicitSourceCatalogAuthority {
        schema_version: CATALOG_SCHEMA_VERSION,
        revision,
        integrity_sha256: digest.finalize().into(),
    })
}

pub(super) fn catalog_revision_filename(revision: u64) -> String {
    format!("{CATALOG_FILE_PREFIX}{revision:020}{CATALOG_FILE_SUFFIX}")
}

fn parse_catalog_revision_filename(name: &str) -> Option<u64> {
    let revision = name
        .strip_prefix(CATALOG_FILE_PREFIX)?
        .strip_suffix(CATALOG_FILE_SUFFIX)?;
    if revision.len() != 20 || !revision.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    revision.parse().ok()
}

pub(super) fn random_catalog_lineage() -> [u8; 32] {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut lineage = [0_u8; 32];
    lineage[..16].copy_from_slice(first.as_bytes());
    lineage[16..].copy_from_slice(second.as_bytes());
    lineage
}

pub(super) fn encode_hex(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

pub(super) fn decode_digest(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("catalog digest must be 64 hexadecimal characters");
    }
    let mut decoded = [0_u8; 32];
    for (index, output) in decoded.iter_mut().enumerate() {
        let offset = index * 2;
        *output = (decode_nibble(value.as_bytes()[offset])? << 4)
            | decode_nibble(value.as_bytes()[offset + 1])?;
    }
    Ok(decoded)
}

fn decode_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => bail!("invalid hexadecimal catalog digest"),
    }
}
