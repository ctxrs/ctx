use std::{error::Error, fmt};

use ctx_pro_host_protocol::{
    BlameDiagnosticCandidate as ProtocolCandidate, BlameDiagnosticReason as ProtocolReason,
    ErrorClass, ProtocolError,
};
use serde::Serialize;

pub(crate) const MAX_BLAME_DIAGNOSTIC_CANDIDATES: usize = 5;
const MAX_BLAME_DIAGNOSTIC_CANDIDATE_BYTES: usize = 160;

pub(crate) const RESOURCE_NOT_FOUND_DIAGNOSTIC: &str =
    "No indexed Pro resource matches the requested blame target.";

/// A trusted public blame failure.
///
/// `Display` intentionally remains the stable error code so existing callers
/// keep their string contract. Structured consumers should serialize this type
/// and use `message` for prose. No helper-authored message or error source is
/// retained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BlameDiagnostic {
    pub(crate) error: &'static str,
    pub(crate) error_code: &'static str,
    pub(crate) reason: BlameDiagnosticReason,
    pub(crate) message: &'static str,
    pub(crate) retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) freshness: Option<BlameDiagnosticFreshness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_action: Option<BlameNextAction>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) candidates: Vec<BlameDiagnosticCandidate>,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) candidates_truncated: bool,
}

impl fmt::Display for BlameDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.error_code)
    }
}

impl Error for BlameDiagnostic {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BlameDiagnosticReason {
    ProNotInstalled,
    EntitlementRequired,
    EntitlementExpired,
    EntitlementInvalid,
    SecureStorageUnavailable,
    SecureStorageLocked,
    ProjectionAbsent,
    ProjectionPartial,
    ProjectionStale,
    ProjectionIncompatible,
    SourceUnavailable,
    RepositoryNotBound,
    CheckoutUnavailable,
    GitUnavailable,
    TargetNotIndexed,
    RepositorySelectorNotIndexed,
    OperationNotCovered,
    FileBlameNotCovered,
    CommitBlameNotCovered,
    PullRequestBlameNotCovered,
    EvidenceStale,
    LineOutOfRange,
    RepositoryChanged,
    TargetOrRepositoryAmbiguous,
    TargetAmbiguous,
    RepositoryAmbiguous,
    CommitRewriteAmbiguous,
    GraphCorrupt,
    RequestInvalid,
    InvalidTarget,
    InvalidRepositorySelector,
    InvalidCursor,
    InvalidBounds,
    CatchingUp,
    GenerationChanged,
    HelperIncompatible,
    HelperResponseInvalid,
    HelperTimedOut,
    HelperFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BlameDiagnosticFreshness {
    pub(crate) state: BlameFreshnessState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) served_generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) active_generation: Option<String>,
    pub(crate) catch_up_active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // The protocol-details seam does not populate freshness on this base.
pub(crate) enum BlameFreshnessState {
    Current,
    CatchingUp,
    StaleCommitted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BlameNextAction {
    pub(crate) kind: BlameNextActionKind,
    pub(crate) argv: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // Request-specific actions are populated by the integration/output lane.
pub(crate) enum BlameNextActionKind {
    SetupPro,
    ManagePro,
    RepairPro,
    CheckStatus,
    Retry,
    SearchCore,
    SpecifyRepository,
    SelectCommit,
    RetryFromCheckout,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum BlameDiagnosticCandidate {
    Repository { selector: String },
    Commit { repository: String, oid: String },
}

#[derive(Default)]
struct ProtocolDiagnosticDetails {
    reason: Option<BlameDiagnosticReason>,
    message: Option<&'static str>,
    freshness: Option<BlameDiagnosticFreshness>,
    next_action: Option<(BlameNextActionKind, &'static [&'static str])>,
    candidates: Vec<BlameDiagnosticCandidate>,
    candidates_truncated: bool,
}

#[derive(Clone, Copy)]
struct DiagnosticMapping {
    error_code: &'static str,
    reason: BlameDiagnosticReason,
    message: &'static str,
    next_action: Option<(BlameNextActionKind, &'static [&'static str])>,
}

impl BlameDiagnostic {
    pub(crate) fn from_protocol_error(error: ProtocolError) -> Self {
        let details = protocol_diagnostic_details(&error);
        let mapping = protocol_class_mapping(error.class);
        Self::from_mapping(mapping, error.retryable, details)
    }

    pub(crate) fn for_stable_error_code(code: &'static str) -> Option<Self> {
        let mapping = stable_code_mapping(code)?;
        Some(Self::from_mapping(
            mapping,
            legacy_retryable(code),
            ProtocolDiagnosticDetails::default(),
        ))
    }

    fn from_mapping(
        mapping: DiagnosticMapping,
        retryable: bool,
        details: ProtocolDiagnosticDetails,
    ) -> Self {
        let (candidates, candidates_truncated) =
            sanitize_candidates(details.candidates, details.candidates_truncated);
        Self {
            error: mapping.error_code,
            error_code: mapping.error_code,
            reason: details.reason.unwrap_or(mapping.reason),
            message: details.message.unwrap_or(mapping.message),
            retryable,
            freshness: details.freshness.map(sanitize_freshness),
            next_action: details
                .next_action
                .or(mapping.next_action)
                .map(|(kind, argv)| BlameNextAction::trusted(kind, argv)),
            candidates,
            candidates_truncated,
        }
    }
}

impl BlameNextAction {
    fn trusted(kind: BlameNextActionKind, argv: &[&str]) -> Self {
        Self {
            kind,
            argv: argv.iter().map(|value| (*value).to_owned()).collect(),
        }
    }
}

/// Maps only protocol-validated enums and candidate identities. The helper's
/// free-form message is intentionally ignored.
fn protocol_diagnostic_details(error: &ProtocolError) -> ProtocolDiagnosticDetails {
    let Some(details) = &error.details else {
        return ProtocolDiagnosticDetails::default();
    };
    let (reason, message, next_action) = match details.reason {
        ProtocolReason::TargetNotIndexed => (
            BlameDiagnosticReason::TargetNotIndexed,
            RESOURCE_NOT_FOUND_DIAGNOSTIC,
            None,
        ),
        ProtocolReason::RepositorySelectorNotIndexed => (
            BlameDiagnosticReason::RepositorySelectorNotIndexed,
            "No indexed Pro repository matches the requested selector.",
            Some((
                BlameNextActionKind::SpecifyRepository,
                &["ctx", "blame"] as &[_],
            )),
        ),
        ProtocolReason::RepositoryNotBound => (
            BlameDiagnosticReason::RepositoryNotBound,
            "The blame target is not bound to a repository.",
            Some((
                BlameNextActionKind::SpecifyRepository,
                &["ctx", "blame"] as &[_],
            )),
        ),
        ProtocolReason::CheckoutUnavailable => (
            BlameDiagnosticReason::CheckoutUnavailable,
            "The repository checkout required for this blame request is unavailable.",
            Some((
                BlameNextActionKind::RetryFromCheckout,
                &["ctx", "blame"] as &[_],
            )),
        ),
        ProtocolReason::GitUnavailable => (
            BlameDiagnosticReason::GitUnavailable,
            "Git is unavailable for this blame request.",
            None,
        ),
        ProtocolReason::RepositoryAmbiguous => (
            BlameDiagnosticReason::RepositoryAmbiguous,
            "More than one repository matches this blame target.",
            Some((
                BlameNextActionKind::SpecifyRepository,
                &["ctx", "blame"] as &[_],
            )),
        ),
        ProtocolReason::TargetAmbiguous => (
            BlameDiagnosticReason::TargetAmbiguous,
            "More than one target matches this blame request.",
            Some((BlameNextActionKind::SelectCommit, &["ctx", "blame"] as &[_])),
        ),
        ProtocolReason::CommitRewriteAmbiguous => (
            BlameDiagnosticReason::CommitRewriteAmbiguous,
            "More than one surviving commit matches the requested rewritten commit.",
            Some((BlameNextActionKind::SelectCommit, &["ctx", "blame"] as &[_])),
        ),
        ProtocolReason::FileBlameNotCovered => (
            BlameDiagnosticReason::FileBlameNotCovered,
            "This Pro graph does not cover file blame.",
            Some((
                BlameNextActionKind::CheckStatus,
                &["ctx", "pro", "status"] as &[_],
            )),
        ),
        ProtocolReason::CommitBlameNotCovered => (
            BlameDiagnosticReason::CommitBlameNotCovered,
            "This Pro graph does not cover commit blame.",
            Some((
                BlameNextActionKind::CheckStatus,
                &["ctx", "pro", "status"] as &[_],
            )),
        ),
        ProtocolReason::PullRequestBlameNotCovered => (
            BlameDiagnosticReason::PullRequestBlameNotCovered,
            "This Pro graph does not cover pull request blame.",
            Some((
                BlameNextActionKind::CheckStatus,
                &["ctx", "pro", "status"] as &[_],
            )),
        ),
    };
    ProtocolDiagnosticDetails {
        reason: Some(reason),
        message: Some(message),
        next_action,
        candidates: details
            .candidates
            .iter()
            .map(|candidate| match candidate {
                ProtocolCandidate::Repository { selector } => {
                    BlameDiagnosticCandidate::Repository {
                        selector: selector.clone(),
                    }
                }
                ProtocolCandidate::Commit { repository, oid } => BlameDiagnosticCandidate::Commit {
                    repository: repository.clone(),
                    oid: oid.clone(),
                },
            })
            .collect(),
        candidates_truncated: details.candidates_truncated,
        ..ProtocolDiagnosticDetails::default()
    }
}

fn protocol_class_mapping(class: ErrorClass) -> DiagnosticMapping {
    match class {
        ErrorClass::EntitlementExpired => mapping(
            "entitlement_expired",
            BlameDiagnosticReason::EntitlementExpired,
            "ctx Pro access has expired.",
            manage_pro_action(),
        ),
        ErrorClass::KeyStoreUnavailable => mapping(
            "key_store_unavailable",
            BlameDiagnosticReason::SecureStorageUnavailable,
            "The secure key store required by ctx Pro is unavailable.",
            setup_pro_action(),
        ),
        ErrorClass::KeyStoreLocked => mapping(
            "key_store_locked",
            BlameDiagnosticReason::SecureStorageLocked,
            "The secure key store required by ctx Pro is locked.",
            setup_pro_action(),
        ),
        ErrorClass::NotMaterialized => mapping(
            "not_materialized",
            BlameDiagnosticReason::ProjectionAbsent,
            "ctx Pro blame data is not materialized.",
            repair_pro_action(),
        ),
        ErrorClass::ProtocolMismatch => mapping(
            "protocol_mismatch",
            BlameDiagnosticReason::HelperIncompatible,
            "The installed ctx Pro helper is incompatible with this ctx version.",
            repair_pro_action(),
        ),
        ErrorClass::MissingSource => mapping(
            "source_unavailable",
            BlameDiagnosticReason::SourceUnavailable,
            "The source data required for this blame request is unavailable.",
            None,
        ),
        ErrorClass::MissingRepository => mapping(
            "repository_unavailable",
            BlameDiagnosticReason::RepositoryNotBound,
            "The repository required for this blame request is unavailable.",
            None,
        ),
        ErrorClass::ResourceNotFound => mapping(
            "resource_not_found",
            BlameDiagnosticReason::TargetNotIndexed,
            RESOURCE_NOT_FOUND_DIAGNOSTIC,
            None,
        ),
        ErrorClass::StaleFact => mapping(
            "stale_fact",
            BlameDiagnosticReason::EvidenceStale,
            "The indexed evidence for this blame request is stale.",
            None,
        ),
        ErrorClass::LineOutOfRange => mapping(
            "line_out_of_range",
            BlameDiagnosticReason::LineOutOfRange,
            "The requested line range is outside the committed file.",
            None,
        ),
        ErrorClass::StaleSnapshot => mapping(
            "stale_snapshot",
            BlameDiagnosticReason::RepositoryChanged,
            "The repository changed while ctx Pro was evaluating blame.",
            None,
        ),
        ErrorClass::Ambiguous => mapping(
            "ambiguous",
            BlameDiagnosticReason::TargetOrRepositoryAmbiguous,
            "The blame target or repository selector is ambiguous.",
            None,
        ),
        ErrorClass::OperationUnavailable => mapping(
            "operation_unavailable",
            BlameDiagnosticReason::OperationNotCovered,
            "This ctx Pro graph does not cover the requested blame operation.",
            Some((BlameNextActionKind::CheckStatus, &["ctx", "pro", "status"])),
        ),
        ErrorClass::Corrupt => mapping(
            "corrupt_graph",
            BlameDiagnosticReason::GraphCorrupt,
            "The ctx Pro graph cannot be read safely.",
            repair_pro_action(),
        ),
        ErrorClass::InvalidRequest => mapping(
            "invalid_request",
            BlameDiagnosticReason::RequestInvalid,
            "The blame request is invalid.",
            None,
        ),
        ErrorClass::Bounds => mapping(
            "invalid_request",
            BlameDiagnosticReason::InvalidBounds,
            "The blame request exceeds a supported bound.",
            None,
        ),
        ErrorClass::RebuildRequired => mapping(
            "needs_rebuild",
            BlameDiagnosticReason::ProjectionIncompatible,
            "The ctx Pro graph must be rebuilt before blame can continue.",
            repair_pro_action(),
        ),
        ErrorClass::Sequence => mapping(
            "invalid_response",
            BlameDiagnosticReason::HelperResponseInvalid,
            "The ctx Pro helper returned an invalid response.",
            repair_pro_action(),
        ),
        ErrorClass::Internal => mapping(
            "helper_crashed",
            BlameDiagnosticReason::HelperFailed,
            "The ctx Pro helper could not complete the blame request.",
            None,
        ),
    }
}

fn stable_code_mapping(code: &'static str) -> Option<DiagnosticMapping> {
    let value = match code {
        "pro_not_installed" => mapping(
            code,
            BlameDiagnosticReason::ProNotInstalled,
            "The signed ctx Pro helper is not installed.",
            setup_pro_action(),
        ),
        "entitlement_required" => mapping(
            code,
            BlameDiagnosticReason::EntitlementRequired,
            "ctx Pro requires an active entitlement.",
            setup_pro_action(),
        ),
        "entitlement_expired" => mapping(
            code,
            BlameDiagnosticReason::EntitlementExpired,
            "ctx Pro access has expired.",
            manage_pro_action(),
        ),
        "entitlement_invalid" => mapping(
            code,
            BlameDiagnosticReason::EntitlementInvalid,
            "The stored ctx Pro entitlement is invalid.",
            manage_pro_action(),
        ),
        "key_store_unavailable" => mapping(
            code,
            BlameDiagnosticReason::SecureStorageUnavailable,
            "The secure key store required by ctx Pro is unavailable.",
            setup_pro_action(),
        ),
        "key_store_locked" => mapping(
            code,
            BlameDiagnosticReason::SecureStorageLocked,
            "The secure key store required by ctx Pro is locked.",
            setup_pro_action(),
        ),
        "not_materialized" => mapping(
            code,
            BlameDiagnosticReason::ProjectionAbsent,
            "ctx Pro blame data is not materialized.",
            repair_pro_action(),
        ),
        "partial" | "needs_resume" => mapping(
            code,
            BlameDiagnosticReason::ProjectionPartial,
            "ctx Pro blame data is only partially materialized.",
            repair_pro_action(),
        ),
        "stale_source" => mapping(
            code,
            BlameDiagnosticReason::ProjectionStale,
            "ctx Pro blame data is not current.",
            Some((BlameNextActionKind::CheckStatus, &["ctx", "pro", "status"])),
        ),
        "needs_rebuild" => mapping(
            code,
            BlameDiagnosticReason::ProjectionIncompatible,
            "The ctx Pro graph must be rebuilt before blame can continue.",
            repair_pro_action(),
        ),
        "source_unavailable" => mapping(
            code,
            BlameDiagnosticReason::SourceUnavailable,
            "The source data required for this blame request is unavailable.",
            None,
        ),
        "repository_unavailable" => mapping(
            code,
            BlameDiagnosticReason::RepositoryNotBound,
            "The repository required for this blame request is unavailable.",
            None,
        ),
        "resource_not_found" => mapping(
            code,
            BlameDiagnosticReason::TargetNotIndexed,
            RESOURCE_NOT_FOUND_DIAGNOSTIC,
            None,
        ),
        "operation_unavailable" => mapping(
            code,
            BlameDiagnosticReason::OperationNotCovered,
            "This ctx Pro installation does not currently cover the requested blame operation.",
            Some((BlameNextActionKind::CheckStatus, &["ctx", "pro", "status"])),
        ),
        "stale_fact" => mapping(
            code,
            BlameDiagnosticReason::EvidenceStale,
            "The indexed evidence for this blame request is stale.",
            None,
        ),
        "line_out_of_range" => mapping(
            code,
            BlameDiagnosticReason::LineOutOfRange,
            "The requested line range is outside the committed file.",
            None,
        ),
        "stale_snapshot" => mapping(
            code,
            BlameDiagnosticReason::RepositoryChanged,
            "The repository changed while ctx Pro was evaluating blame.",
            None,
        ),
        "ambiguous" => mapping(
            code,
            BlameDiagnosticReason::TargetOrRepositoryAmbiguous,
            "The blame target or repository selector is ambiguous.",
            None,
        ),
        "corrupt_graph" => mapping(
            code,
            BlameDiagnosticReason::GraphCorrupt,
            "The ctx Pro graph cannot be read safely.",
            repair_pro_action(),
        ),
        "invalid_request" => mapping(
            code,
            BlameDiagnosticReason::RequestInvalid,
            "The blame request is invalid.",
            None,
        ),
        "protocol_mismatch" | "helper_upgrade_required" => mapping(
            code,
            BlameDiagnosticReason::HelperIncompatible,
            "The installed ctx Pro helper is incompatible with this ctx version.",
            repair_pro_action(),
        ),
        "invalid_response" => mapping(
            code,
            BlameDiagnosticReason::HelperResponseInvalid,
            "The ctx Pro helper returned an invalid response.",
            repair_pro_action(),
        ),
        "helper_timeout" => mapping(
            code,
            BlameDiagnosticReason::HelperTimedOut,
            "The ctx Pro helper did not respond in time.",
            None,
        ),
        "helper_crashed" => mapping(
            code,
            BlameDiagnosticReason::HelperFailed,
            "The ctx Pro helper could not complete the blame request.",
            None,
        ),
        _ => return None,
    };
    Some(value)
}

const fn mapping(
    error_code: &'static str,
    reason: BlameDiagnosticReason,
    message: &'static str,
    next_action: Option<(BlameNextActionKind, &'static [&'static str])>,
) -> DiagnosticMapping {
    DiagnosticMapping {
        error_code,
        reason,
        message,
        next_action,
    }
}

const fn setup_pro_action() -> Option<(BlameNextActionKind, &'static [&'static str])> {
    Some((BlameNextActionKind::SetupPro, &["ctx", "pro"]))
}

const fn manage_pro_action() -> Option<(BlameNextActionKind, &'static [&'static str])> {
    Some((BlameNextActionKind::ManagePro, &["ctx", "pro", "manage"]))
}

const fn repair_pro_action() -> Option<(BlameNextActionKind, &'static [&'static str])> {
    Some((BlameNextActionKind::RepairPro, &["ctx", "pro"]))
}

fn legacy_retryable(code: &str) -> bool {
    matches!(code, "stale_source" | "stale_snapshot" | "helper_timeout")
}

fn sanitize_candidates(
    candidates: Vec<BlameDiagnosticCandidate>,
    already_truncated: bool,
) -> (Vec<BlameDiagnosticCandidate>, bool) {
    let mut sanitized = Vec::with_capacity(candidates.len());
    let mut truncated = already_truncated;
    for candidate in candidates {
        let Some(candidate) = sanitize_candidate(candidate) else {
            truncated = true;
            continue;
        };
        sanitized.push(candidate);
    }
    sanitized.sort();
    sanitized.dedup();
    if sanitized.len() > MAX_BLAME_DIAGNOSTIC_CANDIDATES {
        sanitized.truncate(MAX_BLAME_DIAGNOSTIC_CANDIDATES);
        truncated = true;
    }
    (sanitized, truncated)
}

fn sanitize_candidate(candidate: BlameDiagnosticCandidate) -> Option<BlameDiagnosticCandidate> {
    match candidate {
        BlameDiagnosticCandidate::Repository { selector } => {
            Some(BlameDiagnosticCandidate::Repository {
                selector: sanitize_repository(&selector)?,
            })
        }
        BlameDiagnosticCandidate::Commit { repository, oid } => {
            Some(BlameDiagnosticCandidate::Commit {
                repository: sanitize_repository(&repository)?,
                oid: sanitize_commit_oid(&oid)?,
            })
        }
    }
}

fn sanitize_repository(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_BLAME_DIAGNOSTIC_CANDIDATE_BYTES
        || value.chars().any(char::is_control)
        || looks_like_private_local_path(value)
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-._/:".contains(character))
    {
        return None;
    }
    Some(value.to_owned())
}

fn sanitize_commit_oid(value: &str) -> Option<String> {
    (matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
    .then(|| value.to_owned())
}

fn looks_like_private_local_path(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let bytes = value.as_bytes();
    value.starts_with(['/', '\\', '~'])
        || lower.starts_with("file:")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
        || value
            .split(['/', '\\'])
            .any(|segment| segment == "." || segment == "..")
        || lower.contains("/home/")
        || lower.contains("/users/")
        || lower.contains("\\users\\")
}

fn sanitize_freshness(mut freshness: BlameDiagnosticFreshness) -> BlameDiagnosticFreshness {
    freshness.served_generation = freshness
        .served_generation
        .as_deref()
        .and_then(sanitize_generation_id);
    freshness.active_generation = freshness
        .active_generation
        .as_deref()
        .and_then(sanitize_generation_id);
    freshness
}

fn sanitize_generation_id(value: &str) -> Option<String> {
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

const fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_shape_keeps_error_alias_and_message_separate() {
        let diagnostic = BlameDiagnostic::for_stable_error_code("operation_unavailable").unwrap();
        let value = serde_json::to_value(&diagnostic).unwrap();
        assert_eq!(value["error"], "operation_unavailable");
        assert_eq!(value["error_code"], value["error"]);
        assert_eq!(value["reason"], "operation_not_covered");
        assert_eq!(value["retryable"], false);
        assert_eq!(value["next_action"]["kind"], "check_status");
        assert_eq!(
            value["next_action"]["argv"],
            serde_json::json!(["ctx", "pro", "status"])
        );
        assert_ne!(value["message"], value["error"]);
    }

    #[test]
    fn protocol_retryability_is_preserved_exactly() {
        for retryable in [false, true] {
            let mut error = ProtocolError::new(ErrorClass::Internal, "ignored helper message");
            error.retryable = retryable;
            let diagnostic = BlameDiagnostic::from_protocol_error(error);
            assert_eq!(diagnostic.retryable, retryable);
            assert_eq!(diagnostic.to_string(), "helper_crashed");
        }
    }

    #[test]
    fn candidates_are_sanitized_deduplicated_sorted_and_bounded() {
        let candidates = vec![
            repository("z/repo"),
            repository("/home/alice/private"),
            repository("e/repo"),
            repository("d/repo"),
            repository("c/repo"),
            repository("b/repo"),
            repository("a/repo"),
            repository("a/repo"),
            repository("token=malicious"),
            BlameDiagnosticCandidate::Commit {
                repository: "safe/repo".to_owned(),
                oid: "a".repeat(40),
            },
        ];
        let details = ProtocolDiagnosticDetails {
            candidates,
            ..ProtocolDiagnosticDetails::default()
        };
        let diagnostic = BlameDiagnostic::from_mapping(
            protocol_class_mapping(ErrorClass::Ambiguous),
            false,
            details,
        );
        assert_eq!(diagnostic.candidates.len(), MAX_BLAME_DIAGNOSTIC_CANDIDATES);
        assert!(diagnostic.candidates_truncated);
        assert!(diagnostic
            .candidates
            .iter()
            .all(|candidate| !format!("{candidate:?}").contains("alice")));
        assert!(diagnostic
            .candidates
            .iter()
            .all(|candidate| !format!("{candidate:?}").contains("malicious")));
        assert!(diagnostic
            .candidates
            .windows(2)
            .all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn freshness_drops_unvalidated_generation_values() {
        let details = ProtocolDiagnosticDetails {
            freshness: Some(BlameDiagnosticFreshness {
                state: BlameFreshnessState::StaleCommitted,
                served_generation: Some("/secret/graph/generation".to_owned()),
                active_generation: Some("A".repeat(64)),
                catch_up_active: true,
            }),
            ..ProtocolDiagnosticDetails::default()
        };
        let diagnostic = BlameDiagnostic::from_mapping(
            protocol_class_mapping(ErrorClass::ResourceNotFound),
            true,
            details,
        );
        let freshness = diagnostic.freshness.unwrap();
        assert_eq!(freshness.served_generation, None);
        assert_eq!(freshness.active_generation, Some("a".repeat(64)));
        assert!(freshness.catch_up_active);
    }

    fn repository(selector: &str) -> BlameDiagnosticCandidate {
        BlameDiagnosticCandidate::Repository {
            selector: selector.to_owned(),
        }
    }
}
