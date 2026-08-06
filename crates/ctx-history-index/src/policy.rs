//! Canonical policies for self-contained Core and semantic generations.
//!
//! The compact JSON encoding of [`SourceGenerationPolicy`] is hashed into each
//! lexical generation manifest. It intentionally excludes semantic eligibility,
//! chunking, and model policy: semantic state carries and validates that
//! independent policy through [`SemanticGenerationPolicy`].

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SOURCE_GENERATION_POLICY_VERSION: u32 = 10;
pub const SOURCE_ROUTE_SNAPSHOT_REVISION: u32 = 1;
pub const AUTOMATIC_ROUTE_DELETION_GRACE_OBSERVATIONS: u32 = 3;
pub const LEXICAL_SCHEMA_REVISION: u32 = 18;
pub const LEXICAL_TOKENIZER_REVISION: u32 = 2;
pub const SOURCE_EVENT_PROJECTOR_REVISION: u32 = 4;
pub const LEXICAL_INDEXED_BODY_LIMIT: LexicalIndexedBodyLimit =
    LexicalIndexedBodyLimit::ProviderValidatedFullText;
pub const SEMANTIC_ELIGIBILITY_REVISION: u32 = 3;
pub const SEMANTIC_CHUNKING_REVISION: u32 = 1;
pub const SEMANTIC_CHUNK_TARGET_CHARS: usize = 1_200;
pub const SEMANTIC_CHUNK_OVERLAP_CHARS: usize = 200;
pub const SEMANTIC_SOURCE_MAX_CHARS: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceGenerationPolicy {
    pub policy_version: u32,
    pub source_lifecycle: SourceLifecyclePolicy,
    pub lexical: LexicalGenerationPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLifecyclePolicy {
    pub route_snapshot_revision: u32,
    pub automatic_route_deletion_grace_observations: u32,
}

impl SourceGenerationPolicy {
    /// Returns the SHA-256 of this policy's compact declaration-order JSON.
    pub fn canonical_sha256(&self) -> serde_json::Result<String> {
        let digest = Sha256::digest(serde_json::to_vec(self)?);
        Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

impl SemanticGenerationPolicy {
    /// Returns the SHA-256 of this semantic policy's compact declaration-order JSON.
    pub fn canonical_sha256(&self) -> serde_json::Result<String> {
        let digest = Sha256::digest(serde_json::to_vec(self)?);
        Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    /// Returns whether current semantic metadata policy selects this stored
    /// Core event. This decision is semantic-owned and never participates in
    /// Core generation compatibility or identity.
    pub fn includes_event(&self, event_type: &str, role: Option<&str>) -> bool {
        let event_class = match event_type {
            "message" => SourceEventClass::Message,
            "tool_call" => SourceEventClass::ToolCall,
            "tool_output" => SourceEventClass::ToolOutput,
            "command_started" => SourceEventClass::CommandStarted,
            "command_output" => SourceEventClass::CommandOutput,
            "command_finished" => SourceEventClass::CommandFinished,
            "file_touched" => SourceEventClass::FileTouched,
            "vcs_change" => SourceEventClass::VcsChange,
            "artifact" => SourceEventClass::Artifact,
            "summary" => SourceEventClass::Summary,
            "notice" => SourceEventClass::Notice,
            _ => return false,
        };
        let role = match role {
            Some("user") => SourceEventRole::User,
            Some("assistant") => SourceEventRole::Assistant,
            _ => return false,
        };
        self.candidate_event_classes.contains(&event_class) && self.candidate_roles.contains(&role)
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
    pub core_repository_association_policy_revision: u32,
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
    Assistant,
}

pub fn current_source_generation_policy() -> SourceGenerationPolicy {
    SourceGenerationPolicy {
        policy_version: SOURCE_GENERATION_POLICY_VERSION,
        source_lifecycle: SourceLifecyclePolicy {
            route_snapshot_revision: SOURCE_ROUTE_SNAPSHOT_REVISION,
            automatic_route_deletion_grace_observations:
                AUTOMATIC_ROUTE_DELETION_GRACE_OBSERVATIONS,
        },
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
            core_repository_association_policy_revision:
                ctx_history_core::CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
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
    }
}

pub fn current_source_generation_policy_hash() -> serde_json::Result<String> {
    current_source_generation_policy().canonical_sha256()
}

pub fn current_semantic_generation_policy() -> SemanticGenerationPolicy {
    let model = ctx_semantic_model::semantic_model_contract();
    SemanticGenerationPolicy {
        eligibility_revision: SEMANTIC_ELIGIBILITY_REVISION,
        candidate_event_classes: [SourceEventClass::Message],
        candidate_roles: [SourceEventRole::User],
        core_content_filter: SemanticCoreContentFilter::PolicySelectedMeaningfulTextV1,
        chunking_revision: SEMANTIC_CHUNKING_REVISION,
        chunk_target_chars: SEMANTIC_CHUNK_TARGET_CHARS as u32,
        chunk_overlap_chars: SEMANTIC_CHUNK_OVERLAP_CHARS as u32,
        source_max_chars: SEMANTIC_SOURCE_MAX_CHARS as u32,
        embedding: EmbeddingGenerationPolicy {
            contract_revision: model.contract_revision(),
            model: model.model_id().to_owned(),
            model_revision: model.model_revision().to_owned(),
            dimensions: model.dimensions() as u32,
            normalization: model.normalization().to_owned(),
        },
    }
}

pub fn current_semantic_generation_policy_hash() -> serde_json::Result<String> {
    current_semantic_generation_policy().canonical_sha256()
}

pub(crate) fn is_semantic_candidate(event_type: &str, role: Option<&str>) -> bool {
    current_semantic_generation_policy().includes_event(event_type, role)
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
        assert!(serde_json::to_value(&first).unwrap()["lexical"]
            .as_object()
            .unwrap()
            .contains_key("core_repository_association_policy_revision"));
        assert_eq!(first.policy_version, 10);
        assert_eq!(first.lexical.event_projector_revision, 4);
        assert_eq!(first.lexical.schema_revision, 18);
        assert_eq!(first.lexical.tokenizer_revision, 2);
        assert_eq!(
            first.canonical_sha256().unwrap(),
            "32f3851e10da262f6e9f71467e1579f295eab84e353916ddaabaf7bfe409c3f4"
        );
    }

    #[test]
    fn generation_affecting_field_changes_policy_hash() {
        let current = current_source_generation_policy();
        let mut changed = current.clone();
        changed.lexical.core_repository_association_policy_revision += 1;

        assert_ne!(
            current.canonical_sha256().unwrap(),
            changed.canonical_sha256().unwrap()
        );
    }

    #[test]
    fn semantic_policy_changes_do_not_change_core_generation_policy() {
        let core_policy_hash = current_source_generation_policy_hash().unwrap();
        let current_semantic = current_semantic_generation_policy();
        let mut changed_semantic = current_semantic.clone();
        changed_semantic.eligibility_revision += 1;

        assert_ne!(
            current_semantic.canonical_sha256().unwrap(),
            changed_semantic.canonical_sha256().unwrap()
        );
        assert_eq!(
            current_source_generation_policy_hash().unwrap(),
            core_policy_hash
        );
        assert!(
            serde_json::to_value(current_source_generation_policy())
                .unwrap()
                .get("semantic")
                .is_none(),
            "Core generation policy must not carry semantic compatibility identity"
        );
    }

    #[test]
    fn semantic_policy_persisted_bytes_and_model_authority_are_frozen() {
        const EXPECTED: &str = "{\"eligibility_revision\":2,\"candidate_event_classes\":[\"message\"],\"candidate_roles\":[\"user\"],\"core_content_filter\":\"policy_selected_meaningful_text_v1\",\"chunking_revision\":1,\"chunk_target_chars\":1200,\"chunk_overlap_chars\":200,\"source_max_chars\":65536,\"embedding\":{\"contract_revision\":2,\"model\":\"intfloat/multilingual-e5-small\",\"model_revision\":\"614241f622f53c4eeff9890bdc4f31cfecc418b3\",\"dimensions\":384,\"normalization\":\"l2\"}}";
        let policy = current_semantic_generation_policy();
        let contract = ctx_semantic_model::semantic_model_contract();
        let persisted = serde_json::to_string(&policy).unwrap();
        assert_eq!(EXPECTED.len(), 424);
        assert_eq!(persisted.len(), 424);
        assert_eq!(persisted, EXPECTED);
        assert_eq!(
            policy.canonical_sha256().unwrap(),
            "821211bd0eb0f0a73427c9c1e325a5503e765393f33b41f21d4a57323ed1fff0"
        );
        assert_eq!(
            policy.embedding.contract_revision,
            contract.contract_revision()
        );
        assert_eq!(policy.embedding.model, contract.model_id());
        assert_eq!(policy.embedding.model_revision, contract.model_revision());
        assert_eq!(policy.embedding.dimensions, contract.dimensions() as u32);
        assert_eq!(policy.embedding.normalization, contract.normalization());
    }
}
