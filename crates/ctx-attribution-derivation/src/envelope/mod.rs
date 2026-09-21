//! Private work-graph observations and derived evidence types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::protocol::ResourceKind;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub observation_id: String,
    pub source_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_session_id: Option<String>,
    pub citation: Citation,
    pub payload: ObservationPayload,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub citation_id: String,
    pub source_id: String,
    pub provider_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_start: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_end: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservationPayload {
    Message { role: MessageRole, text: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionBatch {
    pub facts: Vec<Fact>,
    #[serde(default)]
    pub warnings: Vec<DetectionWarning>,
}

impl DetectionBatch {
    pub fn append(&mut self, mut other: Self) {
        self.facts.append(&mut other.facts);
        self.warnings.append(&mut other.warnings);
    }

    pub fn canonicalize(&mut self) -> Result<(), String> {
        self.facts
            .sort_by(|left, right| left.fact_id.cmp(&right.fact_id));
        let mut canonical = Vec::<Fact>::with_capacity(self.facts.len());
        for mut fact in std::mem::take(&mut self.facts) {
            let Some(existing) = canonical
                .last_mut()
                .filter(|item| item.fact_id == fact.fact_id)
            else {
                canonical.push(fact);
                continue;
            };
            if !existing.same_non_evidence_semantics(&fact) {
                return Err("fact identity collision or ambiguous canonicalization".to_owned());
            }
            existing.evidence.append(&mut fact.evidence);
        }
        for fact in &mut canonical {
            fact.evidence.sort();
            fact.evidence.dedup();
        }
        self.facts = canonical;
        self.warnings.sort_by(|left, right| {
            (&left.detector_id, &left.observation_id, &left.code).cmp(&(
                &right.detector_id,
                &right.observation_id,
                &right.code,
            ))
        });
        self.warnings.dedup();
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DetectionWarning {
    pub detector_id: String,
    pub observation_id: String,
    pub code: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub fact_id: String,
    pub fact_type: String,
    pub subject: ResourceRef,
    pub predicate: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<ResourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<String>,
    pub confidence: Confidence,
    pub state: FactState,
    pub detector_id: String,
    pub detector_version: String,
    pub direct_actor_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<String>,
    #[serde(skip)]
    operation_identity_sha256: Option<String>,
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

impl Fact {
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        fact_type: impl Into<String>,
        subject: ResourceRef,
        predicate: impl Into<String>,
        object: Option<ResourceRef>,
        occurred_at: Option<String>,
        confidence: Confidence,
        state: FactState,
        detector_id: impl Into<String>,
        detector_version: impl Into<String>,
        direct_actor_session_id: impl Into<String>,
        root_session_id: Option<String>,
        mut evidence: Vec<EvidenceRef>,
        attributes: BTreeMap<String, String>,
    ) -> Self {
        evidence.sort();
        evidence.dedup();
        let fact_type = fact_type.into();
        let predicate = predicate.into();
        let detector_id = detector_id.into();
        let detector_version = detector_version.into();
        let direct_actor_session_id = direct_actor_session_id.into();
        let fact_id = logical_fact_id(
            &fact_type,
            &subject,
            &predicate,
            object.as_ref(),
            occurred_at.as_deref(),
            confidence,
            state,
            &detector_id,
            &detector_version,
            &direct_actor_session_id,
            root_session_id.as_deref(),
            None,
            &attributes,
        );
        Self {
            fact_id,
            fact_type,
            subject,
            predicate,
            object,
            occurred_at,
            confidence,
            state,
            detector_id,
            detector_version,
            direct_actor_session_id,
            root_session_id,
            operation_identity_sha256: None,
            evidence,
            attributes,
        }
    }

    #[doc(hidden)]
    pub fn with_operation_identity_sha256(
        mut self,
        operation_identity_sha256: impl Into<String>,
    ) -> Self {
        self.operation_identity_sha256 = Some(operation_identity_sha256.into());
        self.fact_id = logical_fact_id(
            &self.fact_type,
            &self.subject,
            &self.predicate,
            self.object.as_ref(),
            self.occurred_at.as_deref(),
            self.confidence,
            self.state,
            &self.detector_id,
            &self.detector_version,
            &self.direct_actor_session_id,
            self.root_session_id.as_deref(),
            self.operation_identity_sha256.as_deref(),
            &self.attributes,
        );
        self
    }

    #[doc(hidden)]
    pub fn operation_identity_sha256(&self) -> Option<&str> {
        self.operation_identity_sha256.as_deref()
    }

    fn same_non_evidence_semantics(&self, other: &Self) -> bool {
        self.fact_id == other.fact_id
            && self.fact_type == other.fact_type
            && self.subject == other.subject
            && self.predicate == other.predicate
            && self.object == other.object
            && self.occurred_at == other.occurred_at
            && self.confidence == other.confidence
            && self.state == other.state
            && self.detector_id == other.detector_id
            && self.detector_version == other.detector_version
            && self.direct_actor_session_id == other.direct_actor_session_id
            && self.root_session_id == other.root_session_id
            && self.operation_identity_sha256 == other.operation_identity_sha256
            && self.attributes == other.attributes
    }
}

#[allow(clippy::too_many_arguments)]
fn logical_fact_id(
    fact_type: &str,
    subject: &ResourceRef,
    predicate: &str,
    object: Option<&ResourceRef>,
    occurred_at: Option<&str>,
    confidence: Confidence,
    state: FactState,
    detector_id: &str,
    detector_version: &str,
    direct_actor_session_id: &str,
    root_session_id: Option<&str>,
    operation_identity_sha256: Option<&str>,
    attributes: &BTreeMap<String, String>,
) -> String {
    // Evidence is intentionally absent: citations and observation digests are
    // support for this occurrence, not part of the occurrence itself.
    let mut identity = Vec::new();
    identity.extend_from_slice(b"ctx.pro.logical-fact.v4\0");
    identity_text(&mut identity, fact_type);
    identity_resource(&mut identity, subject);
    identity_text(&mut identity, predicate);
    identity_option_resource(&mut identity, object);
    identity_option_text(&mut identity, occurred_at);
    identity_text(&mut identity, confidence_wire_name(confidence));
    identity_text(&mut identity, fact_state_wire_name(state));
    identity_text(&mut identity, detector_id);
    identity_text(&mut identity, detector_version);
    identity_text(&mut identity, direct_actor_session_id);
    identity_option_text(&mut identity, root_session_id);
    identity_option_text(&mut identity, operation_identity_sha256);
    identity.extend_from_slice(&(attributes.len() as u128).to_be_bytes());
    for (key, value) in attributes {
        identity_text(&mut identity, key);
        identity_text(&mut identity, value);
    }
    ctx_attribution_index::stable_id_bytes("fact", &identity)
}

fn identity_resource(target: &mut Vec<u8>, resource: &ResourceRef) {
    identity_text(target, resource.kind.wire_name());
    identity_text(target, &resource.id);
    identity_option_text(target, resource.repository_id.as_deref());
    identity_option_text(target, resource.worktree_id.as_deref());
}

fn identity_option_resource(target: &mut Vec<u8>, resource: Option<&ResourceRef>) {
    target.push(u8::from(resource.is_some()));
    if let Some(resource) = resource {
        identity_resource(target, resource);
    }
}

fn identity_option_text(target: &mut Vec<u8>, value: Option<&str>) {
    target.push(u8::from(value.is_some()));
    if let Some(value) = value {
        identity_text(target, value);
    }
}

fn identity_text(target: &mut Vec<u8>, value: &str) {
    target.extend_from_slice(&(value.len() as u128).to_be_bytes());
    target.extend_from_slice(value.as_bytes());
}

const fn confidence_wire_name(value: Confidence) -> &'static str {
    match value {
        Confidence::Verified => "verified",
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Ambiguous => "ambiguous",
    }
}

const fn fact_state_wire_name(value: FactState) -> &'static str {
    match value {
        FactState::Asserted => "asserted",
        FactState::Ambiguous => "ambiguous",
        FactState::Contradicted => "contradicted",
        FactState::Superseded => "superseded",
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRef {
    pub kind: ResourceKind,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
}

impl ResourceRef {
    pub fn new(kind: ResourceKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            repository_id: None,
            worktree_id: None,
        }
    }

    pub fn in_repository(
        kind: ResourceKind,
        id: impl Into<String>,
        repository_id: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            id: id.into(),
            repository_id: Some(repository_id.into()),
            worktree_id: None,
        }
    }

    pub fn in_worktree(
        kind: ResourceKind,
        id: impl Into<String>,
        repository_id: impl Into<String>,
        worktree_id: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            id: id.into(),
            repository_id: Some(repository_id.into()),
            worktree_id: Some(worktree_id.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Verified,
    High,
    Medium,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactState {
    Asserted,
    Ambiguous,
    Contradicted,
    Superseded,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub citation_id: String,
    pub observation_id: String,
    pub role: String,
    /// Stable digest of the private observation, not its raw payload.
    pub observation_digest: String,
}

impl EvidenceRef {
    pub fn from_observation(observation: &Observation, role: impl Into<String>) -> Self {
        Self {
            citation_id: observation.citation.citation_id.clone(),
            observation_id: observation.observation_id.clone(),
            role: role.into(),
            observation_digest: stable_id(
                "obs",
                &format!(
                    "{}\u{1f}{}\u{1f}{}",
                    observation.observation_id,
                    observation.source_sequence,
                    observation.citation.citation_id
                ),
            ),
        }
    }
}

/// A stable non-cryptographic identity for derived rows. Source integrity uses
/// the cryptographic digest supplied by the OSS host; this function only makes
/// deterministic graph identifiers and deliberately has no secret key.
#[must_use]
pub fn stable_id(prefix: &str, value: &str) -> String {
    ctx_attribution_index::stable_id(prefix, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_ids_are_repeatable_and_domain_separated() {
        assert_eq!(stable_id("fact", "same"), stable_id("fact", "same"));
        assert_ne!(stable_id("fact", "same"), stable_id("obs", "same"));
        assert_ne!(stable_id("fact", "same"), stable_id("fact", "other"));
    }

    #[test]
    fn logical_facts_merge_evidence_but_preserve_semantic_occurrences() {
        let first_evidence = EvidenceRef {
            citation_id: "citation-1".to_owned(),
            observation_id: "observation-1".to_owned(),
            role: "supports".to_owned(),
            observation_digest: "digest-1".to_owned(),
        };
        let second_evidence = EvidenceRef {
            citation_id: "citation-2".to_owned(),
            observation_id: "observation-2".to_owned(),
            role: "corroborates".to_owned(),
            observation_digest: "digest-2".to_owned(),
        };
        let create = |subject: &str,
                      predicate: &str,
                      object: &str,
                      occurred_at: &str,
                      confidence: Confidence,
                      state: FactState,
                      actor: &str,
                      root: &str,
                      operation: &str,
                      attributes: BTreeMap<String, String>,
                      evidence: Vec<EvidenceRef>| {
            Fact::create(
                "git.commit.produced",
                ResourceRef::in_repository(ResourceKind::Commit, subject, "repo-1"),
                predicate,
                Some(ResourceRef::new(ResourceKind::Session, object)),
                Some(occurred_at.to_owned()),
                confidence,
                state,
                "core.repository",
                "1",
                actor,
                Some(root.to_owned()),
                evidence,
                attributes,
            )
            .with_operation_identity_sha256(operation)
        };
        let operation_a = "a".repeat(64);
        let operation_b = "b".repeat(64);
        let baseline = create(
            &"a".repeat(40),
            "produced_by",
            "worker-a",
            "1700000000000",
            Confidence::Verified,
            FactState::Asserted,
            "worker-a",
            "root-a",
            &operation_a,
            BTreeMap::new(),
            vec![first_evidence.clone()],
        );
        let alternate_evidence = create(
            &"a".repeat(40),
            "produced_by",
            "worker-a",
            "1700000000000",
            Confidence::Verified,
            FactState::Asserted,
            "worker-a",
            "root-a",
            &operation_a,
            BTreeMap::new(),
            vec![second_evidence.clone()],
        );

        assert_eq!(baseline.fact_id, alternate_evidence.fact_id);
        let mut batch = DetectionBatch {
            facts: vec![baseline.clone(), baseline.clone(), alternate_evidence],
            warnings: Vec::new(),
        };
        batch.canonicalize().expect("merge supporting evidence");
        assert_eq!(batch.facts.len(), 1);
        assert_eq!(
            batch.facts[0].evidence,
            vec![first_evidence, second_evidence]
        );

        for distinct in [
            create(
                &"a".repeat(40),
                "produced_by",
                "worker-a",
                "1700000000001",
                Confidence::Verified,
                FactState::Asserted,
                "worker-a",
                "root-a",
                &operation_a,
                BTreeMap::new(),
                Vec::new(),
            ),
            create(
                &"a".repeat(40),
                "produced_by",
                "worker-b",
                "1700000000000",
                Confidence::Verified,
                FactState::Asserted,
                "worker-b",
                "root-a",
                &operation_a,
                BTreeMap::new(),
                Vec::new(),
            ),
            create(
                &"a".repeat(40),
                "produced_by",
                "worker-a",
                "1700000000000",
                Confidence::Verified,
                FactState::Asserted,
                "worker-a",
                "root-b",
                &operation_a,
                BTreeMap::new(),
                Vec::new(),
            ),
            create(
                &"a".repeat(40),
                "produced_by",
                "worker-a",
                "1700000000000",
                Confidence::Verified,
                FactState::Asserted,
                "worker-a",
                "root-a",
                &operation_b,
                BTreeMap::new(),
                Vec::new(),
            ),
            create(
                &"b".repeat(40),
                "produced_by",
                "worker-a",
                "1700000000000",
                Confidence::Verified,
                FactState::Asserted,
                "worker-a",
                "root-a",
                &operation_a,
                BTreeMap::new(),
                Vec::new(),
            ),
            create(
                &"a".repeat(40),
                "created_by",
                "worker-a",
                "1700000000000",
                Confidence::Verified,
                FactState::Asserted,
                "worker-a",
                "root-a",
                &operation_a,
                BTreeMap::new(),
                Vec::new(),
            ),
            create(
                &"a".repeat(40),
                "produced_by",
                "worker-a",
                "1700000000000",
                Confidence::High,
                FactState::Asserted,
                "worker-a",
                "root-a",
                &operation_a,
                BTreeMap::new(),
                Vec::new(),
            ),
            create(
                &"a".repeat(40),
                "produced_by",
                "worker-a",
                "1700000000000",
                Confidence::Verified,
                FactState::Ambiguous,
                "worker-a",
                "root-a",
                &operation_a,
                BTreeMap::new(),
                Vec::new(),
            ),
            create(
                &"a".repeat(40),
                "produced_by",
                "worker-a",
                "1700000000000",
                Confidence::Verified,
                FactState::Asserted,
                "worker-a",
                "root-a",
                &operation_a,
                BTreeMap::from([("semantic".to_owned(), "distinct".to_owned())]),
                Vec::new(),
            ),
        ] {
            assert_ne!(baseline.fact_id, distinct.fact_id);
        }
    }

    #[test]
    fn canonicalization_fails_closed_on_equal_ids_with_unequal_facts() {
        let fact = Fact::create(
            "file.touched",
            ResourceRef::new(ResourceKind::File, "src/lib.rs"),
            "read",
            None,
            Some("1".to_owned()),
            Confidence::Verified,
            FactState::Asserted,
            "fixture",
            "1",
            "worker",
            Some("root".to_owned()),
            Vec::new(),
            BTreeMap::new(),
        );
        let mut conflicting = fact.clone();
        conflicting.predicate = "written".to_owned();
        let mut batch = DetectionBatch {
            facts: vec![fact, conflicting],
            warnings: Vec::new(),
        };

        assert!(batch.canonicalize().is_err());
    }

    #[test]
    fn multiple_real_producers_remain_distinct_logical_facts() {
        let create = |actor: &str, root: &str| {
            Fact::create(
                "git.commit.produced",
                ResourceRef::in_repository(ResourceKind::Commit, "c".repeat(40), "repo-1"),
                "produced_by",
                Some(ResourceRef::new(ResourceKind::Session, actor)),
                Some("1700000000000".to_owned()),
                Confidence::Verified,
                FactState::Asserted,
                "core.repository",
                "1",
                actor,
                Some(root.to_owned()),
                Vec::new(),
                BTreeMap::new(),
            )
            .with_operation_identity_sha256("d".repeat(64))
        };
        let mut batch = DetectionBatch {
            facts: vec![
                create("producer-session-a", "root-session-a"),
                create("producer-session-b", "root-session-b"),
            ],
            warnings: Vec::new(),
        };

        batch.canonicalize().expect("distinct real producers");
        assert_eq!(batch.facts.len(), 2);
        assert_ne!(batch.facts[0].fact_id, batch.facts[1].fact_id);
    }
}
