//! Immutable persisted lexical-index format and trust contracts.
//!
//! This crate is the sole owner of schema field identities, analyzers,
//! tokenization, canonical document projection, generation manifests, and
//! structural/logical verification. It has no query or writer dependency.

mod analyzer;
mod contracts;
mod core_contract;
mod index_document;
mod manifest;
pub mod policy;
mod record_digest;
mod schema;
#[doc(hidden)]
pub mod search_projection;
mod source_identity;
mod stored_document;
mod verification;
mod verification_record;

pub use contracts::{
    AppliedProviderRoot, AppliedProviderRootSourceMembership,
    CommittedPredecessorMigrationRecovery, ConsecutiveSourceMissingCount,
    DetachedReleasedProviderRootAuthority, GenerationManifest, IndexError,
    ProviderRootConnectorBinding, Result, SourceCoreRecordAggregate, SourceMissingObservationPoint,
    SourceRouteMissingState, SourceRouteSnapshot, GENERATION_MANIFEST_VERSION,
    LEXICAL_ANALYZER_VERSION, LEXICAL_SCHEMA_VERSION, LEXICAL_SEGMENT_MERGE_FAN_IN,
    MAX_DETACHED_RELEASED_PROVIDER_ROOTS, MAX_PUBLICATION_METADATA_BYTES,
};
pub use ctx_history_capture_model::{
    ProviderRootDefinition, ProviderRootKind, ProviderRootSourceIdentity, SourceRouteIdentity,
};
#[doc(hidden)]
pub use policy::is_semantic_candidate;
pub use policy::{
    current_semantic_generation_policy, current_semantic_generation_policy_hash,
    current_source_generation_policy, current_source_generation_policy_hash,
    EmbeddingGenerationPolicy, LexicalBodySelection, LexicalGenerationPolicy,
    LexicalIndexedBodyLimit, SemanticCoreContentFilter, SemanticEventCopyFilter,
    SemanticGenerationPolicy, SourceEventClass, SourceEventRole, SourceGenerationPolicy,
    StoredSourceContent, LEXICAL_INDEXED_BODY_LIMIT, LEXICAL_SCHEMA_REVISION,
    LEXICAL_TOKENIZER_REVISION, SEMANTIC_CHUNK_OVERLAP_CHARS, SEMANTIC_CHUNK_TARGET_CHARS,
    SEMANTIC_SOURCE_MAX_CHARS,
};
pub use search_projection::project_body_search;

#[doc(hidden)]
pub use analyzer::{body_analyzer, register_body_analyzer, BODY_ANALYZER};
#[doc(hidden)]
pub use contracts::{
    implicit_source_routes, CommitPayload, COMMIT_PAYLOAD_VERSION, INDEX_MEMORY_MIN_PER_THREAD,
    LEXICAL_DELETED_DOCUMENT_RECLAIM_DENOMINATOR, LEXICAL_DELETED_DOCUMENT_RECLAIM_NUMERATOR,
    MAX_DOCUMENT_METADATA_BYTES,
};
#[doc(hidden)]
pub use core_contract::{
    current_core_record_contract_fingerprint, expected_source_generation_policy_hash,
    validate_core_contract_fingerprint,
};
#[doc(hidden)]
pub use ctx_history_capture_model::provider_source_config_digest;
#[doc(hidden)]
pub use index_document::{
    core_content_bytes, EventRangeOrderKey, IndexDocument, SemanticEventOrderKey,
    SessionAuthorityKey, SessionEventOrderKey, SourceEventOrderKey, EVENT_RANGE_ORDER_KEY_LEN,
    SEMANTIC_EVENT_ORDER_KEY_LEN, SESSION_AUTHORITY_KEY_LEN, SESSION_EVENT_ORDER_KEY_LEN,
    SOURCE_EVENT_ORDER_KEY_LEN, SOURCE_EVENT_ORDER_SIZE_SUFFIX_LEN,
    SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN,
};
#[doc(hidden)]
pub use manifest::{
    canonical_commit_payload, clear_manifest_cache_for_root, load_publication_for_metas,
    meta_generation, payload_generation_id, prepare_successor_manifest, reconcile_commit_error,
    searcher_generation, write_manifest, write_prepared_manifest, LoadedPublication,
    PreparedManifest,
};
#[doc(hidden)]
pub use record_digest::{accumulate_core_record, core_record_accumulator_leaf, core_record_leaf};
#[doc(hidden)]
pub use schema::{fields_from_schema, lexical_schema, required_field, validate_schema, Fields};
#[doc(hidden)]
pub use source_identity::{source_sort_key, source_token};
#[doc(hidden)]
pub use stored_document::{
    core_document_fast_facts, decode_core_document, decode_core_record_bytes,
    decode_owned_core_document, decode_validated_core_record_bytes, unique_required_bytes,
    validate_core_record_encoded_bytes, validated_core_record_bytes, AcceptedCoreDocument,
    CoreDocumentFastFacts,
};
#[doc(hidden)]
pub use verification::{
    load_active_publication_authority, open_pinned_publication, open_publication_candidate,
    verify_and_bind_publication_candidate, verify_and_bind_publication_candidate_with_progress,
    verify_and_bind_reusable_publication, verify_complete_searcher,
    verify_pinned_publication_authority, verify_publication_candidate, verify_searcher,
    verify_searcher_structure, ActivePublicationAuthority, CandidatePublicationVerificationError,
    EmptyPublicationIndex, OpenedPinnedPublication, OpenedPublicationCandidate, PinnedPublication,
    ReusablePublicationError, VerifiedCandidatePublication, VerifiedPublication,
};
#[doc(hidden)]
pub use verification_record::{
    stored_verification_record, validate_verification_projection, CompactIdentity,
    CompactVerificationIdentities, IdentityFieldRole, VerificationRecord,
};

#[doc(hidden)]
pub use ctx_history_index_generation::{
    certify_activated_generation, hex, is_generation_id, open_slot_index, physical_integrity_audit,
    physical_integrity_audit_with_candidate_proof, scrub_and_certify_physical_integrity,
    sha256_hex, verify_certified_physical_integrity, verify_or_certify_physical_integrity,
    verify_physical_integrity, ActiveGenerationPointer, CandidatePhysicalProof,
    CertifiedPhysicalIntegrity, DurableMmapDirectory, GenerationRetentionLease, GenerationSlot,
    PhysicalIntegrityAudit, MANIFEST_DIRECTORY,
};

#[cfg(any(test, feature = "test-support"))]
pub use verification::{
    candidate_identity_verification_activity, candidate_lineage_verification_activity,
    candidate_projection_verification_activity, complete_session_id_traversals,
    reset_verification_activity, verification_activity, verify_searcher_with_metrics,
};
