use thiserror::Error;

use super::identity::StableEntityKind;

pub(super) const MAX_PROVIDER_BYTES: usize = 128;
pub(super) const MAX_SOURCE_FORMAT_BYTES: usize = 256;
pub(super) const MAX_SCHEMA_VARIANT_BYTES: usize = 256;
pub(super) const MAX_KEY_NAMESPACE_BYTES: usize = 256;
pub(super) const MAX_TYPED_KEY_BYTES: usize = 64 * 1024;
pub(super) const MAX_TYPED_KEY_COMPONENTS: usize = 256;
pub(super) const MAX_LOGICAL_KIND_BYTES: usize = 256;
pub(super) const MAX_LOCATOR_KIND_BYTES: usize = 256;
pub(super) const MAX_LOCATOR_BYTES: usize = 64 * 1024;
pub(super) const MAX_REVISION_KIND_BYTES: usize = 256;
pub(super) const MAX_REVISION_BYTES: usize = 4 * 1024;
pub(super) const MAX_PARSER_REVISION_BYTES: usize = 256;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProjectionContractError {
    #[error("{field} is empty")]
    EmptyField { field: &'static str },
    #[error("{field} is too large: {actual} bytes, maximum {maximum}")]
    FieldTooLarge {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("typed identity key has too many components: {actual}, maximum {maximum}")]
    TooManyKeyComponents { actual: usize, maximum: usize },
    #[error("source certification compared different sources")]
    SourceChanged,
    #[error("source descriptor changed within one scan or identity binding")]
    SourceDescriptorChanged,
    #[error("source revision changed while it was being scanned")]
    SourceRevisionChanged,
    #[error("source inventory authority changed while it was being scanned")]
    InventoryAuthorityChanged,
    #[error("source inventory revision changed while it was being scanned")]
    InventoryRevisionChanged,
    #[error("source inventory provider does not own the deleted source")]
    InventoryProviderMismatch,
    #[error("authoritative inventory still contains the source proposed for deletion")]
    InventoryContainsDeletedSource,
    #[error("authoritative inventory contains a duplicate source identity")]
    DuplicateInventorySource,
    #[error("scanned source counts do not reconcile")]
    CountMismatch,
    #[error("source frontier does not reconcile with the certified source")]
    FrontierMismatch,
    #[error("append proof does not match the committed source prefix")]
    AppendPrefixMismatch,
    #[error("append candidate regressed committed source counts")]
    AppendCountRegression,
    #[error("append candidate changed parser revision")]
    AppendParserChanged,
    #[error("revision-scoped positional identity requires an explicit revision scope")]
    RevisionScopeRequired,
    #[error("revision scope is only valid for revision-scoped positional identity")]
    UnexpectedRevisionScope,
    #[error("identity kind mismatch: expected {expected:?}, actual {actual:?}")]
    EntityKindMismatch {
        expected: StableEntityKind,
        actual: StableEntityKind,
    },
    #[error("serialized or supplied derived identity is invalid")]
    InvalidDerivedIdentity,
}

pub type ProjectionContractResult<T> = Result<T, ProjectionContractError>;

pub(super) fn encode_length_prefixed(target: &mut Vec<u8>, value: &[u8]) {
    target.extend_from_slice(&(value.len() as u64).to_be_bytes());
    target.extend_from_slice(value);
}

pub(super) fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> ProjectionContractResult<()> {
    validate_nonempty_bytes(field, value.as_bytes(), maximum)
}

pub(super) fn validate_nonempty_bytes(
    field: &'static str,
    value: &[u8],
    maximum: usize,
) -> ProjectionContractResult<()> {
    if value.is_empty() {
        return Err(ProjectionContractError::EmptyField { field });
    }
    validate_bytes(field, value, maximum)
}

pub(super) fn validate_bytes(
    field: &'static str,
    value: &[u8],
    maximum: usize,
) -> ProjectionContractResult<()> {
    if value.len() > maximum {
        return Err(ProjectionContractError::FieldTooLarge {
            field,
            actual: value.len(),
            maximum,
        });
    }
    Ok(())
}
