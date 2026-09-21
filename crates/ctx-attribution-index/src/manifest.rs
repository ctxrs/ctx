use serde::{Deserialize, Serialize};
use thiserror::Error;

use ctx_attribution_model::CoreMaterializationReceipt;

pub const MANIFEST_SCHEMA_VERSION: u16 = 1;
pub const MAX_MANIFEST_PLAINTEXT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SEGMENT_REFS: usize = 4_096;
pub const MAX_SEGMENT_PLAINTEXT_BYTES: u64 = 1024 * 1024 * 1024 * 1024;

const MAX_IDENTITY_BYTES: usize = 256;
const MAX_SEGMENT_FILE_NAME_BYTES: usize = 160;
const SEGMENT_FILE_PREFIX: &str = "attribution-segment-";
const SEGMENT_FILE_SUFFIX: &str = ".ctxs";

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("attribution manifest exceeds a bounded limit: {0}")]
    Bounds(&'static str),
    #[error("attribution manifest is invalid: {0}")]
    Invalid(&'static str),
    #[error("attribution manifest encoding is invalid")]
    Encoding(#[source] serde_json::Error),
    #[error("operating-system entropy is unavailable")]
    Entropy,
}

/// Complete metadata for one visible graph generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentManifest {
    pub schema_version: u16,
    pub generation_id: String,
    pub prior_generation_id: Option<String>,
    pub graph_generation: u64,
    pub core_receipt: CoreMaterializationReceipt,
    pub materializer_identity: String,
    pub schema_identity: String,
    pub evidence_identity: String,
    pub ordering_identity: String,
    pub segments: Vec<SegmentRef>,
    /// Exact segment reachability of the adjacent predecessor generation.
    /// Readers may have pinned these immutable files before active-manifest
    /// replacement, so restart collection retains this one durable set.
    pub predecessor_segments: Vec<SegmentRef>,
}

impl SegmentManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(ManifestError::Invalid("unsupported schema version"));
        }
        self.validate_contents()
    }

    #[allow(clippy::too_many_lines)]
    fn validate_contents(&self) -> Result<(), ManifestError> {
        decode_generation_id(&self.generation_id)?;
        if let Some(prior) = self.prior_generation_id.as_deref() {
            decode_generation_id(prior)?;
            if prior == self.generation_id {
                return Err(ManifestError::Invalid(
                    "current and prior generation IDs are equal",
                ));
            }
        }
        if self.graph_generation == 0 {
            return Err(ManifestError::Invalid("graph generation is zero"));
        }
        if (self.graph_generation == 1) != self.prior_generation_id.is_none() {
            return Err(ManifestError::Invalid(
                "graph generation and prior generation do not agree",
            ));
        }
        self.core_receipt
            .validate()
            .map_err(|_| ManifestError::Invalid("Core receipt is invalid"))?;
        validate_identity(&self.materializer_identity)?;
        validate_identity(&self.schema_identity)?;
        validate_identity(&self.evidence_identity)?;
        validate_identity(&self.ordering_identity)?;
        if self.materializer_identity != self.core_receipt.materializer_revision {
            return Err(ManifestError::Invalid(
                "materializer identity does not bind the Core receipt",
            ));
        }
        if self.segments.len() > MAX_SEGMENT_REFS {
            return Err(ManifestError::Bounds("too many segment references"));
        }
        if self.predecessor_segments.len() > MAX_SEGMENT_REFS {
            return Err(ManifestError::Bounds(
                "too many predecessor segment references",
            ));
        }
        if self
            .segments
            .first()
            .is_some_and(|segment| segment.publication_generation != self.graph_generation)
        {
            return Err(ManifestError::Invalid(
                "newest segment publication does not match the manifest generation",
            ));
        }
        let mut prior_publication_generation = None;
        for (index, segment) in self.segments.iter().enumerate() {
            let expected_ordinal = u32::try_from(index)
                .map_err(|_| ManifestError::Bounds("segment ordinal overflow"))?;
            segment.validate(expected_ordinal, self.graph_generation)?;
            match prior_publication_generation {
                None => {}
                Some(prior) if segment.publication_generation <= prior => {}
                Some(_) => {
                    return Err(ManifestError::Invalid(
                        "segment publication generations are not newest-first",
                    ));
                }
            }
            prior_publication_generation = Some(segment.publication_generation);
            if self.segments[..index]
                .iter()
                .any(|prior| prior.file_name == segment.file_name)
            {
                return Err(ManifestError::Invalid("duplicate segment file reference"));
            }
        }
        let predecessor_generation = self.graph_generation.saturating_sub(1);
        let mut prior_publication_generation = None;
        for (index, segment) in self.predecessor_segments.iter().enumerate() {
            let expected_ordinal = u32::try_from(index)
                .map_err(|_| ManifestError::Bounds("predecessor segment ordinal overflow"))?;
            segment.validate(expected_ordinal, predecessor_generation)?;
            match prior_publication_generation {
                None => {}
                Some(prior) if segment.publication_generation <= prior => {}
                Some(_) => {
                    return Err(ManifestError::Invalid(
                        "predecessor segment publication generations are not newest-first",
                    ));
                }
            }
            prior_publication_generation = Some(segment.publication_generation);
            if self.predecessor_segments[..index]
                .iter()
                .any(|prior| prior.file_name == segment.file_name)
            {
                return Err(ManifestError::Invalid(
                    "duplicate predecessor segment file reference",
                ));
            }
        }
        if self.prior_generation_id.is_none() && !self.predecessor_segments.is_empty() {
            return Err(ManifestError::Invalid(
                "first manifest has predecessor segment references",
            ));
        }
        Ok(())
    }

    pub fn generation_bytes(&self) -> Result<[u8; 32], ManifestError> {
        decode_generation_id(&self.generation_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentRef {
    /// Zero-based serving order. Readers visit lower ordinals first.
    pub ordinal: u32,
    /// Graph publication that created this immutable segment layer. Equal
    /// adjacent values are one unordered temporal layer.
    pub publication_generation: u64,
    pub generation_id: String,
    pub role: u32,
    pub file_name: String,
    pub plaintext_bytes: u64,
    pub file_sha256: String,
}

impl SegmentRef {
    fn validate(&self, expected_ordinal: u32, graph_generation: u64) -> Result<(), ManifestError> {
        if self.ordinal != expected_ordinal {
            return Err(ManifestError::Invalid(
                "segment references are not in canonical order",
            ));
        }
        if self.publication_generation == 0 || self.publication_generation > graph_generation {
            return Err(ManifestError::Invalid(
                "segment publication generation is outside the manifest generation",
            ));
        }
        decode_generation_id(&self.generation_id)?;
        if self.role == 0 {
            return Err(ManifestError::Invalid("segment role is reserved"));
        }
        if self.file_name.len() > MAX_SEGMENT_FILE_NAME_BYTES
            || self.file_name != segment_file_name(&self.generation_id, self.role)
        {
            return Err(ManifestError::Invalid("segment file name is invalid"));
        }
        if self.plaintext_bytes > MAX_SEGMENT_PLAINTEXT_BYTES {
            return Err(ManifestError::Bounds("segment plaintext is too large"));
        }
        decode_lower_hex::<32>(&self.file_sha256)
            .map_err(|()| ManifestError::Invalid("segment digest is invalid"))?;
        Ok(())
    }
}

pub fn encode_manifest(manifest: &SegmentManifest) -> Result<Vec<u8>, ManifestError> {
    manifest.validate()?;
    let encoded = serde_json::to_vec(manifest).map_err(ManifestError::Encoding)?;
    if encoded.is_empty() || encoded.len() > MAX_MANIFEST_PLAINTEXT_BYTES {
        return Err(ManifestError::Bounds("encoded manifest is too large"));
    }
    Ok(encoded)
}

pub fn decode_manifest(bytes: &[u8]) -> Result<SegmentManifest, ManifestError> {
    if bytes.is_empty() || bytes.len() > MAX_MANIFEST_PLAINTEXT_BYTES {
        return Err(ManifestError::Bounds("encoded manifest has an unsafe size"));
    }
    let manifest =
        serde_json::from_slice::<SegmentManifest>(bytes).map_err(ManifestError::Encoding)?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn random_generation_id() -> Result<String, ManifestError> {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    Ok(hex::encode(bytes))
}

#[must_use]
pub fn segment_file_name(generation_id: &str, role: u32) -> String {
    format!("{SEGMENT_FILE_PREFIX}{generation_id}-{role:08x}{SEGMENT_FILE_SUFFIX}")
}

#[must_use]
pub fn parse_segment_file_name(value: &str) -> Option<([u8; 32], u32)> {
    let body = value
        .strip_prefix(SEGMENT_FILE_PREFIX)?
        .strip_suffix(SEGMENT_FILE_SUFFIX)?;
    let (generation, role) = body.split_once('-')?;
    if generation.len() != 64
        || role.len() != 8
        || !role
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    Some((
        decode_generation_id(generation).ok()?,
        u32::from_str_radix(role, 16).ok()?,
    ))
}

pub fn decode_generation_id(value: &str) -> Result<[u8; 32], ManifestError> {
    decode_lower_hex::<32>(value).map_err(|()| ManifestError::Invalid("generation ID is invalid"))
}

fn validate_identity(value: &str) -> Result<(), ManifestError> {
    if value.is_empty()
        || value.len() > MAX_IDENTITY_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\'))
    {
        return Err(ManifestError::Invalid("contract identity is invalid"));
    }
    Ok(())
}

fn decode_lower_hex<const N: usize>(value: &str) -> Result<[u8; N], ()> {
    if value.len() != N.saturating_mul(2)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(());
    }
    let decoded = hex::decode(value).map_err(|_| ())?;
    decoded.try_into().map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt() -> CoreMaterializationReceipt {
        CoreMaterializationReceipt {
            core_generation_id: "11".repeat(32),
            core_record_contract_fingerprint: "22".repeat(32),
            source_snapshot_sha256: "33".repeat(32),
            materializer_revision: "test-materializer-v1".to_owned(),
            source_count: 1,
            event_count: 2,
        }
    }

    fn manifest() -> SegmentManifest {
        SegmentManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            generation_id: "44".repeat(32),
            prior_generation_id: None,
            graph_generation: 1,
            core_receipt: receipt(),
            materializer_identity: "test-materializer-v1".to_owned(),
            schema_identity: "schema-v1".to_owned(),
            evidence_identity: "evidence-v1".to_owned(),
            ordering_identity: "ordering-v1".to_owned(),
            segments: Vec::new(),
            predecessor_segments: Vec::new(),
        }
    }

    #[test]
    fn manifest_decoder_denies_unknown_fields_and_bounds_input() {
        let manifest = manifest();
        let mut value = serde_json::to_value(&manifest).expect("serialize fixture");
        value
            .as_object_mut()
            .expect("manifest object")
            .insert("future_authority".to_owned(), serde_json::json!(true));
        let bytes = serde_json::to_vec(&value).expect("encode fixture");
        assert!(decode_manifest(&bytes).is_err());
        assert!(decode_manifest(&vec![b'x'; MAX_MANIFEST_PLAINTEXT_BYTES + 1]).is_err());
    }

    #[test]
    fn manifest_binds_receipt_and_canonical_segment_order() {
        let mut manifest = manifest();
        manifest.materializer_identity = "different-materializer".to_owned();
        assert!(manifest.validate().is_err());

        manifest.materializer_identity = manifest.core_receipt.materializer_revision.clone();
        let generation_id = "55".repeat(32);
        manifest.segments.push(SegmentRef {
            ordinal: 1,
            publication_generation: 1,
            generation_id: generation_id.clone(),
            role: 7,
            file_name: segment_file_name(&generation_id, 7),
            plaintext_bytes: 0,
            file_sha256: "66".repeat(32),
        });
        assert!(manifest.validate().is_err());
        manifest.segments[0].ordinal = 0;
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn manifest_requires_current_generation_then_newest_first_layers() {
        let reference = |publication_generation, byte: u8| {
            let generation_id = format!("{byte:02x}").repeat(32);
            SegmentRef {
                ordinal: 0,
                publication_generation,
                generation_id: generation_id.clone(),
                role: 7,
                file_name: segment_file_name(&generation_id, 7),
                plaintext_bytes: 0,
                file_sha256: "66".repeat(32),
            }
        };
        let mut manifest = manifest();
        manifest.graph_generation = 2;
        manifest.prior_generation_id = Some("33".repeat(32));
        manifest.segments = vec![reference(1, 0x51), reference(2, 0x52)];
        manifest.segments[1].ordinal = 1;
        assert!(manifest.validate().is_err());

        manifest.segments = vec![reference(1, 0x50)];
        assert!(manifest.validate().is_err());

        manifest.segments = vec![reference(2, 0x53), reference(2, 0x54)];
        manifest.segments[1].ordinal = 1;
        assert!(manifest.validate().is_ok());

        manifest.segments = vec![reference(2, 0x55), reference(1, 0x56)];
        manifest.segments[1].ordinal = 1;
        assert!(manifest.validate().is_ok());
    }
}
