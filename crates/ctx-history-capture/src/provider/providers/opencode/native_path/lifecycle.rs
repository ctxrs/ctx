use std::path::PathBuf;

use crate::{CaptureError, Result};

use super::model::{
    OpenCodeNativePersistedState, OpenCodeNativeProFrontier, OpenCodeNativeProfile,
    OpenCodeNativeScanSummary, OpenCodeNativeSequencePrefixEvidence,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OpenCodeNativeGenerationChange {
    New,
    ExactReplay,
    AppendOnly,
    Rewrite,
    Rewind,
    RewriteAndRewind,
    Replacement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OpenCodeNativePublicationMode {
    ObservationOnly,
    ResumeProReplay,
    AuthoritativeGeneration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativeLifecyclePlan {
    pub(super) change: OpenCodeNativeGenerationChange,
    pub(super) publication: OpenCodeNativePublicationMode,
    pub(super) prior_path: Option<PathBuf>,
    pub(super) pro_replay_frontier: Option<OpenCodeNativeProFrontier>,
}

#[derive(Clone)]
pub(super) struct OpenCodeNativePriorGeneration {
    pub(super) state: OpenCodeNativePersistedState,
}

impl OpenCodeNativePriorGeneration {
    pub(super) fn from_persisted(state: OpenCodeNativePersistedState) -> Self {
        Self { state }
    }
}

pub(super) fn classify_opencode_native_lifecycle(
    previous: &[OpenCodeNativePriorGeneration],
    current: &OpenCodeNativeScanSummary,
) -> Result<OpenCodeNativeLifecyclePlan> {
    let current_state = current.persisted_state();
    if !current_state.is_supported() {
        return Err(CaptureError::InvalidPayload(
            "OpenCode current lifecycle state is incomplete or unsupported".to_owned(),
        ));
    }
    let same_path = previous
        .iter()
        .filter(|prior| prior.state.selected_path == current_state.selected_path)
        .collect::<Vec<_>>();
    match same_path.as_slice() {
        [] => Ok(authoritative_plan(
            OpenCodeNativeGenerationChange::New,
            None,
        )),
        [prior] => classify_same_path(prior, current),
        [_, _, ..] => Ok(authoritative_plan(
            OpenCodeNativeGenerationChange::Rewrite,
            None,
        )),
    }
}

fn classify_same_path(
    prior: &OpenCodeNativePriorGeneration,
    current: &OpenCodeNativeScanSummary,
) -> Result<OpenCodeNativeLifecyclePlan> {
    let previous = &prior.state;
    let current_state = current.persisted_state();
    let prior_path = Some(previous.selected_path.clone());
    if !previous.is_supported() {
        return Ok(authoritative_plan(
            OpenCodeNativeGenerationChange::Rewrite,
            prior_path,
        ));
    }
    if previous.physical_source_identity != current_state.physical_source_identity {
        return Ok(authoritative_plan(
            OpenCodeNativeGenerationChange::Replacement,
            prior_path,
        ));
    }
    if previous.parser_revision != current_state.parser_revision
        || previous.policy_revision != current_state.policy_revision
        || previous.capability_digest != current_state.capability_digest
        || previous.schema_family != current_state.schema_family
        || previous.identity_semantics != current_state.identity_semantics
        || previous.ordering_semantics != current_state.ordering_semantics
    {
        return Ok(authoritative_plan(
            OpenCodeNativeGenerationChange::Rewrite,
            prior_path,
        ));
    }
    if exact_generation_match(previous, &current_state) {
        return Ok(exact_replay_plan(previous, current.profile));
    }
    let change = classify_restarted_prefix_change(previous, current)
        .unwrap_or(OpenCodeNativeGenerationChange::Rewrite);
    Ok(if change == OpenCodeNativeGenerationChange::ExactReplay {
        exact_replay_plan(previous, current.profile)
    } else {
        authoritative_plan(change, prior_path)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OrderedSequenceChange {
    Exact,
    Append,
    Rewind,
    Rewrite,
}

fn classify_restarted_prefix_change(
    previous: &OpenCodeNativePersistedState,
    current: &OpenCodeNativeScanSummary,
) -> Option<OpenCodeNativeGenerationChange> {
    let comparison = current.restart_prefix_comparison.as_ref()?;
    if comparison.prior_evidence_fingerprint != previous.ordered_prefix_evidence.fingerprint() {
        return None;
    }
    let current_evidence = &current.ordered_prefix_evidence;
    let mut changes = vec![
        sequence_change(
            &previous.ordered_prefix_evidence.sessions,
            &current_evidence.sessions,
            comparison.sessions_prefix_matches,
        ),
        sequence_change(
            &previous.ordered_prefix_evidence.core_events,
            &current_evidence.core_events,
            comparison.core_events_prefix_matches,
        ),
    ];
    if previous.profile == OpenCodeNativeProfile::CoreAndPro
        && current.profile == OpenCodeNativeProfile::CoreAndPro
    {
        changes.push(sequence_change(
            &previous.ordered_prefix_evidence.pro_units,
            &current_evidence.pro_units,
            comparison.pro_units_prefix_matches,
        ));
    }
    let has_append = changes.contains(&OrderedSequenceChange::Append);
    let has_rewind = changes.contains(&OrderedSequenceChange::Rewind);
    let has_rewrite = changes.contains(&OrderedSequenceChange::Rewrite);
    Some(match (has_append, has_rewrite, has_rewind) {
        (false, false, false) => OpenCodeNativeGenerationChange::ExactReplay,
        (true, false, false) => OpenCodeNativeGenerationChange::AppendOnly,
        (_, true, false) => OpenCodeNativeGenerationChange::Rewrite,
        (_, false, true) => OpenCodeNativeGenerationChange::Rewind,
        (_, true, true) => OpenCodeNativeGenerationChange::RewriteAndRewind,
    })
}

fn sequence_change(
    previous: &OpenCodeNativeSequencePrefixEvidence,
    current: &OpenCodeNativeSequencePrefixEvidence,
    previous_prefix_matches: bool,
) -> OrderedSequenceChange {
    if previous == current {
        OrderedSequenceChange::Exact
    } else if current.count < previous.count {
        OrderedSequenceChange::Rewind
    } else if current.count > previous.count && previous_prefix_matches {
        OrderedSequenceChange::Append
    } else {
        OrderedSequenceChange::Rewrite
    }
}

fn exact_generation_match(
    previous: &OpenCodeNativePersistedState,
    current: &OpenCodeNativePersistedState,
) -> bool {
    previous.is_supported()
        && current.is_supported()
        && previous.source_generation_digest == current.source_generation_digest
        && previous.capability_digest == current.capability_digest
        && previous.semantic_digest == current.semantic_digest
        && previous.completed_inventory == current.completed_inventory
}

fn exact_replay_plan(
    previous: &OpenCodeNativePersistedState,
    current_profile: OpenCodeNativeProfile,
) -> OpenCodeNativeLifecyclePlan {
    let resume_pro =
        current_profile == OpenCodeNativeProfile::CoreAndPro && !previous.pro_frontier.terminal;
    OpenCodeNativeLifecyclePlan {
        change: OpenCodeNativeGenerationChange::ExactReplay,
        publication: if resume_pro {
            OpenCodeNativePublicationMode::ResumeProReplay
        } else {
            OpenCodeNativePublicationMode::ObservationOnly
        },
        prior_path: Some(previous.selected_path.clone()),
        pro_replay_frontier: resume_pro.then_some(previous.pro_frontier),
    }
}

fn authoritative_plan(
    change: OpenCodeNativeGenerationChange,
    prior_path: Option<PathBuf>,
) -> OpenCodeNativeLifecyclePlan {
    OpenCodeNativeLifecyclePlan {
        change,
        publication: OpenCodeNativePublicationMode::AuthoritativeGeneration,
        prior_path,
        pro_replay_frontier: None,
    }
}
