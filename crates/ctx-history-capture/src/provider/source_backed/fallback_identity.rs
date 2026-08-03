use std::collections::HashMap;

use ctx_history_core::{
    derive_event_id, EventIdentityInput, NativeItemKey, SourceKey, StableEntityId,
    SubrecordSelector, TypedKey,
};
use ctx_history_index::BaseEventIdentityLookup;

use crate::{CaptureError, Result};

use super::family::jsonl::JsonlFamilyProjectionMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FallbackEventIdentityMode {
    Cold,
    CertifiedAppend,
    Replacement,
}

impl From<JsonlFamilyProjectionMode> for FallbackEventIdentityMode {
    fn from(mode: JsonlFamilyProjectionMode) -> Self {
        match mode {
            JsonlFamilyProjectionMode::Cold => Self::Cold,
            JsonlFamilyProjectionMode::CertifiedAppend => Self::CertifiedAppend,
            JsonlFamilyProjectionMode::Replacement => Self::Replacement,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FallbackEventIdentityAssignment {
    native_item_key: NativeItemKey,
    native_event_id: TypedKey,
    #[cfg(test)]
    duplicate_occurrence: u64,
}

impl FallbackEventIdentityAssignment {
    pub(crate) fn native_item_key(&self) -> &NativeItemKey {
        &self.native_item_key
    }

    pub(crate) fn native_event_id(&self) -> &TypedKey {
        &self.native_event_id
    }

    #[cfg(test)]
    fn duplicate_occurrence(&self) -> u64 {
        self.duplicate_occurrence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FallbackGroupKey {
    fingerprint: TypedKey,
    subrecord_selector: Option<SubrecordSelector>,
}

struct FallbackGroupState {
    base_occurrences: u64,
    projected_occurrences: u64,
}

/// Assigns content-shaped fallback identities without making source position
/// part of the identity.
///
/// A certified append may safely continue an indistinguishable duplicate run,
/// because the family has proved that the complete old prefix is unchanged.
/// A replacement restarts occurrence numbering and reconciles every observed
/// duplicate group against the immutable Core base. If a group existed in the
/// current scheme and its cardinality changed, the replacement is ambiguous:
/// there is no provider evidence identifying which duplicate survived. The
/// caller must fail the source instead of adopting an arbitrary prior ID.
pub(crate) struct FallbackEventIdentityState {
    source: SourceKey,
    session_id: StableEntityId,
    logical_item_kind: String,
    native_item_namespace: String,
    identity_version: String,
    mode: FallbackEventIdentityMode,
    base_lookup: Option<BaseEventIdentityLookup>,
    groups: HashMap<FallbackGroupKey, FallbackGroupState>,
}

impl FallbackEventIdentityState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        source: SourceKey,
        session_id: StableEntityId,
        logical_item_kind: impl Into<String>,
        native_item_namespace: impl Into<String>,
        identity_version: impl Into<String>,
        mode: FallbackEventIdentityMode,
        base_lookup: Option<BaseEventIdentityLookup>,
    ) -> Result<Self> {
        match (mode, base_lookup.is_some()) {
            (FallbackEventIdentityMode::Cold, false)
            | (FallbackEventIdentityMode::CertifiedAppend, true)
            | (FallbackEventIdentityMode::Replacement, true) => {}
            _ => {
                return Err(CaptureError::SystemInvariant(
                    "fallback event identity mode has inconsistent Core base authority",
                ));
            }
        }
        Ok(Self {
            source,
            session_id,
            logical_item_kind: logical_item_kind.into(),
            native_item_namespace: native_item_namespace.into(),
            identity_version: identity_version.into(),
            mode,
            base_lookup,
            groups: HashMap::new(),
        })
    }

    pub(crate) fn assign(
        &mut self,
        fingerprint: TypedKey,
        subrecord_selector: Option<&SubrecordSelector>,
    ) -> Result<FallbackEventIdentityAssignment> {
        fingerprint
            .validate_contract()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let key = FallbackGroupKey {
            fingerprint,
            subrecord_selector: subrecord_selector.cloned(),
        };
        if !self.groups.contains_key(&key) {
            let base_occurrences = self.base_occurrence_count(&key)?;
            self.groups.insert(
                key.clone(),
                FallbackGroupState {
                    base_occurrences,
                    projected_occurrences: 0,
                },
            );
        }
        let group = self
            .groups
            .get_mut(&key)
            .ok_or(CaptureError::SystemInvariant(
                "fallback event identity group disappeared",
            ))?;
        let first_occurrence = match self.mode {
            FallbackEventIdentityMode::CertifiedAppend => group.base_occurrences,
            FallbackEventIdentityMode::Cold | FallbackEventIdentityMode::Replacement => 0,
        };
        let duplicate_occurrence = first_occurrence
            .checked_add(group.projected_occurrences)
            .ok_or(CaptureError::SystemInvariant(
                "fallback event duplicate occurrence overflowed",
            ))?;
        group.projected_occurrences =
            group
                .projected_occurrences
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "fallback event duplicate occurrence overflowed",
                ))?;
        self.assignment(&key, duplicate_occurrence)
    }

    pub(crate) fn finish(&self) -> Result<()> {
        if self.mode != FallbackEventIdentityMode::Replacement {
            return Ok(());
        }
        for group in self.groups.values() {
            if group.base_occurrences != 0 && group.base_occurrences != group.projected_occurrences
            {
                return Err(CaptureError::InvalidPayload(format!(
                    "fallback event identity is ambiguous: an indistinguishable duplicate group changed from {} to {} records",
                    group.base_occurrences, group.projected_occurrences
                )));
            }
        }
        Ok(())
    }

    fn base_occurrence_count(&self, key: &FallbackGroupKey) -> Result<u64> {
        let Some(base_lookup) = self.base_lookup.as_ref() else {
            return Ok(0);
        };
        if !self.base_occurrence_exists(base_lookup, key, 0)? {
            return Ok(0);
        }
        let mut present = 0_u64;
        let mut missing = 1_u64;
        while self.base_occurrence_exists(base_lookup, key, missing)? {
            present = missing;
            missing = match missing.checked_mul(2) {
                Some(next) => next,
                None if missing != u64::MAX => u64::MAX,
                None => {
                    return Err(CaptureError::SystemInvariant(
                        "fallback event duplicate occurrence overflowed",
                    ));
                }
            };
        }
        while present.saturating_add(1) < missing {
            let candidate = present + (missing - present) / 2;
            if self.base_occurrence_exists(base_lookup, key, candidate)? {
                present = candidate;
            } else {
                missing = candidate;
            }
        }
        Ok(missing)
    }

    fn base_occurrence_exists(
        &self,
        base_lookup: &BaseEventIdentityLookup,
        key: &FallbackGroupKey,
        occurrence: u64,
    ) -> Result<bool> {
        let assignment = self.assignment(key, occurrence)?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: &self.logical_item_kind,
            native_item_key: assignment.native_item_key(),
            subrecord_selector: key.subrecord_selector.as_ref(),
        })
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        base_lookup
            .contains(event_id.as_uuid())
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
    }

    fn assignment(
        &self,
        key: &FallbackGroupKey,
        duplicate_occurrence: u64,
    ) -> Result<FallbackEventIdentityAssignment> {
        let parts = vec![
            TypedKey::utf8(&self.identity_version)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
            key.fingerprint.clone(),
            TypedKey::U64(duplicate_occurrence),
        ];
        let native_item_key = NativeItemKey::composite(&self.native_item_namespace, parts.clone())
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let native_event_id = TypedKey::composite(parts)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        Ok(FallbackEventIdentityAssignment {
            native_item_key,
            native_event_id,
            #[cfg(test)]
            duplicate_occurrence,
        })
    }
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{
        derive_session_id, CertifiedSource, NativeSessionKey, ScannedSourceCounts,
        SessionIdentityInput, SourceAnchor, SourceObservation,
    };
    use ctx_history_index::{GenerationWriter, WriterOptions};
    use sha2::{Digest, Sha256};

    use super::*;

    const LOGICAL_ITEM_KIND: &str = "fallback-test-event";
    const NATIVE_ITEM_NAMESPACE: &str = "fallback.test.event";
    const IDENTITY_VERSION: &str = "fallback-test-v1";

    fn source_and_session() -> (SourceKey, StableEntityId) {
        let source = SourceKey::derive(
            "pi",
            "pi_session_jsonl",
            "fallback-test-v1",
            1,
            SourceAnchor::provider_native(
                "fallback.test.session",
                TypedKey::utf8("session").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let native_session_key = NativeSessionKey::native_id(
            "fallback.test.session",
            TypedKey::utf8("session").unwrap(),
        )
        .unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "fallback-test-session",
            native_session_key: &native_session_key,
        })
        .unwrap();
        (source, session_id)
    }

    fn fingerprint(value: &str) -> TypedKey {
        let mut digest = Sha256::new();
        digest.update(b"fallback-test-fingerprint-v1\0");
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
        TypedKey::bytes(digest.finalize().to_vec()).unwrap()
    }

    fn assignments(
        source: &SourceKey,
        session_id: StableEntityId,
        mode: FallbackEventIdentityMode,
        lookup: Option<BaseEventIdentityLookup>,
        values: &[&str],
    ) -> (Vec<(StableEntityId, TypedKey)>, Result<()>) {
        let mut state = FallbackEventIdentityState::new(
            source.clone(),
            session_id,
            LOGICAL_ITEM_KIND,
            NATIVE_ITEM_NAMESPACE,
            IDENTITY_VERSION,
            mode,
            lookup,
        )
        .unwrap();
        let events = values
            .iter()
            .map(|value| {
                let assignment = state.assign(fingerprint(value), None).unwrap();
                let event_id = derive_event_id(EventIdentityInput {
                    source,
                    session_id,
                    logical_item_kind: LOGICAL_ITEM_KIND,
                    native_item_key: assignment.native_item_key(),
                    subrecord_selector: None,
                })
                .unwrap();
                (event_id, assignment.native_event_id().clone())
            })
            .collect();
        let finished = state.finish();
        (events, finished)
    }

    fn base_lookup_with_events(
        source: &SourceKey,
        session_id: StableEntityId,
        events: &[(StableEntityId, TypedKey)],
    ) -> (tempfile::TempDir, BaseEventIdentityLookup) {
        let temp = tempfile::tempdir().unwrap();
        let options = WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        };
        let mut writer = GenerationWriter::open(temp.path(), options.clone()).unwrap();
        writer.begin_source(source.clone()).unwrap();
        for (index, (event_id, native_event_id)) in events.iter().enumerate() {
            let sequence = u64::try_from(index).unwrap();
            let mut record = ctx_history_core::CoreRecord::new_selected(
                *event_id,
                session_id,
                session_id,
                source.clone(),
                sequence,
                "message",
                "primary",
                true,
                "fallback-test-parser-v1",
                "fallback identity test",
            )
            .unwrap();
            record.provider_session_id = Some("session".to_owned());
            record.native_event_id = Some(native_event_id.clone());
            writer.add_core_record(record).unwrap();
        }
        let observation =
            SourceObservation::new(source.clone(), "fallback-test-source-v1", vec![1]).unwrap();
        let count = u64::try_from(events.len()).unwrap();
        writer
            .certify_source(
                CertifiedSource::certify(
                    observation.clone(),
                    observation,
                    "fallback-test-parser-v1",
                    [1; 32],
                    ScannedSourceCounts {
                        complete_records: count,
                        retained_records: count,
                        indexed_documents: count,
                        certified_bytes: count,
                        ..ScannedSourceCounts::default()
                    },
                )
                .unwrap(),
            )
            .unwrap();
        writer.commit(|_| true).unwrap();
        let writer = GenerationWriter::open(temp.path(), options).unwrap();
        let lookup = writer.base_event_identity_lookup();
        drop(writer);
        (temp, lookup)
    }

    #[test]
    fn fallback_assignment_mutation_matrix_preserves_only_proven_identity() {
        let (source, session_id) = source_and_session();
        let baseline_values = ["anchor", "target", "suffix"];
        let (baseline, finished) = assignments(
            &source,
            session_id,
            FallbackEventIdentityMode::Cold,
            None,
            &baseline_values,
        );
        finished.unwrap();
        let (_base, lookup) = base_lookup_with_events(&source, session_id, &baseline);

        let cases = [
            (
                vec!["inserted", "anchor", "target", "suffix"],
                vec![(0, 1), (1, 2), (2, 3)],
            ),
            (vec!["target", "suffix"], vec![(1, 0), (2, 1)]),
            (vec!["anchor", "rewritten", "suffix"], vec![(0, 0), (2, 2)]),
            (vec!["anchor", "target"], vec![(0, 0), (1, 1)]),
        ];
        for (values, preserved) in cases {
            let (current, finished) = assignments(
                &source,
                session_id,
                FallbackEventIdentityMode::Replacement,
                Some(lookup.clone()),
                &values,
            );
            finished.unwrap();
            for (old, new) in preserved {
                assert_eq!(baseline[old].0, current[new].0);
            }
        }
    }

    #[test]
    fn duplicate_groups_are_stable_when_unchanged_and_fail_when_ambiguous() {
        let (source, session_id) = source_and_session();
        let (baseline, finished) = assignments(
            &source,
            session_id,
            FallbackEventIdentityMode::Cold,
            None,
            &["anchor", "duplicate", "duplicate", "suffix"],
        );
        finished.unwrap();
        let (_base, lookup) = base_lookup_with_events(&source, session_id, &baseline);

        let (inserted, finished) = assignments(
            &source,
            session_id,
            FallbackEventIdentityMode::Replacement,
            Some(lookup.clone()),
            &["inserted", "anchor", "duplicate", "duplicate", "suffix"],
        );
        finished.unwrap();
        assert_eq!(baseline[1].0, inserted[2].0);
        assert_eq!(baseline[2].0, inserted[3].0);

        let (_, ambiguous_delete) = assignments(
            &source,
            session_id,
            FallbackEventIdentityMode::Replacement,
            Some(lookup.clone()),
            &["anchor", "duplicate", "suffix"],
        );
        assert!(ambiguous_delete
            .unwrap_err()
            .to_string()
            .contains("indistinguishable duplicate group changed from 2 to 1"));

        let (_, ambiguous_insert) = assignments(
            &source,
            session_id,
            FallbackEventIdentityMode::Replacement,
            Some(lookup.clone()),
            &["anchor", "duplicate", "duplicate", "duplicate", "suffix"],
        );
        assert!(ambiguous_insert.is_err());

        let mut append = FallbackEventIdentityState::new(
            source.clone(),
            session_id,
            LOGICAL_ITEM_KIND,
            NATIVE_ITEM_NAMESPACE,
            IDENTITY_VERSION,
            FallbackEventIdentityMode::CertifiedAppend,
            Some(lookup),
        )
        .unwrap();
        let appended = append.assign(fingerprint("duplicate"), None).unwrap();
        assert_eq!(appended.duplicate_occurrence(), 2);
        append.finish().unwrap();
    }
}
