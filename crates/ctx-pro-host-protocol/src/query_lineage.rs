use std::{cmp::Ordering, collections::BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    query::{same_resource_identity, validate_evidence_numbers},
    ErrorClass, ProtocolError, ResourceKind, ResourceRef, MAX_BLAME_TARGET_BYTES,
    MAX_COMMIT_LINEAGE_EXAMINED_EVENTS, MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitObjectFormat {
    Sha1,
    Sha256,
}

impl GitObjectFormat {
    const fn oid_bytes(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }
}

/// One exact Git commit object in its logical repository object domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactCommitRef {
    pub resource: ResourceRef,
    pub object_format: GitObjectFormat,
    pub oid: String,
}

impl ExactCommitRef {
    fn validate(&self) -> Result<(), ProtocolError> {
        self.resource.validate()?;
        if self.resource.kind != ResourceKind::Commit {
            return Err(corrupt("exact commit reference is not a commit resource"));
        }
        if self.oid.len() != self.object_format.oid_bytes()
            || !self
                .oid
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(corrupt(
                "exact commit object ID does not match its declared Git object format",
            ));
        }
        if self.resource.display != self.oid {
            return Err(corrupt(
                "exact commit resource display must preserve the full object ID",
            ));
        }
        Ok(())
    }

    fn same_identity(&self, other: &Self) -> bool {
        self.object_format == other.object_format
            && self.oid == other.oid
            && same_resource_identity(&self.resource, &other.resource)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitLineageOperationKind {
    Amend,
    Rebase,
    CherryPick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitLineageRelationClass {
    Replacement,
    Derivation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitLineageProofClass {
    RecordExact,
    RepositoryVerified,
    ForgeVerified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitLineageState {
    Asserted,
    Ambiguous,
    Contradicted,
}

/// One exact causal operation mapping. Timestamps describe evidence observation,
/// never Git chronology, authorship, endpoint selection, or ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitLineageEdge {
    pub operation_id: String,
    pub kind: CommitLineageOperationKind,
    pub relation_class: CommitLineageRelationClass,
    pub source: ExactCommitRef,
    pub result: ExactCommitRef,
    pub actor: ResourceRef,
    pub proof_class: CommitLineageProofClass,
    pub state: CommitLineageState,
    pub observed_at_ms: Option<i64>,
    pub evidence_numbers: Vec<u32>,
}

impl CommitLineageEdge {
    fn validate(
        &self,
        available: &BTreeSet<u32>,
        referenced: &mut BTreeSet<u32>,
    ) -> Result<(), ProtocolError> {
        validate_text(&self.operation_id, "commit lineage operation ID")?;
        self.source.validate()?;
        self.result.validate()?;
        if self.source.same_identity(&self.result) {
            return Err(corrupt(
                "commit lineage operation source and result must be distinct exact objects",
            ));
        }
        let expected_relation = match self.kind {
            CommitLineageOperationKind::Amend | CommitLineageOperationKind::Rebase => {
                CommitLineageRelationClass::Replacement
            }
            CommitLineageOperationKind::CherryPick => CommitLineageRelationClass::Derivation,
        };
        if self.relation_class != expected_relation {
            return Err(corrupt(
                "commit lineage operation kind and relation class disagree",
            ));
        }
        validate_session(&self.actor, "commit lineage operation actor")?;
        validate_observed_at(self.observed_at_ms)?;
        validate_evidence_numbers(&self.evidence_numbers, available, referenced)
    }

    fn stable_cmp(&self, other: &Self) -> Ordering {
        (
            self.kind,
            self.source.object_format,
            self.source.oid.as_str(),
            self.result.object_format,
            self.result.oid.as_str(),
            self.operation_id.as_str(),
        )
            .cmp(&(
                other.kind,
                other.source.object_format,
                other.source.oid.as_str(),
                other.result.object_format,
                other.result.oid.as_str(),
                other.operation_id.as_str(),
            ))
    }
}

/// A weaker event-scoped yield attribution for the exact requested object.
/// It is present only when no incoming operation edge explains that object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitLineageYield {
    pub yield_id: String,
    pub actor: ResourceRef,
    pub proof_class: CommitLineageProofClass,
    pub state: CommitLineageState,
    pub observed_at_ms: Option<i64>,
    pub evidence_numbers: Vec<u32>,
}

impl CommitLineageYield {
    fn validate(
        &self,
        available: &BTreeSet<u32>,
        referenced: &mut BTreeSet<u32>,
    ) -> Result<(), ProtocolError> {
        validate_text(&self.yield_id, "commit lineage yield ID")?;
        validate_session(&self.actor, "commit lineage yield actor")?;
        validate_observed_at(self.observed_at_ms)?;
        validate_evidence_numbers(&self.evidence_numbers, available, referenced)
    }

    fn stable_cmp(&self, other: &Self) -> Ordering {
        (self.actor.id.as_str(), self.yield_id.as_str())
            .cmp(&(other.actor.id.as_str(), other.yield_id.as_str()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "count",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CommitLineageOmission {
    Exact(u32),
    AtLeast(u32),
    Unknown,
}

impl CommitLineageOmission {
    const fn is_exact_zero(&self) -> bool {
        matches!(self, Self::Exact(0))
    }

    fn validate(&self) -> Result<(), ProtocolError> {
        if matches!(self, Self::AtLeast(0)) {
            return Err(corrupt(
                "commit lineage at-least omission count must be positive",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitLineageTruncationReason {
    ReturnedEventLimit,
    ExaminedEventLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitLineageBounds {
    pub returned_events: u32,
    pub returned_event_limit: u32,
    pub examined_events: u32,
    pub examined_event_limit: u32,
    pub omission: CommitLineageOmission,
    pub truncation_reason: Option<CommitLineageTruncationReason>,
}

impl CommitLineageBounds {
    fn validate(&self, actual_returned_events: usize, complete: bool) -> Result<(), ProtocolError> {
        if self.returned_event_limit != MAX_COMMIT_LINEAGE_RETURNED_EVENTS
            || self.examined_event_limit != MAX_COMMIT_LINEAGE_EXAMINED_EVENTS
        {
            return Err(corrupt(
                "commit lineage result does not publish the deterministic protocol limits",
            ));
        }
        let actual_returned_events = u32::try_from(actual_returned_events)
            .map_err(|_| bounds("commit lineage returned-event count exceeds u32"))?;
        if self.returned_events != actual_returned_events
            || self.returned_events > self.returned_event_limit
            || self.examined_events < self.returned_events
            || self.examined_events > self.examined_event_limit
        {
            return Err(bounds(
                "commit lineage returned or examined event count is inconsistent with its bounds",
            ));
        }
        self.omission.validate()?;
        if complete {
            if !self.omission.is_exact_zero() || self.truncation_reason.is_some() {
                return Err(corrupt(
                    "complete commit lineage cannot omit or truncate proven events",
                ));
            }
        } else if self.omission.is_exact_zero() || self.truncation_reason.is_none() {
            return Err(corrupt(
                "incomplete commit lineage must report an omission and truncation reason",
            ));
        }
        match self.truncation_reason {
            Some(CommitLineageTruncationReason::ReturnedEventLimit)
                if self.returned_events != self.returned_event_limit =>
            {
                return Err(corrupt(
                    "returned-event truncation requires the returned-event limit to be reached",
                ));
            }
            Some(CommitLineageTruncationReason::ExaminedEventLimit)
                if self.examined_events != self.examined_event_limit =>
            {
                return Err(corrupt(
                    "examined-event truncation requires the examined-event limit to be reached",
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScopedCommitEndpoint {
    CurrentAtRef {
        commit: ExactCommitRef,
        scope: ResourceRef,
        observation_id: String,
        observed_at_ms: i64,
        evidence_numbers: Vec<u32>,
    },
    CurrentForPr {
        commit: ExactCommitRef,
        scope: ResourceRef,
        observation_id: String,
        observed_at_ms: i64,
        evidence_numbers: Vec<u32>,
    },
}

impl ScopedCommitEndpoint {
    fn validate(
        &self,
        available: &BTreeSet<u32>,
        referenced: &mut BTreeSet<u32>,
    ) -> Result<(), ProtocolError> {
        let (commit, scope, expected_scope, observation_id, observed_at_ms, evidence_numbers) =
            match self {
                Self::CurrentAtRef {
                    commit,
                    scope,
                    observation_id,
                    observed_at_ms,
                    evidence_numbers,
                } => (
                    commit,
                    scope,
                    ResourceKind::Branch,
                    observation_id,
                    observed_at_ms,
                    evidence_numbers,
                ),
                Self::CurrentForPr {
                    commit,
                    scope,
                    observation_id,
                    observed_at_ms,
                    evidence_numbers,
                } => (
                    commit,
                    scope,
                    ResourceKind::PullRequest,
                    observation_id,
                    observed_at_ms,
                    evidence_numbers,
                ),
            };
        commit.validate()?;
        scope.validate()?;
        if scope.kind != expected_scope {
            return Err(corrupt(
                "scoped commit endpoint has an unexpected scope resource kind",
            ));
        }
        validate_text(observation_id, "commit endpoint observation ID")?;
        validate_observed_at(Some(*observed_at_ms))?;
        validate_evidence_numbers(evidence_numbers, available, referenced)
    }

    fn commit(&self) -> &ExactCommitRef {
        match self {
            Self::CurrentAtRef { commit, .. } | Self::CurrentForPr { commit, .. } => commit,
        }
    }
}

/// Bounded, deterministic, exact-object lineage for commit blame only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitLineage {
    pub requested: ExactCommitRef,
    pub edges: Vec<CommitLineageEdge>,
    pub yielded_by: Vec<CommitLineageYield>,
    pub origin: Option<ExactCommitRef>,
    pub endpoint: Option<ScopedCommitEndpoint>,
    pub complete: bool,
    pub ambiguous: bool,
    pub bounds: CommitLineageBounds,
}

impl CommitLineage {
    pub(crate) fn validate(
        &self,
        target: &ResourceRef,
        available: &BTreeSet<u32>,
        referenced: &mut BTreeSet<u32>,
    ) -> Result<(), ProtocolError> {
        self.requested.validate()?;
        if !same_resource_identity(&self.requested.resource, target) {
            return Err(corrupt(
                "commit lineage requested object does not preserve the resolved blame target",
            ));
        }

        for edge in &self.edges {
            edge.validate(available, referenced)?;
        }
        if self
            .edges
            .windows(2)
            .any(|pair| pair[0].stable_cmp(&pair[1]) != Ordering::Less)
        {
            return Err(corrupt(
                "commit lineage edges are not in stable deterministic order",
            ));
        }

        for yielded_by in &self.yielded_by {
            yielded_by.validate(available, referenced)?;
        }
        if self
            .yielded_by
            .windows(2)
            .any(|pair| pair[0].stable_cmp(&pair[1]) != Ordering::Less)
        {
            return Err(corrupt(
                "commit lineage yielded-by events are not in stable deterministic order",
            ));
        }

        if !self.yielded_by.is_empty()
            && self
                .edges
                .iter()
                .any(|edge| edge.result.same_identity(&self.requested))
        {
            return Err(corrupt(
                "standalone yielded-by attribution duplicates an incoming operation edge",
            ));
        }

        let actual_returned_events = self
            .edges
            .len()
            .checked_add(self.yielded_by.len())
            .ok_or_else(|| bounds("commit lineage returned-event count overflowed"))?;
        self.bounds
            .validate(actual_returned_events, self.complete)?;

        let has_non_asserted = self
            .edges
            .iter()
            .any(|edge| edge.state != CommitLineageState::Asserted)
            || self
                .yielded_by
                .iter()
                .any(|yielded_by| yielded_by.state != CommitLineageState::Asserted);
        if has_non_asserted && !self.ambiguous {
            return Err(corrupt(
                "commit lineage with ambiguous or contradicted events must report ambiguity",
            ));
        }

        if (!self.complete || self.ambiguous || !self.bounds.omission.is_exact_zero())
            && (self.origin.is_some() || self.endpoint.is_some())
        {
            return Err(corrupt(
                "partial or ambiguous commit lineage cannot claim a unique origin or endpoint",
            ));
        }
        if let Some(origin) = &self.origin {
            origin.validate()?;
            if !self.contains_commit(origin) {
                return Err(corrupt(
                    "commit lineage origin is not present in the retained lineage",
                ));
            }
        }
        if let Some(endpoint) = &self.endpoint {
            endpoint.validate(available, referenced)?;
            if !self.contains_commit(endpoint.commit()) {
                return Err(corrupt(
                    "scoped commit endpoint is not present in the retained lineage",
                ));
            }
        }
        Ok(())
    }

    fn contains_commit(&self, commit: &ExactCommitRef) -> bool {
        self.requested.same_identity(commit)
            || self
                .edges
                .iter()
                .any(|edge| edge.source.same_identity(commit) || edge.result.same_identity(commit))
    }
}

fn validate_session(resource: &ResourceRef, name: &str) -> Result<(), ProtocolError> {
    resource.validate()?;
    if resource.kind != ResourceKind::Session {
        return Err(corrupt(format!("{name} is not a session resource")));
    }
    Ok(())
}

fn validate_text(value: &str, name: &str) -> Result<(), ProtocolError> {
    if value.trim().is_empty()
        || value.len() > MAX_BLAME_TARGET_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(bounds(format!(
            "{name} is empty, unsafe, or exceeds its byte bound"
        )));
    }
    Ok(())
}

fn validate_observed_at(value: Option<i64>) -> Result<(), ProtocolError> {
    if value.is_some_and(|value| value < 0) {
        return Err(corrupt(
            "commit lineage evidence observation timestamp cannot be negative",
        ));
    }
    Ok(())
}

fn bounds(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorClass::Bounds, message)
}

fn corrupt(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorClass::Corrupt, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str, digit: char) -> ExactCommitRef {
        let oid = digit.to_string().repeat(40);
        ExactCommitRef {
            resource: ResourceRef {
                id: format!("commit:{id}"),
                kind: ResourceKind::Commit,
                display: oid.clone(),
            },
            object_format: GitObjectFormat::Sha1,
            oid,
        }
    }

    fn session(id: &str) -> ResourceRef {
        ResourceRef {
            id: format!("session:{id}"),
            kind: ResourceKind::Session,
            display: id.to_owned(),
        }
    }

    fn edge(
        operation_id: &str,
        kind: CommitLineageOperationKind,
        source: ExactCommitRef,
        result: ExactCommitRef,
    ) -> CommitLineageEdge {
        CommitLineageEdge {
            operation_id: operation_id.to_owned(),
            kind,
            relation_class: if kind == CommitLineageOperationKind::CherryPick {
                CommitLineageRelationClass::Derivation
            } else {
                CommitLineageRelationClass::Replacement
            },
            source,
            result,
            actor: session("operator"),
            proof_class: CommitLineageProofClass::RepositoryVerified,
            state: CommitLineageState::Asserted,
            observed_at_ms: Some(1_700_000_000_000),
            evidence_numbers: vec![1],
        }
    }

    fn complete_lineage() -> CommitLineage {
        let source = commit("source", '1');
        let requested = commit("requested", '2');
        CommitLineage {
            requested: requested.clone(),
            edges: vec![edge(
                "operation:rebase",
                CommitLineageOperationKind::Rebase,
                source.clone(),
                requested.clone(),
            )],
            yielded_by: Vec::new(),
            origin: Some(source),
            endpoint: Some(ScopedCommitEndpoint::CurrentAtRef {
                commit: requested,
                scope: ResourceRef {
                    id: "branch:main".to_owned(),
                    kind: ResourceKind::Branch,
                    display: "main".to_owned(),
                },
                observation_id: "observation:main".to_owned(),
                observed_at_ms: 1_700_000_000_000,
                evidence_numbers: vec![1],
            }),
            complete: true,
            ambiguous: false,
            bounds: CommitLineageBounds {
                returned_events: 1,
                returned_event_limit: MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
                examined_events: 1,
                examined_event_limit: MAX_COMMIT_LINEAGE_EXAMINED_EVENTS,
                omission: CommitLineageOmission::Exact(0),
                truncation_reason: None,
            },
        }
    }

    fn validate(lineage: &CommitLineage) -> Result<BTreeSet<u32>, ProtocolError> {
        let available = BTreeSet::from([1]);
        let mut referenced = BTreeSet::new();
        lineage.validate(&lineage.requested.resource, &available, &mut referenced)?;
        Ok(referenced)
    }

    #[test]
    fn complete_exact_lineage_preserves_requested_object_and_all_references() {
        let lineage = complete_lineage();
        assert_eq!(validate(&lineage).unwrap(), BTreeSet::from([1]));
        let encoded = serde_json::to_value(&lineage).unwrap();
        assert_eq!(encoded["requested"]["oid"], "2".repeat(40));
        assert_eq!(encoded["edges"][0]["kind"], "rebase");
        assert_eq!(encoded["endpoint"]["kind"], "current_at_ref");
        assert!(encoded.get("current").is_none());
    }

    #[test]
    fn amend_and_cherry_pick_have_closed_distinct_relation_classes() {
        let source = commit("source", '1');
        let result = commit("result", '2');
        for (kind, expected) in [
            (
                CommitLineageOperationKind::Amend,
                CommitLineageRelationClass::Replacement,
            ),
            (
                CommitLineageOperationKind::Rebase,
                CommitLineageRelationClass::Replacement,
            ),
            (
                CommitLineageOperationKind::CherryPick,
                CommitLineageRelationClass::Derivation,
            ),
        ] {
            let mut value = edge("operation:test", kind, source.clone(), result.clone());
            value.relation_class = expected;
            let available = BTreeSet::from([1]);
            value.validate(&available, &mut BTreeSet::new()).unwrap();
            value.relation_class = if expected == CommitLineageRelationClass::Replacement {
                CommitLineageRelationClass::Derivation
            } else {
                CommitLineageRelationClass::Replacement
            };
            assert!(value.validate(&available, &mut BTreeSet::new()).is_err());
        }
    }

    #[test]
    fn exact_commit_rejects_abbreviated_mismatched_and_uppercase_oids() {
        let mut value = commit("commit", 'a');
        value.oid.pop();
        assert!(value.validate().is_err());

        let mut value = commit("commit", 'a');
        value.oid = "A".repeat(40);
        value.resource.display = value.oid.clone();
        assert!(value.validate().is_err());

        let mut value = commit("commit", 'a');
        value.object_format = GitObjectFormat::Sha256;
        assert!(value.validate().is_err());
    }

    #[test]
    fn partial_or_ambiguous_lineage_suppresses_origin_and_endpoint() {
        let mut partial = complete_lineage();
        partial.complete = false;
        partial.bounds.returned_events = MAX_COMMIT_LINEAGE_RETURNED_EVENTS;
        partial.bounds.examined_events = MAX_COMMIT_LINEAGE_RETURNED_EVENTS;
        partial.bounds.omission = CommitLineageOmission::AtLeast(1);
        partial.bounds.truncation_reason = Some(CommitLineageTruncationReason::ReturnedEventLimit);
        assert!(validate(&partial).is_err());
        partial.origin = None;
        partial.endpoint = None;
        assert!(
            validate(&partial).is_err(),
            "actual retained count still disagrees"
        );

        partial.bounds.returned_events = 1;
        partial.bounds.returned_event_limit = 1;
        assert!(
            validate(&partial).is_err(),
            "published limit is not deterministic"
        );

        let mut ambiguous = complete_lineage();
        ambiguous.ambiguous = true;
        assert!(validate(&ambiguous).is_err());
        ambiguous.origin = None;
        ambiguous.endpoint = None;
        validate(&ambiguous).unwrap();
    }

    #[test]
    fn incomplete_bounds_require_nonzero_or_unknown_omission_and_reached_limit() {
        let mut lineage = complete_lineage();
        lineage.origin = None;
        lineage.endpoint = None;
        lineage.complete = false;
        lineage.bounds.omission = CommitLineageOmission::Unknown;
        lineage.bounds.truncation_reason = Some(CommitLineageTruncationReason::ExaminedEventLimit);
        assert!(validate(&lineage).is_err());
        lineage.bounds.examined_events = MAX_COMMIT_LINEAGE_EXAMINED_EVENTS;
        validate(&lineage).unwrap();

        lineage.bounds.omission = CommitLineageOmission::AtLeast(0);
        assert!(validate(&lineage).is_err());
        lineage.bounds.omission = CommitLineageOmission::Exact(0);
        assert!(validate(&lineage).is_err());
    }

    #[test]
    fn incoming_operation_and_standalone_yield_cannot_duplicate_the_actor() {
        let mut lineage = complete_lineage();
        lineage.origin = None;
        lineage.endpoint = None;
        lineage.yielded_by.push(CommitLineageYield {
            yield_id: "yield:duplicate".to_owned(),
            actor: session("operator"),
            proof_class: CommitLineageProofClass::RecordExact,
            state: CommitLineageState::Asserted,
            observed_at_ms: None,
            evidence_numbers: vec![1],
        });
        lineage.bounds.returned_events = 2;
        assert!(validate(&lineage).is_err());
    }

    #[test]
    fn edge_and_yield_order_is_deterministic_and_strict() {
        let mut lineage = complete_lineage();
        lineage.origin = None;
        lineage.endpoint = None;
        let source = commit("earlier", '0');
        lineage.edges.push(edge(
            "operation:amend",
            CommitLineageOperationKind::Amend,
            source,
            lineage.edges[0].source.clone(),
        ));
        lineage.bounds.returned_events = 2;
        lineage.bounds.examined_events = 2;
        assert!(validate(&lineage).is_err());
        lineage.edges.sort_by(CommitLineageEdge::stable_cmp);
        validate(&lineage).unwrap();
        lineage.edges.push(lineage.edges[0].clone());
        lineage.bounds.returned_events = 3;
        lineage.bounds.examined_events = 3;
        assert!(validate(&lineage).is_err());
    }

    #[test]
    fn endpoint_requires_exact_scope_observation_time_and_citation() {
        let mut lineage = complete_lineage();
        if let Some(ScopedCommitEndpoint::CurrentAtRef { scope, .. }) = lineage.endpoint.as_mut() {
            scope.kind = ResourceKind::Repository;
        }
        assert!(validate(&lineage).is_err());
        if let Some(ScopedCommitEndpoint::CurrentAtRef {
            scope,
            observed_at_ms,
            ..
        }) = lineage.endpoint.as_mut()
        {
            scope.kind = ResourceKind::Branch;
            *observed_at_ms = -1;
        }
        assert!(validate(&lineage).is_err());
    }
}
