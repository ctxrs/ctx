use super::{SegmentCoreCoverage, SegmentEventOwner, SegmentPreparedUnit};
use crate::ingest::ProducerAuthorityDisposition;

#[test]
fn staged_private_semantics_round_trip_without_synthetic_root() {
    let prepared = SegmentPreparedUnit {
        origin_event_id: "event:fixture".to_owned(),
        producer_authority_disposition: ProducerAuthorityDisposition::IneligibleCopied,
        stable_entities: Vec::new(),
        facts: Vec::new(),
        evidence: None,
        coverage: SegmentCoreCoverage::default(),
    };
    let encoded = serde_json::to_value(&prepared).expect("prepared unit JSON");
    assert_eq!(
        encoded["producer_authority_disposition"],
        "ineligible_copied"
    );
    assert_eq!(
        serde_json::from_value::<SegmentPreparedUnit>(encoded).expect("prepared unit round trip"),
        prepared
    );

    let owner = SegmentEventOwner {
        source_id: "source:fixture".to_owned(),
        event_id: "event:fixture".to_owned(),
        direct_session_id: "session:fixture".to_owned(),
        root_session_id: None,
        event_sequence: 1,
    };
    let encoded = serde_json::to_value(&owner).expect("event owner JSON");
    assert!(encoded.get("root_session_id").is_none());
    assert_eq!(
        serde_json::from_value::<SegmentEventOwner>(encoded).expect("event owner round trip"),
        owner
    );
}
