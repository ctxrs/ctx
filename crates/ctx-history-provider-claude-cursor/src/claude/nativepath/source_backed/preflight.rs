//! Bounded Claude source preflight and row-error classification.

use std::path::PathBuf;

use ctx_history_core::{
    derive_event_id, EventIdentityInput, NativeItemKey, ProjectionContractError, SourceKey,
    StableEntityId, TypedKey,
};
use ctx_history_jsonl::JsonlFamilyProjectorPreflightError;
use ctx_history_provider_runtime::CaptureError;
use sha2::{Digest, Sha256};

use super::{
    claude_annotation, contract, native_event_key_parts, Binding, JsonlReader, LOGICAL_EVENT_KIND,
    NATIVE_EVENT_KEY_NAMESPACE,
};
use crate::claude::nativepath::{
    record::{parse_native_record, ParsedClaudeRecord},
    rows::{ClaudePhysicalLocator, ClaudeRetainedRow, CLAUDE_MAX_RECORD_ROWS},
};

pub(super) const MAX_PREFLIGHT_EVENT_IDENTITIES: usize = 1_048_576;

pub(super) type ClaudePreflightError = JsonlFamilyProjectorPreflightError<CaptureError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ClaudePreflightIdentity {
    event_digest: [u8; 32],
    line_number: u64,
}

#[derive(Debug)]
pub(super) enum ClaudeRecordValidationError {
    InvalidNativeRecordId(ProjectionContractError),
    InvalidProviderCallId(ProjectionContractError),
}

impl ClaudeRecordValidationError {
    fn detail(self) -> String {
        match self {
            Self::InvalidNativeRecordId(error) => {
                format!("Claude native record identity is invalid: {error}")
            }
            Self::InvalidProviderCallId(error) => {
                format!("Claude provider call identity is invalid: {error}")
            }
        }
    }
}

#[derive(Debug)]
pub(super) enum ClaudeRowValidationError {
    Record(ClaudeRecordValidationError),
    Fatal(CaptureError),
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ClaudeRecordKeyField {
    NativeRecordId,
    ProviderCallId,
}

pub(super) fn classify_typed_record_key_error(
    field: ClaudeRecordKeyField,
    error: ProjectionContractError,
) -> ClaudeRowValidationError {
    match error {
        error @ (ProjectionContractError::EmptyField {
            field: "typed_key_utf8",
        }
        | ProjectionContractError::FieldTooLarge {
            field: "typed_key_utf8",
            ..
        }) => ClaudeRowValidationError::Record(match field {
            ClaudeRecordKeyField::NativeRecordId => {
                ClaudeRecordValidationError::InvalidNativeRecordId(error)
            }
            ClaudeRecordKeyField::ProviderCallId => {
                ClaudeRecordValidationError::InvalidProviderCallId(error)
            }
        }),
        error => ClaudeRowValidationError::Fatal(contract(error)),
    }
}

pub(super) fn typed_claude_record_key(
    field: ClaudeRecordKeyField,
    value: &str,
) -> std::result::Result<TypedKey, ClaudeRowValidationError> {
    TypedKey::utf8(value).map_err(|error| classify_typed_record_key_error(field, error))
}

pub(super) fn scope_claude_row_validation_error(
    error: ClaudeRowValidationError,
) -> ClaudePreflightError {
    match error {
        ClaudeRowValidationError::Record(error) => {
            ClaudePreflightError::record_rejection(error.detail())
        }
        ClaudeRowValidationError::Fatal(error) => ClaudePreflightError::internal(error),
    }
}

pub(super) fn checked_preflight_identity_count(
    current: usize,
    additional: usize,
) -> std::result::Result<usize, ClaudePreflightError> {
    current
        .checked_add(additional)
        .filter(|count| *count <= MAX_PREFLIGHT_EVENT_IDENTITIES)
        .ok_or_else(|| {
            ClaudePreflightError::internal(CaptureError::SystemInvariant(
                "Claude preflight event identity bound exceeded",
            ))
        })
}

pub(super) fn validate_source(
    reader: &mut JsonlReader,
    source_path: &str,
    binding: &Binding,
    source: &SourceKey,
    session_id: StableEntityId,
) -> std::result::Result<bool, ClaudePreflightError> {
    let mut identities = Vec::new();
    while reader
        .visit_page(
            &mut |record| -> std::result::Result<(), ClaudePreflightError> {
                if record.oversized() {
                    return Ok(());
                }
                let evidence = record.evidence();
                let ordinal = evidence.physical_ordinal();
                let line_number = ordinal.checked_add(1).ok_or_else(|| {
                    ClaudePreflightError::internal(CaptureError::SystemInvariant(
                        "Claude line number overflowed",
                    ))
                })?;
                let locator = ClaudePhysicalLocator {
                    path: PathBuf::from(source_path),
                    byte_start: evidence.byte_start(),
                    byte_end_exclusive: evidence.byte_end_exclusive(),
                    line_number,
                    record_sha256: Sha256::digest(record.bytes()).into(),
                };
                let Ok(parsed) = parse_native_record(record.bytes(), ordinal, &locator) else {
                    return Ok(());
                };
                if parsed_record_is_rejected(&parsed, binding) {
                    return Ok(());
                }

                let mut record_identities = Vec::new();
                record_identities
                    .try_reserve_exact(parsed.rows.len())
                    .map_err(|_| {
                        ClaudePreflightError::internal(CaptureError::SystemInvariant(
                            "Claude preflight record identity allocation failed",
                        ))
                    })?;
                for row in &parsed.rows {
                    match stable_native_event_identity(row, source, session_id) {
                        Ok(Some(event_id)) => record_identities.push(ClaudePreflightIdentity {
                            event_digest: event_id.digest(),
                            line_number,
                        }),
                        Ok(None) => {}
                        Err(error) => match scope_claude_row_validation_error(error) {
                            ClaudePreflightError::RecordRejection { .. } => return Ok(()),
                            error => return Err(error),
                        },
                    }
                    match validate_claude_row_annotation(
                        row,
                        parsed.cwd.as_deref(),
                        parsed.git_branch.as_deref(),
                    ) {
                        Ok(()) => {}
                        Err(ClaudePreflightError::RecordRejection { .. }) => return Ok(()),
                        Err(error) => return Err(error),
                    }
                }
                let count =
                    checked_preflight_identity_count(identities.len(), record_identities.len())?;
                identities
                    .try_reserve(count - identities.len())
                    .map_err(|_| {
                        ClaudePreflightError::internal(CaptureError::SystemInvariant(
                            "Claude preflight source identity allocation failed",
                        ))
                    })?;
                identities.extend(record_identities);
                Ok(())
            },
        )?
        .is_some()
    {}
    identities.sort_unstable();
    if let Some(pair) = identities
        .windows(2)
        .find(|pair| pair[0].event_digest == pair[1].event_digest)
    {
        return Err(ClaudePreflightError::logical_source_failure(
            source.clone(),
            format!(
                "Claude transcript repeats a stable event identity at lines {} and {}",
                pair[0].line_number, pair[1].line_number
            ),
        ));
    }
    Ok(false)
}

pub(super) fn parsed_record_is_rejected(parsed: &ParsedClaudeRecord, binding: &Binding) -> bool {
    parsed
        .session_id
        .as_deref()
        .filter(|session| !session.trim().is_empty())
        .is_some_and(|session| session != binding.key.root_session_id)
        || (parsed.rows.is_empty() && !parsed.ignored_private_thinking)
        || parsed.rows.len() > CLAUDE_MAX_RECORD_ROWS
}

pub(super) fn stable_native_event_identity(
    row: &ClaudeRetainedRow,
    source: &SourceKey,
    session_id: StableEntityId,
) -> std::result::Result<Option<StableEntityId>, ClaudeRowValidationError> {
    if row.native_record_id.is_none() {
        return Ok(None);
    }
    let native_record_id = typed_claude_record_key(
        ClaudeRecordKeyField::NativeRecordId,
        row.native_record_id
            .as_deref()
            .expect("native record identity was checked above"),
    )?;
    let native_item_key = NativeItemKey::composite(
        NATIVE_EVENT_KEY_NAMESPACE,
        native_event_key_parts(native_record_id, row.identity.source_subrecord_index),
    )
    .map_err(|error| ClaudeRowValidationError::Fatal(contract(error)))?;
    derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map(Some)
    .map_err(|error| ClaudeRowValidationError::Fatal(contract(error)))
}

pub(super) fn validate_claude_row_annotation(
    row: &ClaudeRetainedRow,
    declared_cwd: Option<&str>,
    declared_branch: Option<&str>,
) -> std::result::Result<(), ClaudePreflightError> {
    claude_annotation(row, declared_cwd, declared_branch)
        .map(drop)
        .map_err(scope_claude_row_validation_error)
}
