use ctx_history_core::{
    CertifiedSource, ProjectionContractError, ScannedSourceCounts, SourceKey, SourceObservation,
};
use sha2::{Digest, Sha256};

const LOGICAL_REVISION_DOMAIN: &[u8] = b"ctx-sqlite-logical-snapshot-v1\0";
const LOGICAL_REVISION_KIND: &str = "sqlite-logical-snapshot-v1";

/// Provider-defined logical evidence from one pinned SQLite transaction.
///
/// Physical DB/WAL evidence belongs to acquisition and is deliberately absent
/// here. The resulting observation is stable across checkpointing, sidecar
/// removal, page-layout changes, and `VACUUM` when the relevant schema and rows
/// are unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SqliteLogicalSnapshot {
    parser_revision: String,
    content_digest: [u8; 32],
    counts: ScannedSourceCounts,
    revision: [u8; 32],
}

impl SqliteLogicalSnapshot {
    pub(crate) fn new(
        parser_revision: impl Into<String>,
        schema_evidence: &[u8],
        content_digest: [u8; 32],
        counts: ScannedSourceCounts,
    ) -> Self {
        let parser_revision = parser_revision.into();
        let mut hasher = Sha256::new();
        hasher.update(LOGICAL_REVISION_DOMAIN);
        hash_bytes(&mut hasher, parser_revision.as_bytes());
        hash_bytes(&mut hasher, schema_evidence);
        hasher.update(content_digest);
        hash_counts(&mut hasher, counts);
        Self {
            parser_revision,
            content_digest,
            counts,
            revision: hasher.finalize().into(),
        }
    }

    pub(crate) fn certify(
        &self,
        source: SourceKey,
    ) -> Result<CertifiedSource, ProjectionContractError> {
        let observation =
            SourceObservation::new(source, LOGICAL_REVISION_KIND, self.revision.to_vec())?;
        CertifiedSource::certify(
            observation.clone(),
            observation,
            self.parser_revision.clone(),
            self.content_digest,
            self.counts,
        )
    }
}

fn hash_counts(hasher: &mut Sha256, counts: ScannedSourceCounts) {
    hasher.update(counts.complete_records.to_le_bytes());
    hasher.update(counts.retained_records.to_le_bytes());
    hasher.update(counts.rejected_records.to_le_bytes());
    hasher.update(counts.ignored_records.to_le_bytes());
    hasher.update(counts.indexed_documents.to_le_bytes());
    hasher.update(counts.certified_bytes.to_le_bytes());
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{CaptureProvider, SourceAnchor, TypedKey};

    use super::*;

    fn source() -> SourceKey {
        SourceKey::derive(
            CaptureProvider::OpenCode.as_str(),
            "opencode_sqlite",
            "logical-test-v1",
            1,
            SourceAnchor::provider_native("logical-test", TypedKey::utf8("database").unwrap())
                .unwrap(),
        )
        .unwrap()
    }

    fn counts() -> ScannedSourceCounts {
        ScannedSourceCounts {
            complete_records: 2,
            retained_records: 1,
            rejected_records: 1,
            ignored_records: 0,
            indexed_documents: 1,
            certified_bytes: 32,
        }
    }

    #[test]
    fn logical_certificate_has_no_frontier_and_is_repeatable() {
        let first = SqliteLogicalSnapshot::new("parser-v1", b"schema", [7; 32], counts());
        let second = SqliteLogicalSnapshot::new("parser-v1", b"schema", [7; 32], counts());
        let first_certificate = first.certify(source()).unwrap();
        let second_certificate = second.certify(source()).unwrap();

        assert_eq!(first, second);
        assert_eq!(first_certificate, second_certificate);
        assert!(first_certificate.frontier().is_none());
    }

    #[test]
    fn parser_schema_content_and_count_changes_replace() {
        let baseline = SqliteLogicalSnapshot::new("parser-v1", b"schema", [7; 32], counts());
        let changed_parser = SqliteLogicalSnapshot::new("parser-v2", b"schema", [7; 32], counts());
        let changed_schema =
            SqliteLogicalSnapshot::new("parser-v1", b"schema-2", [7; 32], counts());
        let changed_content = SqliteLogicalSnapshot::new("parser-v1", b"schema", [8; 32], counts());
        let mut changed_counts = counts();
        changed_counts.certified_bytes += 1;
        let changed_counts =
            SqliteLogicalSnapshot::new("parser-v1", b"schema", [7; 32], changed_counts);

        for changed in [
            changed_parser,
            changed_schema,
            changed_content,
            changed_counts,
        ] {
            assert_ne!(baseline, changed);
            assert_ne!(
                baseline.certify(source()).unwrap(),
                changed.certify(source()).unwrap()
            );
        }
    }
}
