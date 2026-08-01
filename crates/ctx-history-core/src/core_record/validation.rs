use std::collections::BTreeMap;

use crate::{SourceKey, StableEntityId, StableEntityKind};

use super::{
    CoreRecordError, CoreRecordResult, MAX_METADATA_BYTES, MAX_REPOSITORY_RELATIVE_PATH_BYTES,
    MAX_TEXT_METADATA_BYTES,
};

pub(super) fn validate_owned_identity(
    identity: StableEntityId,
    expected_kind: StableEntityKind,
    source: &SourceKey,
) -> CoreRecordResult<()> {
    identity
        .validate_contract()
        .map_err(|_| CoreRecordError::InvalidIdentityRelationship)?;
    if identity.entity_kind() != expected_kind
        || identity.source_digest() != source.identity().digest()
        || identity.source_descriptor_digest() != source.exact_descriptor_digest()
    {
        return Err(CoreRecordError::InvalidIdentityRelationship);
    }
    Ok(())
}

pub(super) fn validate_related_session_identity(identity: StableEntityId) -> CoreRecordResult<()> {
    identity
        .validate_contract()
        .map_err(|_| CoreRecordError::InvalidIdentityRelationship)?;
    if identity.entity_kind() != StableEntityKind::Session {
        return Err(CoreRecordError::InvalidIdentityRelationship);
    }
    Ok(())
}

pub(super) fn validate_json_map(
    metadata: &BTreeMap<String, serde_json::Value>,
) -> CoreRecordResult<()> {
    for key in metadata.keys() {
        validate_text("metadata_key", key, MAX_TEXT_METADATA_BYTES)?;
    }
    let encoded = serde_json::to_vec(metadata)?;
    validate_size("metadata", encoded.len(), MAX_METADATA_BYTES)
        .map_err(|_| CoreRecordError::InvalidMetadata)
}

pub(super) fn validate_repository_alias_component(value: &str) -> CoreRecordResult<()> {
    if value.is_empty()
        || value.len() > MAX_TEXT_METADATA_BYTES
        || matches!(value, "." | "..")
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'\\' | b'@' | b':'))
    {
        return Err(CoreRecordError::InvalidRepositoryAlias);
    }
    Ok(())
}

pub(super) fn validate_repository_relative_path(path: &str) -> CoreRecordResult<()> {
    if path.is_empty()
        || path.len() > MAX_REPOSITORY_RELATIVE_PATH_BYTES
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
        || path
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || path.as_bytes().get(1).is_some_and(|second| *second == b':')
    {
        return Err(CoreRecordError::InvalidRepositoryRelativePath(
            path.to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> CoreRecordResult<()> {
    if value.is_empty() {
        return Err(CoreRecordError::EmptyField { field });
    }
    validate_size(field, value.len(), maximum)
}

pub(super) fn validate_optional_text(
    field: &'static str,
    value: Option<&str>,
    maximum: usize,
) -> CoreRecordResult<()> {
    if let Some(value) = value {
        validate_text(field, value, maximum)?;
    }
    Ok(())
}

pub(super) fn validate_size(
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> CoreRecordResult<()> {
    if actual > maximum {
        return Err(CoreRecordError::FieldTooLarge {
            field,
            actual,
            maximum,
        });
    }
    Ok(())
}

pub(super) fn validate_count(
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> CoreRecordResult<()> {
    if actual > maximum {
        return Err(CoreRecordError::TooManyItems {
            field,
            actual,
            maximum,
        });
    }
    Ok(())
}
