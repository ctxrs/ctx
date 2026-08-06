use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use serde::{Deserialize, Serialize};

use crate::{
    query::{canonical_logical_repository_id, same_resource_identity, validate_evidence_numbers},
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
    pub logical_repository_id: String,
    pub object_format: GitObjectFormat,
    pub oid: String,
}

impl ExactCommitRef {
    fn validate(&self) -> Result<(), ProtocolError> {
        self.resource.validate()?;
        if self.resource.kind != ResourceKind::Commit {
            return Err(corrupt("exact commit reference is not a commit resource"));
        }
        validate_logical_repository_id(&self.logical_repository_id)?;
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
        self.logical_repository_id == other.logical_repository_id
            && self.object_format == other.object_format
            && self.oid == other.oid
    }

    fn shares_object_domain(&self, other: &Self) -> bool {
        self.logical_repository_id == other.logical_repository_id
            && self.object_format == other.object_format
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
        validate_operation_id(&self.operation_id)?;
        self.source.validate()?;
        self.result.validate()?;
        if !self.source.shares_object_domain(&self.result) {
            return Err(corrupt(
                "commit lineage operation crosses a repository object domain",
            ));
        }
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
        validate_causal_proof(self.state, self.proof_class)?;
        validate_observed_at(self.observed_at_ms)?;
        validate_evidence_numbers(&self.evidence_numbers, available, referenced)
    }

    fn same_operation_metadata(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.relation_class == other.relation_class
            && self.actor == other.actor
            && self.proof_class == other.proof_class
            && self.state == other.state
            && self.observed_at_ms == other.observed_at_ms
            && self.evidence_numbers == other.evidence_numbers
    }

    fn same_shared_operation_metadata(&self, other: &CommitLineageYield) -> bool {
        self.actor == other.actor
            && self.proof_class == other.proof_class
            && self.state == other.state
            && self.observed_at_ms == other.observed_at_ms
            && self.evidence_numbers == other.evidence_numbers
    }

    fn stable_cmp(&self, other: &Self) -> Ordering {
        (
            self.operation_id.as_str(),
            self.kind,
            self.source.logical_repository_id.as_str(),
            self.source.object_format,
            self.source.oid.as_str(),
            self.result.object_format,
            self.result.oid.as_str(),
        )
            .cmp(&(
                other.operation_id.as_str(),
                other.kind,
                other.source.logical_repository_id.as_str(),
                other.source.object_format,
                other.source.oid.as_str(),
                other.result.object_format,
                other.result.oid.as_str(),
            ))
    }
}

/// A weaker event-scoped yield attribution for the exact requested object.
/// It is present only when no incoming operation edge explains that object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitLineageYield {
    pub yield_id: String,
    pub operation_id: String,
    pub logical_repository_id: String,
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
        validate_operation_id(&self.operation_id)?;
        validate_logical_repository_id(&self.logical_repository_id)?;
        validate_session(&self.actor, "commit lineage yield actor")?;
        validate_causal_proof(self.state, self.proof_class)?;
        validate_observed_at(self.observed_at_ms)?;
        validate_evidence_numbers(&self.evidence_numbers, available, referenced)
    }

    fn same_operation_metadata(&self, other: &Self) -> bool {
        self.actor == other.actor
            && self.proof_class == other.proof_class
            && self.state == other.state
            && self.observed_at_ms == other.observed_at_ms
            && self.evidence_numbers == other.evidence_numbers
    }

    fn stable_cmp(&self, other: &Self) -> Ordering {
        (
            self.operation_id.as_str(),
            self.yield_id.as_str(),
            self.actor.id.as_str(),
        )
            .cmp(&(
                other.operation_id.as_str(),
                other.yield_id.as_str(),
                other.actor.id.as_str(),
            ))
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
        repository: &ResourceRef,
        available: &BTreeSet<u32>,
        referenced: &mut BTreeSet<u32>,
    ) -> Result<(), ProtocolError> {
        self.requested.validate()?;
        repository.validate()?;
        if repository.kind != ResourceKind::Repository {
            return Err(corrupt(
                "commit lineage resolved repository is not a repository resource",
            ));
        }
        if !same_resource_identity(&self.requested.resource, target)
            || self.requested.oid != target.display
        {
            return Err(corrupt(
                "commit lineage requested object does not exactly match the resolved blame target",
            ));
        }
        if canonical_logical_repository_id(&repository.display).as_ref()
            != self.requested.logical_repository_id
        {
            return Err(corrupt(
                "commit lineage requested object does not belong to the resolved repository",
            ));
        }

        let mut edge_operations: BTreeMap<&str, &CommitLineageEdge> = BTreeMap::new();
        let mut operation_ids = BTreeSet::new();
        for edge in &self.edges {
            edge.validate(available, referenced)?;
            if !edge.source.shares_object_domain(&self.requested)
                || !edge.result.shares_object_domain(&self.requested)
            {
                return Err(corrupt(
                    "commit lineage edge crosses the requested repository object domain",
                ));
            }
            operation_ids.insert(edge.operation_id.as_str());
            if let Some(first) = edge_operations.get(edge.operation_id.as_str()) {
                if !edge.same_operation_metadata(first) {
                    return Err(corrupt(
                        "commit lineage mappings for one operation disagree on metadata",
                    ));
                }
            } else {
                edge_operations.insert(edge.operation_id.as_str(), edge);
            }
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

        let mut yielded_operations: BTreeMap<&str, &CommitLineageYield> = BTreeMap::new();
        let mut yield_ids = BTreeSet::new();
        for yielded_by in &self.yielded_by {
            yielded_by.validate(available, referenced)?;
            if yielded_by.logical_repository_id != self.requested.logical_repository_id {
                return Err(corrupt(
                    "commit lineage yield crosses the requested repository domain",
                ));
            }
            if !yield_ids.insert(yielded_by.yield_id.as_str()) {
                return Err(corrupt(
                    "commit lineage contains a duplicate yield record identity",
                ));
            }
            operation_ids.insert(yielded_by.operation_id.as_str());
            if let Some(first) = yielded_operations.get(yielded_by.operation_id.as_str()) {
                if !yielded_by.same_operation_metadata(first) {
                    return Err(corrupt(
                        "commit lineage yields for one operation disagree on metadata",
                    ));
                }
            } else {
                yielded_operations.insert(yielded_by.operation_id.as_str(), yielded_by);
            }
            if edge_operations
                .get(yielded_by.operation_id.as_str())
                .is_some_and(|edge| !edge.same_shared_operation_metadata(yielded_by))
            {
                return Err(corrupt(
                    "commit lineage mappings and yields for one operation disagree on metadata",
                ));
            }
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

        self.validate_operation_connectivity(&operation_ids)?;

        let actual_returned_events = operation_ids.len();
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
            if !origin.shares_object_domain(&self.requested) {
                return Err(corrupt(
                    "commit lineage origin crosses the requested repository object domain",
                ));
            }
            let roots = self.indegree_zero_commits();
            if roots.len() != 1 || !roots[0].same_identity(origin) {
                return Err(corrupt(
                    "commit lineage origin is not the unique indegree-zero root",
                ));
            }
            if !self.directed_reachable(origin, &self.requested) {
                return Err(corrupt(
                    "commit lineage origin is not a directed ancestor of the requested commit",
                ));
            }
        }
        if let Some(endpoint) = &self.endpoint {
            endpoint.validate(available, referenced)?;
            if !endpoint.commit().shares_object_domain(&self.requested) {
                return Err(corrupt(
                    "scoped commit endpoint crosses the requested repository object domain",
                ));
            }
            if !self.directed_reachable(&self.requested, endpoint.commit()) {
                return Err(corrupt(
                    "scoped commit endpoint is not the requested commit or a directed descendant",
                ));
            }
        }
        Ok(())
    }

    fn validate_operation_connectivity(
        &self,
        operation_ids: &BTreeSet<&str>,
    ) -> Result<(), ProtocolError> {
        let mut connected_commits = vec![&self.requested];
        let mut connected_operations: BTreeSet<&str> = self
            .yielded_by
            .iter()
            .map(|yielded_by| yielded_by.operation_id.as_str())
            .collect();

        loop {
            let mut changed = false;
            for edge in &self.edges {
                if connected_operations.contains(edge.operation_id.as_str())
                    || !connected_commits.iter().any(|commit| {
                        commit.same_identity(&edge.source) || commit.same_identity(&edge.result)
                    })
                {
                    continue;
                }
                connected_operations.insert(edge.operation_id.as_str());
                for mapping in self
                    .edges
                    .iter()
                    .filter(|mapping| mapping.operation_id == edge.operation_id)
                {
                    for commit in [&mapping.source, &mapping.result] {
                        if !connected_commits
                            .iter()
                            .any(|connected| connected.same_identity(commit))
                        {
                            connected_commits.push(commit);
                        }
                    }
                }
                changed = true;
            }
            if !changed {
                break;
            }
        }

        if connected_operations.len() != operation_ids.len()
            || operation_ids
                .iter()
                .any(|operation_id| !connected_operations.contains(operation_id))
        {
            return Err(corrupt(
                "commit lineage contains an operation disconnected from the requested commit",
            ));
        }
        Ok(())
    }

    fn indegree_zero_commits(&self) -> Vec<&ExactCommitRef> {
        let mut commits = vec![&self.requested];
        for edge in &self.edges {
            for commit in [&edge.source, &edge.result] {
                if !commits
                    .iter()
                    .any(|candidate| candidate.same_identity(commit))
                {
                    commits.push(commit);
                }
            }
        }
        commits
            .into_iter()
            .filter(|commit| {
                !self
                    .edges
                    .iter()
                    .any(|edge| edge.result.same_identity(commit))
            })
            .collect()
    }

    fn directed_reachable(&self, start: &ExactCommitRef, target: &ExactCommitRef) -> bool {
        let mut reachable = vec![start];
        loop {
            if reachable.iter().any(|commit| commit.same_identity(target)) {
                return true;
            }
            let prior_len = reachable.len();
            for edge in &self.edges {
                if reachable
                    .iter()
                    .any(|commit| commit.same_identity(&edge.source))
                    && !reachable
                        .iter()
                        .any(|commit| commit.same_identity(&edge.result))
                {
                    reachable.push(&edge.result);
                }
            }
            if reachable.len() == prior_len {
                return false;
            }
        }
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

fn validate_logical_repository_id(value: &str) -> Result<(), ProtocolError> {
    validate_text(value, "logical repository ID")?;
    if canonical_logical_repository_id(value).as_ref() != value {
        return Err(corrupt("logical repository ID is not canonical"));
    }
    Ok(())
}

fn validate_operation_id(value: &str) -> Result<(), ProtocolError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(corrupt(
            "commit lineage operation ID is not a canonical lowercase SHA-256 digest",
        ));
    }
    Ok(())
}

fn validate_causal_proof(
    state: CommitLineageState,
    proof_class: CommitLineageProofClass,
) -> Result<(), ProtocolError> {
    if state == CommitLineageState::Asserted
        && proof_class != CommitLineageProofClass::RepositoryVerified
    {
        return Err(corrupt(
            "asserted commit operation yields require repository-verified proof",
        ));
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
#[path = "query_lineage/tests.rs"]
mod tests;
