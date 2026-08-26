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
    claude_annotation, contract, native_record_key_parts, Binding, JsonlReader, LOGICAL_EVENT_KIND,
    NATIVE_EVENT_KEY_NAMESPACE,
};
use crate::claude::nativepath::{
    record::{parse_native_record, ParsedClaudeRecord},
    rows::{
        ClaudeEventIdentity, ClaudeEventKind, ClaudePhysicalLocator, ClaudeRetainedRow,
        CLAUDE_MAX_RECORD_ROWS,
    },
};

pub(super) const MAX_PREFLIGHT_EVENT_IDENTITIES: usize = 1_048_576;

pub(super) type ClaudePreflightError = JsonlFamilyProjectorPreflightError<CaptureError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ClaudePreflightIdentity {
    event_digest: [u8; 32],
    source_record_ordinal: u64,
    source_subrecord_index: u16,
    kind: ClaudeEventKind,
    in_certified_prefix: bool,
}

impl ClaudePreflightIdentity {
    fn line_number(self) -> u64 {
        self.source_record_ordinal + 1
    }

    fn position(self) -> ClaudeEventIdentity {
        ClaudeEventIdentity {
            source_record_ordinal: self.source_record_ordinal,
            source_subrecord_index: u64::from(self.source_subrecord_index),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ClaudeDuplicateWinner {
    event_digest: [u8; 32],
    position: ClaudeEventIdentity,
}

#[derive(Debug, Default)]
pub(super) struct ClaudeDuplicatePlan {
    winners: Vec<ClaudeDuplicateWinner>,
}

impl ClaudeDuplicatePlan {
    pub(super) fn retains(
        &self,
        row: &ClaudeRetainedRow,
        source: &SourceKey,
        session_id: StableEntityId,
    ) -> std::result::Result<bool, ClaudeRowValidationError> {
        let Some(event_id) = stable_native_event_identity(row, source, session_id)? else {
            return Ok(true);
        };
        Ok(self
            .winners
            .binary_search_by_key(&event_id.digest(), |winner| winner.event_digest)
            .map_or(true, |index| self.winners[index].position == row.identity))
    }

    fn clear(&mut self) {
        self.winners.clear();
    }

    fn insert(
        &mut self,
        event_digest: [u8; 32],
        winner: ClaudeEventIdentity,
    ) -> std::result::Result<(), ClaudePreflightError> {
        self.winners.try_reserve(1).map_err(|_| {
            ClaudePreflightError::internal(CaptureError::SystemInvariant(
                "Claude duplicate winner allocation failed",
            ))
        })?;
        self.winners.push(ClaudeDuplicateWinner {
            event_digest,
            position: winner,
        });
        Ok(())
    }
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
    certified_prefix_end: Option<u64>,
    duplicate_plan: &mut ClaudeDuplicatePlan,
) -> std::result::Result<bool, ClaudePreflightError> {
    duplicate_plan.clear();
    let mut identities = Vec::new();
    let mut requires_replacement = false;
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
                            source_record_ordinal: row.identity.source_record_ordinal,
                            source_subrecord_index: u16::try_from(
                                row.identity.source_subrecord_index,
                            )
                            .map_err(|_| {
                                ClaudePreflightError::internal(CaptureError::SystemInvariant(
                                    "Claude preflight subrecord identity exceeded its parser bound",
                                ))
                            })?,
                            kind: row.kind,
                            in_certified_prefix: certified_prefix_end
                                .is_some_and(|end| evidence.byte_end_exclusive() <= end),
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
    identities.sort_unstable_by(|left, right| {
        left.event_digest
            .cmp(&right.event_digest)
            .then_with(|| left.source_record_ordinal.cmp(&right.source_record_ordinal))
            .then_with(|| {
                left.source_subrecord_index
                    .cmp(&right.source_subrecord_index)
            })
    });
    let mut group_start = 0;
    while group_start < identities.len() {
        let event_digest = identities[group_start].event_digest;
        let mut group_end = group_start + 1;
        while group_end < identities.len() && identities[group_end].event_digest == event_digest {
            group_end += 1;
        }
        if group_end - group_start > 1 {
            let first = identities[group_start];
            if let Some(incompatible) = identities[group_start + 1..group_end]
                .iter()
                .copied()
                .find(|candidate| candidate.kind != first.kind)
            {
                return Err(ClaudePreflightError::logical_source_failure(
                    source.clone(),
                    format!(
                        "Claude transcript repeats a stable event identity with incompatible event kinds at lines {} and {}",
                        first.line_number(),
                        incompatible.line_number()
                    ),
                ));
            }
            let winner = identities[group_end - 1];
            requires_replacement |=
                identities[group_start].in_certified_prefix && !winner.in_certified_prefix;
            duplicate_plan.insert(event_digest, winner.position())?;
        }
        group_start = group_end;
    }
    Ok(requires_replacement)
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
    let Some(key_parts) = native_record_key_parts(row)? else {
        return Ok(None);
    };
    let native_item_key = NativeItemKey::composite(NATIVE_EVENT_KEY_NAMESPACE, key_parts)
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
