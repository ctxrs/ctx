use std::collections::HashSet;

use super::{HydrationFailure, HydrationFailureKind as K, SourceBackedErrorClass as C};

const PRECISE_FAILURES: [(K, &str, C); 10] = [
    (
        K::TemporarilyUnavailable,
        "temporarily_unavailable",
        C::Unavailable,
    ),
    (
        K::ConfirmedDeleted,
        "confirmed_deleted",
        C::ConfirmedDeleted,
    ),
    (
        K::StaleSourceEvidence,
        "stale_source_evidence",
        C::StaleEvidence,
    ),
    (
        K::StaleRecordEvidence,
        "stale_record_evidence",
        C::StaleEvidence,
    ),
    (K::MissingRecord, "missing_record", C::StaleEvidence),
    (K::MalformedSource, "malformed_source", C::Malformed),
    (
        K::UnsupportedParserRevision,
        "unsupported_parser_revision",
        C::Unsupported,
    ),
    (K::InvalidLocator, "invalid_locator", C::InvalidRequest),
    (K::InvalidRequest, "invalid_request", C::InvalidRequest),
    (K::Internal, "internal", C::Internal),
];

const ERROR_CLASSES: [(C, &str); 7] = [
    (C::Unavailable, "unavailable"),
    (C::ConfirmedDeleted, "confirmed_deleted"),
    (C::StaleEvidence, "stale_evidence"),
    (C::Malformed, "malformed"),
    (C::Unsupported, "unsupported"),
    (C::InvalidRequest, "invalid_request"),
    (C::Internal, "internal"),
];

#[test]
fn precise_failure_wire_spellings_round_trip_exhaustively() {
    for (kind, spelling, _) in PRECISE_FAILURES {
        assert_eq!(kind.as_str(), spelling);
        assert_eq!(K::parse(spelling), Some(kind));
    }
}

#[test]
fn precise_failure_parsing_rejects_unknown_or_class_spellings() {
    for value in [
        "",
        "unavailable",
        "stale_evidence",
        "TemporarilyUnavailable",
        "temporarily-unavailable",
        "internal ",
        "unknown",
    ] {
        assert_eq!(K::parse(value), None, "{value}");
    }
}

#[test]
fn source_backed_error_classes_round_trip_exhaustively() {
    for (class, spelling) in ERROR_CLASSES {
        assert_eq!(class.as_str(), spelling);
        assert_eq!(C::parse(spelling), Some(class));
    }
}

#[test]
fn source_backed_error_class_parsing_rejects_unknown_or_precise_spellings() {
    for value in [
        "",
        "temporarily_unavailable",
        "stale_source_evidence",
        "ConfirmedDeleted",
        "confirmed-deleted",
        "internal ",
        "unknown",
    ] {
        assert_eq!(C::parse(value), None, "{value}");
    }
}

#[test]
fn every_precise_failure_maps_to_exactly_one_of_seven_classes() {
    for (kind, _, expected_class) in PRECISE_FAILURES {
        assert_eq!(kind.class(), expected_class);
    }

    let observed = PRECISE_FAILURES
        .into_iter()
        .map(|(kind, _, _)| kind.class())
        .collect::<HashSet<_>>();
    let expected = ERROR_CLASSES
        .into_iter()
        .map(|(class, _)| class)
        .collect::<HashSet<_>>();
    assert_eq!(observed, expected);
}

#[test]
fn caller_locator_and_request_defects_remain_invalid_request() {
    for kind in [K::InvalidLocator, K::InvalidRequest] {
        let failure = HydrationFailure::new(kind, "/private/source/path");
        assert_eq!(failure.kind.class(), C::InvalidRequest);
        assert_eq!(failure.kind.class().as_str(), "invalid_request");
    }
}
