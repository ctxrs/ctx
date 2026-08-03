use super::*;
use std::{collections::HashSet, path::Component};

pub(super) fn validate_approved_path(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!(
            "explicit source request paths must be absolute: {}",
            path.display()
        );
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!(
            "explicit source request paths must be normalized: {}",
            path.display()
        );
    }
    let text = path.to_str().ok_or_else(|| {
        anyhow!(
            "explicit source request paths must be valid UTF-8: {}",
            path.display()
        )
    })?;
    if text.len() > CATALOG_MAX_PATH_BYTES {
        bail!(
            "explicit source request path exceeds {CATALOG_MAX_PATH_BYTES} bytes: {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn sort_and_validate_entries(entries: &mut [CatalogEntry]) -> Result<()> {
    if entries.len() > CATALOG_MAX_ENTRIES {
        bail!("explicit source request exceeds its {CATALOG_MAX_ENTRIES}-entry bound");
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
        if !entry.enabled {
            bail!("explicit source request overlays cannot authorize deletion");
        }
        validate_approved_path(&entry.path)?;
        let provider = entry.provider()?;
        let lineage = entry.lineage()?;
        if let Some(route_identity) = entry.route_identity.as_ref() {
            ctx_history_index::SourceRouteIdentity::from_sha256(route_identity.clone())
                .context("validate preserved explicit relocation route identity")?;
        }
        if !lineages.insert(lineage) {
            bail!("explicit source request contains duplicate catalog lineage");
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
                "explicit source request contains duplicate {}/{} authority",
                provider.as_str(),
                metadata.certified_source_format
            );
        }
    }
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
        entries: entries.to_vec(),
    })
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
        bail!("request overlay digest must be 64 hexadecimal characters");
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
        _ => bail!("invalid hexadecimal request overlay digest"),
    }
}
