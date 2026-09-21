use std::collections::{BTreeMap, BTreeSet};

use super::*;

#[test]
fn logical_repository_identity_is_opaque_through_the_exact_public_bound() {
    for repository in [
        "opaque#fragment?query/../tail/",
        "https://GitHub.COM/org/repo",
        "forge:LOCALHOST/repo",
        "forge:GitHub.COM/",
    ] {
        validate_repository_id(repository).expect("bounded opaque repository identity");
    }
    validate_repository_id(&"r".repeat(4_097)).expect("identity beyond 4 KiB");
    validate_repository_id(&"r".repeat(MAX_REPOSITORY_ID_BYTES))
        .expect("exact public repository bound");
    assert!(validate_repository_id("").is_err());
    assert!(validate_repository_id(&"r".repeat(MAX_REPOSITORY_ID_BYTES + 1)).is_err());
}

#[test]
fn schema_critical_families_keep_reference_authority_separate() -> Result<(), ServingModelError> {
    let families = SCHEMA_CRITICAL_FACT_FAMILIES
        .into_iter()
        .map(FactFamily::new)
        .collect::<Result<BTreeSet<_>, _>>()?;
    assert_eq!(families.len(), SCHEMA_CRITICAL_FACT_FAMILIES.len());

    for referenced in [
        GIT_COMMIT_REFERENCED,
        PULL_REQUEST_REFERENCED,
        FORGE_PULL_REQUEST_REFERENCED,
    ] {
        assert_eq!(
            FactFamily::new(referenced)?.authority(),
            FactAuthority::Reference
        );
    }
    assert_eq!(
        FactFamily::new(FILE_MENTIONED)?.authority(),
        FactAuthority::LowerAuthorityMention
    );
    assert_eq!(
        FactFamily::new(GIT_COMMIT_INSPECTED)?.authority(),
        FactAuthority::Inspection
    );
    for produced in [
        GIT_COMMIT_PRODUCED,
        GIT_COMMIT_REPLACED,
        PULL_REQUEST_PRODUCED,
        FORGE_PULL_REQUEST_PRODUCED,
        FORGE_CREATE,
        FORGE_MERGE,
    ] {
        assert_eq!(
            FactFamily::new(produced)?.authority(),
            FactAuthority::OwnedOutcome
        );
    }
    Ok(())
}

#[test]
fn producer_authority_family_classification_is_exhaustive() -> Result<(), ServingModelError> {
    let authority_families = BTreeSet::from([
        GIT_COMMIT_PRODUCED,
        GIT_COMMIT_REPLACED,
        GIT_COMMIT_AMBIGUOUS,
        GIT_COMMIT_CHERRY_PICKED,
        PULL_REQUEST_PRODUCED,
        FORGE_PULL_REQUEST_PRODUCED,
        FORGE_CREATE,
        FORGE_MERGE,
    ]);
    let owned_outcome_families = BTreeSet::from([
        GIT_COMMIT_PRODUCED,
        GIT_COMMIT_REPLACED,
        PULL_REQUEST_PRODUCED,
        FORGE_PULL_REQUEST_PRODUCED,
        FORGE_CREATE,
        FORGE_MERGE,
    ]);

    for family in SCHEMA_CRITICAL_FACT_FAMILIES {
        assert_eq!(
            fact_type_may_confer_producer_authority(family),
            authority_families.contains(family),
            "producer-authority classification for {family}"
        );
        assert_eq!(
            FactFamily::new(family)?.authority() == FactAuthority::OwnedOutcome,
            owned_outcome_families.contains(family),
            "owned-outcome classification for {family}"
        );
    }
    assert!(!fact_type_may_confer_producer_authority(
        "unknown.authority.family"
    ));
    Ok(())
}

#[test]
fn authority_matrix_requires_exact_family_origin_state_and_confidence() {
    let authorities = [
        (FactAuthority::OwnedOutcome, AuthorityBoundary::Ownership),
        (
            FactAuthority::DirectObservation,
            AuthorityBoundary::FileIdentity,
        ),
        (
            FactAuthority::AccessAuthorization,
            AuthorityBoundary::LiveRepositoryAccess,
        ),
        (FactAuthority::Alias, AuthorityBoundary::RepositoryAlias),
    ];
    let origins = [
        ObservationOrigin::Direct,
        ObservationOrigin::Copied {
            origin_event_id: "event".to_owned(),
            origin_session_id: "session".to_owned(),
        },
        ObservationOrigin::Later {
            origin_event_sequence: 7,
        },
        ObservationOrigin::Unspecified,
    ];
    let states = [
        ServingFactState::Asserted,
        ServingFactState::Ambiguous,
        ServingFactState::Contradicted,
        ServingFactState::Superseded,
    ];
    let confidences = [
        ServingConfidence::Verified,
        ServingConfidence::High,
        ServingConfidence::Medium,
        ServingConfidence::Ambiguous,
    ];

    for (authority, matching_boundary) in authorities {
        for boundary in [
            AuthorityBoundary::Ownership,
            AuthorityBoundary::FileIdentity,
            AuthorityBoundary::LiveRepositoryAccess,
            AuthorityBoundary::RepositoryAlias,
        ] {
            for origin in &origins {
                for state in states {
                    for confidence in confidences {
                        assert_eq!(
                            authority_eligible(authority, boundary, origin, state, confidence,),
                            boundary == matching_boundary
                                && *origin == ObservationOrigin::Direct
                                && state == ServingFactState::Asserted
                                && confidence == ServingConfidence::Verified
                        );
                    }
                }
            }
        }
    }
    for authority in [
        FactAuthority::Reference,
        FactAuthority::Inspection,
        FactAuthority::LowerAuthorityMention,
        FactAuthority::Unspecified,
    ] {
        assert!(!authority_eligible(
            authority,
            AuthorityBoundary::Ownership,
            &ObservationOrigin::Direct,
            ServingFactState::Asserted,
            ServingConfidence::Verified,
        ));
    }
}

#[test]
fn possible_commit_production_evidence_rejects_pull_request_merge_outcomes() {
    let mut record = ServingRecord {
        record_id: "record".to_owned(),
        event_owner: EventOwner {
            source_id: "source".to_owned(),
            event_id: "event".to_owned(),
            direct_session_id: "session".to_owned(),
            root_session_id: Some("root".to_owned()),
            event_sequence: 7,
        },
        repository_id: "forge:github.com/ctxrs/ctx".to_owned(),
        fact_family: FactFamily::new(GIT_COMMIT_PRODUCED).expect("commit production family"),
        subject: ServingResource {
            kind: ResourceKind::Commit.wire_name().to_owned(),
            id: "1".repeat(40),
            repository_id: Some("forge:github.com/ctxrs/ctx".to_owned()),
            worktree_id: None,
        },
        object: None,
        scope: None,
        direct_actor: None,
        occurred_at_unix_ms: None,
        confidence: ServingConfidence::Verified,
        state: ServingFactState::Ambiguous,
        detector_id: "core.repository".to_owned(),
        detector_revision: CORE_REPOSITORY_CONTRACT_REVISION.to_string(),
        origin: ObservationOrigin::Later {
            origin_event_sequence: 7,
        },
        line_range: None,
        index_terms: Vec::new(),
        attributes: BTreeMap::from([
            (
                "outcome_capture_revision".to_owned(),
                AttributeValue::String(CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION.to_string()),
            ),
            (
                "origin_event_sequence".to_owned(),
                AttributeValue::Integer(7),
            ),
            (
                "outcome_kind".to_owned(),
                AttributeValue::String("commit".to_owned()),
            ),
            (
                "result_record_sha256".to_owned(),
                AttributeValue::String("a".repeat(64)),
            ),
        ]),
        citations: Vec::new(),
    };

    assert!(record.is_possible_commit_production_evidence());
    record.attributes.insert(
        "outcome_kind".to_owned(),
        AttributeValue::String("pull_request_merged".to_owned()),
    );
    assert!(!record.is_possible_commit_production_evidence());
}
