//! Canonical policy for self-contained Core lexical and semantic generations.
//!
//! The compact JSON encoding of [`SourceGenerationPolicy`] is hashed into each
//! lexical generation manifest. Any generation-affecting policy change must
//! therefore change a field below (or its revision) so stale disposable
//! projections fail closed instead of being read under a different contract.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SOURCE_GENERATION_POLICY_VERSION: u32 = 5;
pub const LEXICAL_SCHEMA_REVISION: u32 = 10;
pub const LEXICAL_TOKENIZER_REVISION: u32 = 2;
pub const SOURCE_EVENT_PROJECTOR_REVISION: u32 = 3;
pub const LEXICAL_INDEXED_BODY_LIMIT: LexicalIndexedBodyLimit =
    LexicalIndexedBodyLimit::ProviderValidatedFullText;
pub const SEMANTIC_ELIGIBILITY_REVISION: u32 = 2;
pub const SEMANTIC_CHUNKING_REVISION: u32 = 1;
pub const SEMANTIC_CHUNK_TARGET_CHARS: usize = 1_200;
pub const SEMANTIC_CHUNK_OVERLAP_CHARS: usize = 200;
pub const SEMANTIC_SOURCE_MAX_CHARS: usize = 64 * 1024;
pub const SEMANTIC_EMBEDDING_CONTRACT_REVISION: u32 = 2;
pub const SEMANTIC_EMBEDDING_MODEL: &str = "intfloat/multilingual-e5-small";
pub const SEMANTIC_EMBEDDING_MODEL_REVISION: &str = "614241f622f53c4eeff9890bdc4f31cfecc418b3";
pub const SEMANTIC_EMBEDDING_DIMENSIONS: usize = 384;
pub const SEMANTIC_EMBEDDING_NORMALIZATION: &str = "l2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceGenerationPolicy {
    pub policy_version: u32,
    pub lexical: LexicalGenerationPolicy,
    pub semantic: SemanticGenerationPolicy,
}

impl SourceGenerationPolicy {
    /// Returns the SHA-256 of this policy's compact declaration-order JSON.
    pub fn canonical_sha256(&self) -> serde_json::Result<String> {
        let digest = Sha256::digest(serde_json::to_vec(self)?);
        Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LexicalGenerationPolicy {
    pub event_projector_revision: u32,
    pub core_record_version: u32,
    pub core_normalization_revision: u32,
    pub core_content_policy_revision: u32,
    pub core_repository_contract_revision: u32,
    pub core_repository_observation_revision: u32,
    pub core_bounded_shell_subset_revision: u32,
    pub core_repository_outcome_capture_revision: u32,
    pub core_repository_local_root_authorization_fingerprint_revision: u32,
    pub included_event_classes: [SourceEventClass; 11],
    pub body_selection: LexicalBodySelection,
    pub indexed_body_limit: LexicalIndexedBodyLimit,
    pub stored_content: StoredSourceContent,
    pub schema_revision: u32,
    pub tokenizer_revision: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticGenerationPolicy {
    /// Revision of the metadata-only candidate gate.
    pub eligibility_revision: u32,
    pub candidate_event_classes: [SourceEventClass; 1],
    pub candidate_roles: [SourceEventRole; 1],
    /// Filter applied after complete content is read from stored Core.
    pub core_content_filter: SemanticCoreContentFilter,
    pub chunking_revision: u32,
    pub chunk_target_chars: u32,
    pub chunk_overlap_chars: u32,
    pub source_max_chars: u32,
    pub embedding: EmbeddingGenerationPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingGenerationPolicy {
    pub contract_revision: u32,
    pub model: String,
    pub model_revision: String,
    pub dimensions: u32,
    pub normalization: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalBodySelection {
    FullPolicySelectedMeaningfulText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalIndexedBodyLimit {
    /// Index the complete meaningful text already selected and validated by
    /// the source projector, without another index-layer truncation.
    ProviderValidatedFullText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredSourceContent {
    CompleteCoreRecordV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticCoreContentFilter {
    PolicySelectedMeaningfulTextV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceEventClass {
    Message,
    ToolCall,
    ToolOutput,
    CommandStarted,
    CommandOutput,
    CommandFinished,
    FileTouched,
    VcsChange,
    Artifact,
    Summary,
    Notice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceEventRole {
    User,
}

pub fn current_source_generation_policy() -> SourceGenerationPolicy {
    SourceGenerationPolicy {
        policy_version: SOURCE_GENERATION_POLICY_VERSION,
        lexical: LexicalGenerationPolicy {
            event_projector_revision: SOURCE_EVENT_PROJECTOR_REVISION,
            core_record_version: ctx_history_core::CORE_RECORD_VERSION,
            core_normalization_revision: ctx_history_core::CORE_NORMALIZATION_REVISION,
            core_content_policy_revision: ctx_history_core::CORE_CONTENT_POLICY_REVISION,
            core_repository_contract_revision: ctx_history_core::CORE_REPOSITORY_CONTRACT_REVISION,
            core_repository_observation_revision:
                ctx_history_core::CORE_REPOSITORY_OBSERVATION_REVISION,
            core_bounded_shell_subset_revision:
                ctx_history_core::CORE_BOUNDED_SHELL_SUBSET_REVISION,
            core_repository_outcome_capture_revision:
                ctx_history_core::CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
            core_repository_local_root_authorization_fingerprint_revision:
                ctx_history_core::CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
            included_event_classes: [
                SourceEventClass::Message,
                SourceEventClass::ToolCall,
                SourceEventClass::ToolOutput,
                SourceEventClass::CommandStarted,
                SourceEventClass::CommandOutput,
                SourceEventClass::CommandFinished,
                SourceEventClass::FileTouched,
                SourceEventClass::VcsChange,
                SourceEventClass::Artifact,
                SourceEventClass::Summary,
                SourceEventClass::Notice,
            ],
            body_selection: LexicalBodySelection::FullPolicySelectedMeaningfulText,
            indexed_body_limit: LEXICAL_INDEXED_BODY_LIMIT,
            stored_content: StoredSourceContent::CompleteCoreRecordV1,
            schema_revision: LEXICAL_SCHEMA_REVISION,
            tokenizer_revision: LEXICAL_TOKENIZER_REVISION,
        },
        semantic: SemanticGenerationPolicy {
            eligibility_revision: SEMANTIC_ELIGIBILITY_REVISION,
            candidate_event_classes: [SourceEventClass::Message],
            candidate_roles: [SourceEventRole::User],
            core_content_filter: SemanticCoreContentFilter::PolicySelectedMeaningfulTextV1,
            chunking_revision: SEMANTIC_CHUNKING_REVISION,
            chunk_target_chars: SEMANTIC_CHUNK_TARGET_CHARS as u32,
            chunk_overlap_chars: SEMANTIC_CHUNK_OVERLAP_CHARS as u32,
            source_max_chars: SEMANTIC_SOURCE_MAX_CHARS as u32,
            embedding: EmbeddingGenerationPolicy {
                contract_revision: SEMANTIC_EMBEDDING_CONTRACT_REVISION,
                model: SEMANTIC_EMBEDDING_MODEL.to_owned(),
                model_revision: SEMANTIC_EMBEDDING_MODEL_REVISION.to_owned(),
                dimensions: SEMANTIC_EMBEDDING_DIMENSIONS as u32,
                normalization: SEMANTIC_EMBEDDING_NORMALIZATION.to_owned(),
            },
        },
    }
}

pub fn current_source_generation_policy_hash() -> serde_json::Result<String> {
    current_source_generation_policy().canonical_sha256()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_policy_hash_is_exact_and_deterministic() {
        let first = current_source_generation_policy();
        let second = current_source_generation_policy();

        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
        assert_eq!(
            first.canonical_sha256().unwrap(),
            second.canonical_sha256().unwrap()
        );
        assert!(serde_json::to_value(&first).unwrap()["lexical"]
            .as_object()
            .unwrap()
            .contains_key("core_repository_local_root_authorization_fingerprint_revision"));
        assert_eq!(
            first.canonical_sha256().unwrap(),
            "46da5ba5a0c714f6461d66b134c16156f2a19af9cc19ca924b108a1c46a1e626"
        );
    }

    #[test]
    fn generation_affecting_field_changes_policy_hash() {
        let current = current_source_generation_policy();
        let mut changed = current.clone();
        changed.semantic.chunk_target_chars += 1;

        assert_ne!(
            current.canonical_sha256().unwrap(),
            changed.canonical_sha256().unwrap()
        );
    }
}
