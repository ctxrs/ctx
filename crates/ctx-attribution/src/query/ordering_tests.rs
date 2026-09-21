use super::*;
use crate::query::ResourceId;

#[test]
fn dogfood_incidental_recency_cannot_outrank_direct_operations() {
    let mut evidence = [
        (
            "mention",
            ranked_fact("a-fact", "mentioned_by", Some(900), "a-session"),
        ),
        (
            "reference",
            ranked_fact("b-fact", "referenced_by", Some(800), "a-session"),
        ),
        (
            "inspection",
            ranked_fact("c-fact", "inspected_by", Some(700), "a-session"),
        ),
        (
            "copied",
            ranked_fact("d-fact", "copied_from", Some(600), "a-session"),
        ),
        (
            "possible",
            fact(
                "e-fact",
                "possibly_produced_by",
                FactState::Ambiguous,
                Confidence::Ambiguous,
            ),
        ),
        (
            "modified",
            ranked_fact("z-modification", "modified_by", Some(100), "z-session"),
        ),
        (
            "produced",
            ranked_fact("z-production", "produced_by", None, "z-session"),
        ),
    ];

    evidence.sort_by(|left, right| compare_facts(&left.1, &right.1));

    assert_eq!(
        evidence.map(|item| item.0),
        [
            "produced",
            "modified",
            "possible",
            "copied",
            "inspection",
            "reference",
            "mention",
        ]
    );
}

#[test]
fn direct_rank_requires_asserted_verified_truth() {
    let asserted_verified = fact(
        "direct",
        "produced_by",
        FactState::Asserted,
        Confidence::Verified,
    );
    let merely_high = fact("high", "produced_by", FactState::Asserted, Confidence::High);
    let ambiguous = fact(
        "possible",
        "possibly_produced_by",
        FactState::Ambiguous,
        Confidence::Ambiguous,
    );
    let modified = fact(
        "modified",
        "modified_by",
        FactState::Asserted,
        Confidence::Verified,
    );
    let amended = fact(
        "amended",
        "amended_by",
        FactState::Asserted,
        Confidence::Verified,
    );
    let copied = fact(
        "copied",
        "copied_from",
        FactState::Ambiguous,
        Confidence::Ambiguous,
    );
    let inspected = fact(
        "inspected",
        "inspected_by",
        FactState::Asserted,
        Confidence::Verified,
    );
    let referenced = fact(
        "referenced",
        "referenced_by",
        FactState::Asserted,
        Confidence::Verified,
    );
    let mentioned = fact(
        "mentioned",
        "mentioned_by",
        FactState::Asserted,
        Confidence::Verified,
    );

    assert_eq!(
        producer_first_priority(&asserted_verified),
        ProducerFirstPriority::DirectProduction
    );
    assert_eq!(
        producer_first_priority(&merely_high),
        ProducerFirstPriority::Other
    );
    assert_eq!(
        producer_first_priority(&ambiguous),
        ProducerFirstPriority::PossibleProduction
    );
    assert_eq!(
        producer_first_priority(&modified),
        ProducerFirstPriority::DirectModification
    );
    assert_eq!(
        producer_first_priority(&amended),
        ProducerFirstPriority::DirectModification
    );
    assert_eq!(
        producer_first_priority(&copied),
        ProducerFirstPriority::Copied
    );
    assert_eq!(
        producer_first_priority(&inspected),
        ProducerFirstPriority::Inspected
    );
    assert_eq!(
        producer_first_priority(&referenced),
        ProducerFirstPriority::Referenced
    );
    assert_eq!(
        producer_first_priority(&mentioned),
        ProducerFirstPriority::Mentioned
    );
}

#[test]
fn complete_tie_breaks_make_pagination_repeatable() {
    let mut keys = [
        ProducerFirstOrderKey::new(
            ProducerFirstPriority::Referenced,
            None,
            "session-b",
            "fact-c",
        ),
        ProducerFirstOrderKey::new(
            ProducerFirstPriority::Referenced,
            Some(10),
            "session-b",
            "fact-b",
        ),
        ProducerFirstOrderKey::new(
            ProducerFirstPriority::Referenced,
            Some(10),
            "session-a",
            "fact-z",
        ),
        ProducerFirstOrderKey::new(
            ProducerFirstPriority::Referenced,
            Some(10),
            "session-b",
            "fact-a",
        ),
    ];
    keys.sort_by(compare_producer_evidence);
    let expected = keys;
    let cursor = keys[1];
    let resumed = keys
        .into_iter()
        .filter(|key| compare_producer_evidence(key, &cursor).is_gt())
        .collect::<Vec<_>>();

    assert_eq!(resumed, expected[2..]);
    assert!(
        expected
            .windows(2)
            .all(|pair| compare_producer_evidence(&pair[0], &pair[1]).is_lt())
    );
}

#[test]
fn one_asserted_producer_precedes_incidental_evidence_without_changing_truth() {
    let mut evidence = [
        ranked_fact("a-reference", "referenced_by", Some(900), "a-session"),
        ranked_fact("b-inspection", "inspected_by", Some(800), "a-session"),
        fact(
            "c-possible",
            "possibly_produced_by",
            FactState::Ambiguous,
            Confidence::Ambiguous,
        ),
        ranked_fact("z-producer", "produced_by", Some(100), "z-session"),
    ];
    let truth_before = truth_snapshot(&evidence);

    evidence.sort_by(compare_facts);

    assert_eq!(
        evidence.each_ref().map(|fact| fact.predicate.as_str()),
        [
            "produced_by",
            "possibly_produced_by",
            "inspected_by",
            "referenced_by",
        ]
    );
    assert_eq!(truth_snapshot(&evidence), truth_before);
}

fn fact(id: &str, predicate: &str, state: FactState, confidence: Confidence) -> Fact {
    Fact {
        id: id.to_owned(),
        fact_type: "test.fact".to_owned(),
        subject: ResourceId("target".to_owned()),
        predicate: predicate.to_owned(),
        object: Some(ResourceId("session".to_owned())),
        value: None,
        occurred_at_ms: None,
        confidence,
        state,
        detector_version: "test".to_owned(),
        root_run: None,
        direct_actor: None,
        citations: Vec::new(),
    }
}

fn ranked_fact(id: &str, predicate: &str, occurred_at_ms: Option<i64>, session: &str) -> Fact {
    let mut fact = fact(id, predicate, FactState::Asserted, Confidence::Verified);
    fact.occurred_at_ms = occurred_at_ms;
    fact.object = Some(ResourceId(session.to_owned()));
    fact
}

fn truth_snapshot(facts: &[Fact]) -> Vec<(String, String, FactState, Confidence)> {
    let mut snapshot = facts
        .iter()
        .map(|fact| {
            (
                fact.id.clone(),
                fact.predicate.clone(),
                fact.state,
                fact.confidence,
            )
        })
        .collect::<Vec<_>>();
    snapshot.sort_by(|left, right| left.0.cmp(&right.0));
    snapshot
}
