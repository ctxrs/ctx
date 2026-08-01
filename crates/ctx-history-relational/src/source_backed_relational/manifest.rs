use std::collections::BTreeMap;

use ctx_history_core::{core_record_contract_fingerprint, CORE_RECORD_VERSION};

use super::{CommittedCoreGeneration, RelationalProjectionError, RelationalSourceMetadata, Result};

pub(super) const GENERATION_MANIFEST_VERSION: u32 = 5;

pub(super) struct ValidatedGeneration {
    pub(super) sources: BTreeMap<String, RelationalSourceMetadata>,
    pub(super) indexed_documents: u64,
}

impl ValidatedGeneration {
    pub(super) fn from_commit(commit: &CommittedCoreGeneration) -> Result<Self> {
        validate_generation_id(&commit.generation_id)?;
        if commit.manifest_version != GENERATION_MANIFEST_VERSION {
            return invalid_generation(format!(
                "Core manifest version {} is unsupported; expected {}",
                commit.manifest_version, GENERATION_MANIFEST_VERSION
            ));
        }
        if commit.core_record_version != CORE_RECORD_VERSION
            || commit.core_record_contract_fingerprint != core_record_contract_fingerprint()
        {
            return invalid_generation("Core record revision does not match this materializer");
        }
        if commit.lexical_schema_version == 0 || commit.policy_schema_hash.is_empty() {
            return invalid_generation("Core generation lineage is incomplete");
        }

        let mut expected_events = 0_u64;
        let mut sources = BTreeMap::new();
        for source in &commit.sources {
            source
                .source
                .validate_contract()
                .map_err(|error| invalid_generation_error(error.to_string()))?;
            if source.parser_revision.is_empty() {
                return invalid_generation("source parser revision is empty");
            }
            expected_events = expected_events
                .checked_add(source.indexed_event_count)
                .ok_or(RelationalProjectionError::CountOverflow(
                    "Core source event count",
                ))?;
            let source_id = source.source.identity().as_uuid().to_string();
            if sources.insert(source_id.clone(), source.clone()).is_some() {
                return invalid_generation(format!("Core source {source_id} is duplicated"));
            }
        }
        if expected_events != commit.indexed_documents {
            return invalid_generation(format!(
                "Core source counts total {expected_events}, generation declares {}",
                commit.indexed_documents
            ));
        }
        Ok(Self {
            sources,
            indexed_documents: commit.indexed_documents,
        })
    }
}

fn validate_generation_id(generation_id: &str) -> Result<()> {
    if generation_id.len() != 64
        || !generation_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return invalid_generation("generation ID is not a lowercase SHA-256 value");
    }
    Ok(())
}

pub(super) fn invalid_generation<T>(detail: impl Into<String>) -> Result<T> {
    Err(invalid_generation_error(detail))
}

fn invalid_generation_error(detail: impl Into<String>) -> RelationalProjectionError {
    RelationalProjectionError::InvalidCoreGeneration(detail.into())
}
