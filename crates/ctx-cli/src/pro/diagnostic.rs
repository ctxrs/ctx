use std::{error::Error, fmt};

use ctx_pro_host_protocol::{
    BlameDiagnosticCandidate as ProtocolCandidate, BlameDiagnosticReason as ProtocolReason,
    BlameTarget, ErrorClass, ProtocolError, MAX_BLAME_TARGET_BYTES,
};
#[cfg(test)]
use ctx_pro_host_protocol::{
    BlameDiagnosticDetails as ProtocolDetails, MAX_BLAME_DIAGNOSTIC_CANDIDATES,
};
use serde::Serialize;

const CHECK_STATUS_ARGV: &[&str] = &["ctx", "status"];

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
    #[serde(skip)]
    blame_details_valid: bool,
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
    InvalidBounds,
    HelperIncompatible,
    HelperResponseInvalid,
    HelperTimedOut,
    HelperFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BlameDiagnosticFreshness {
    pub(crate) state: BlameFreshnessState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BlameFreshnessState {
    StaleCommitted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BlameNextAction {
    pub(crate) kind: BlameNextActionKind,
    pub(crate) argv: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BlameNextActionKind {
    SetupPro,
    ManagePro,
    RepairPro,
    CheckStatus,
    SearchCore,
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
        if error.validate().is_err() {
            return Self::invalid_response();
        }
        let blame_details_valid = error.validate_blame_details().is_ok();
        let details = protocol_diagnostic_details(&error);
        let mapping = protocol_class_mapping(error.class);
        let mut diagnostic = Self::from_mapping(mapping, error.retryable, details);
        diagnostic.blame_details_valid = blame_details_valid;
        diagnostic
    }

    pub(crate) fn for_stable_error_code(code: &'static str) -> Option<Self> {
        let mapping = stable_code_mapping(code)?;
        Some(Self::from_mapping(
            mapping,
            legacy_retryable(code),
            ProtocolDiagnosticDetails::default(),
        ))
    }

    #[must_use]
    pub(crate) fn with_stale_committed(mut self) -> Self {
        self.freshness = Some(BlameDiagnosticFreshness {
            state: BlameFreshnessState::StaleCommitted,
        });
        self
    }

    pub(crate) fn with_core_search_for(mut self, target: &BlameTarget) -> Self {
        if !self.blame_details_valid {
            return Self::invalid_response();
        }
        if matches!(
            self.reason,
            BlameDiagnosticReason::TargetNotIndexed | BlameDiagnosticReason::OperationNotCovered
        ) {
            self.next_action = BlameNextAction::core_search_for(target);
        }
        self
    }

    fn from_mapping(
        mapping: DiagnosticMapping,
        retryable: bool,
        details: ProtocolDiagnosticDetails,
    ) -> Self {
        Self {
            error: mapping.error_code,
            error_code: mapping.error_code,
            reason: details.reason.unwrap_or(mapping.reason),
            message: details.message.unwrap_or(mapping.message),
            retryable,
            freshness: details.freshness,
            next_action: details
                .next_action
                .or(mapping.next_action)
                .map(|(kind, argv)| BlameNextAction::trusted(kind, argv)),
            candidates: details.candidates,
            candidates_truncated: details.candidates_truncated,
            blame_details_valid: true,
        }
    }

    fn invalid_response() -> Self {
        Self::from_mapping(
            protocol_class_mapping(ErrorClass::Sequence),
            false,
            ProtocolDiagnosticDetails::default(),
        )
    }
}

impl BlameNextAction {
    pub(crate) fn check_status() -> Self {
        Self {
            kind: BlameNextActionKind::CheckStatus,
            argv: CHECK_STATUS_ARGV
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        }
    }

    pub(crate) fn core_search_for(target: &BlameTarget) -> Option<Self> {
        let term = safe_core_search_term(target)?;
        Some(Self {
            kind: BlameNextActionKind::SearchCore,
            argv: vec![
                "ctx".to_owned(),
                "search".to_owned(),
                term.to_owned(),
                "--refresh".to_owned(),
                "off".to_owned(),
            ],
        })
    }

    pub(crate) fn core_search_for_resolved(
        target: &ctx_pro_host_protocol::ResolvedBlameTarget,
    ) -> Option<Self> {
        let target = match target {
            ctx_pro_host_protocol::ResolvedBlameTarget::File {
                path,
                repository,
                requested_lines,
            } => BlameTarget::File {
                path: path.clone(),
                repository: Some(repository.display.clone()),
                lines: requested_lines.clone(),
            },
            ctx_pro_host_protocol::ResolvedBlameTarget::Commit { commit, repository } => {
                BlameTarget::Commit {
                    oid: commit.display.clone(),
                    repository: Some(repository.display.clone()),
                }
            }
            ctx_pro_host_protocol::ResolvedBlameTarget::PullRequest {
                selector,
                repository,
                ..
            } => BlameTarget::PullRequest {
                selector: selector.clone(),
                repository: Some(repository.display.clone()),
            },
        };
        Self::core_search_for(&target)
    }

    fn trusted(kind: BlameNextActionKind, argv: &[&str]) -> Self {
        if kind == BlameNextActionKind::CheckStatus {
            return Self::check_status();
        }
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
            None,
        ),
        ProtocolReason::RepositoryNotBound => (
            BlameDiagnosticReason::RepositoryNotBound,
            "The blame target is not bound to a repository.",
            None,
        ),
        ProtocolReason::CheckoutUnavailable => (
            BlameDiagnosticReason::CheckoutUnavailable,
            "The repository checkout required for this blame request is unavailable.",
            None,
        ),
        ProtocolReason::GitUnavailable => (
            BlameDiagnosticReason::GitUnavailable,
            "Git is unavailable for this blame request.",
            None,
        ),
        ProtocolReason::RepositoryAmbiguous => (
            BlameDiagnosticReason::RepositoryAmbiguous,
            "More than one repository matches this blame target.",
            None,
        ),
        ProtocolReason::TargetAmbiguous => (
            BlameDiagnosticReason::TargetAmbiguous,
            "More than one target matches this blame request.",
            None,
        ),
        ProtocolReason::CommitRewriteAmbiguous => (
            BlameDiagnosticReason::CommitRewriteAmbiguous,
            "More than one surviving commit matches the requested rewritten commit.",
            None,
        ),
        ProtocolReason::FileBlameNotCovered => (
            BlameDiagnosticReason::FileBlameNotCovered,
            "This Pro graph does not cover file blame.",
            Some((BlameNextActionKind::CheckStatus, CHECK_STATUS_ARGV)),
        ),
        ProtocolReason::CommitBlameNotCovered => (
            BlameDiagnosticReason::CommitBlameNotCovered,
            "This Pro graph does not cover commit blame.",
            Some((BlameNextActionKind::CheckStatus, CHECK_STATUS_ARGV)),
        ),
        ProtocolReason::PullRequestBlameNotCovered => (
            BlameDiagnosticReason::PullRequestBlameNotCovered,
            "This Pro graph does not cover pull request blame.",
            Some((BlameNextActionKind::CheckStatus, CHECK_STATUS_ARGV)),
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
            Some((BlameNextActionKind::CheckStatus, CHECK_STATUS_ARGV)),
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
            Some((BlameNextActionKind::CheckStatus, CHECK_STATUS_ARGV)),
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
            Some((BlameNextActionKind::CheckStatus, CHECK_STATUS_ARGV)),
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

fn safe_core_search_term(target: &BlameTarget) -> Option<&str> {
    match target {
        BlameTarget::File { path, .. } if safe_repository_relative_path(path) => Some(path),
        BlameTarget::Commit { oid, .. }
            if (4..=64).contains(&oid.len())
                && oid.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Some(oid)
        }
        BlameTarget::PullRequest { selector, .. } if target.validate().is_ok() => Some(selector),
        _ => None,
    }
}

fn safe_repository_relative_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    let lowercase = path.to_ascii_lowercase();
    !path.is_empty()
        && path.len() <= MAX_BLAME_TARGET_BYTES
        && path.trim() == path
        && !path.starts_with(['/', '\\', '~'])
        && !lowercase.starts_with("$home/")
        && !lowercase
            .strip_prefix('$')
            .is_some_and(|remainder| remainder.starts_with("{home}/"))
        && !lowercase.starts_with("%userprofile%/")
        && !lowercase.starts_with("%homepath%/")
        && !lowercase.starts_with("%homedrive%")
        && !lowercase.starts_with("file:")
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !bytes.get(1).is_some_and(|byte| *byte == b':')
        && path
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
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
            serde_json::json!(["ctx", "status"])
        );
        assert_eq!(
            diagnostic.next_action,
            Some(BlameNextAction::check_status())
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
    fn validated_candidates_and_truncation_are_preserved_exactly() {
        let candidates = (0..MAX_BLAME_DIAGNOSTIC_CANDIDATES)
            .map(|index| protocol_repository(&format!("forge:github.com/acme/repo-{index}")))
            .collect::<Vec<_>>();
        let error = ProtocolError::new(ErrorClass::Ambiguous, "ignored helper message")
            .with_blame_details(protocol_details(
                ProtocolReason::RepositoryAmbiguous,
                candidates.clone(),
                true,
            ));
        let diagnostic = BlameDiagnostic::from_protocol_error(error)
            .with_core_search_for(&valid_commit_target());
        let expected = candidates
            .into_iter()
            .map(|candidate| match candidate {
                ProtocolCandidate::Repository { selector } => {
                    BlameDiagnosticCandidate::Repository { selector }
                }
                ProtocolCandidate::Commit { repository, oid } => {
                    BlameDiagnosticCandidate::Commit { repository, oid }
                }
            })
            .collect::<Vec<_>>();

        assert_eq!(diagnostic.error_code, "ambiguous");
        assert_eq!(
            diagnostic.reason,
            BlameDiagnosticReason::RepositoryAmbiguous
        );
        assert_eq!(diagnostic.candidates, expected);
        assert!(diagnostic.candidates_truncated);
    }

    #[test]
    fn invalid_typed_details_fail_closed_without_client_cleanup_or_helper_detail() {
        let safe = protocol_repository("forge:github.com/acme/repo");
        let malformed = [
            vec![
                protocol_repository("workspace:private-repository"),
                safe.clone(),
            ],
            vec![
                protocol_repository("forge:github.com/acme/repo"),
                protocol_repository("forge:github.com/acme/repo "),
            ],
            vec![safe.clone(), safe.clone()],
            vec![safe.clone()],
            vec![
                protocol_repository("forge:github.com/acme/repo?token=secret"),
                safe.clone(),
            ],
            (0..=MAX_BLAME_DIAGNOSTIC_CANDIDATES)
                .map(|index| protocol_repository(&format!("forge:github.com/acme/repo-{index}")))
                .collect(),
        ];

        for candidates in malformed {
            let error = ProtocolError::new(
                ErrorClass::Ambiguous,
                "helper failed at /home/alice/private: token=secret",
            )
            .with_retryable(true)
            .with_blame_details(protocol_details(
                ProtocolReason::RepositoryAmbiguous,
                candidates,
                false,
            ));
            let diagnostic = BlameDiagnostic::from_protocol_error(error)
                .with_core_search_for(&valid_commit_target());
            let serialized = serde_json::to_string(&diagnostic).unwrap();

            assert_eq!(diagnostic.error_code, "invalid_response");
            assert_eq!(
                diagnostic.reason,
                BlameDiagnosticReason::HelperResponseInvalid
            );
            assert!(!diagnostic.retryable);
            assert!(diagnostic.candidates.is_empty());
            assert!(!diagnostic.candidates_truncated);
            assert!(!serialized.contains("alice"));
            assert!(!serialized.contains("token=secret"));
        }
    }

    #[test]
    fn zero_candidate_ambiguity_is_preserved_as_safe_omission() {
        let error =
            ProtocolError::new(ErrorClass::Ambiguous, "ignored helper message").with_blame_details(
                protocol_details(ProtocolReason::RepositoryAmbiguous, Vec::new(), false),
            );
        let diagnostic = BlameDiagnostic::from_protocol_error(error)
            .with_core_search_for(&valid_commit_target());
        assert_eq!(diagnostic.error_code, "ambiguous");
        assert_eq!(
            diagnostic.reason,
            BlameDiagnosticReason::RepositoryAmbiguous
        );
        assert!(diagnostic.candidates.is_empty());
        assert!(!diagnostic.candidates_truncated);
    }

    #[test]
    fn stale_committed_requires_the_explicit_host_established_builder() {
        let diagnostic = BlameDiagnostic::for_stable_error_code("stale_source").unwrap();
        assert!(diagnostic.freshness.is_none());
        assert!(serde_json::to_value(&diagnostic)
            .unwrap()
            .get("freshness")
            .is_none());

        let established = diagnostic.with_stale_committed();
        let value = serde_json::to_value(&established).unwrap();
        assert_eq!(
            value["freshness"],
            serde_json::json!({"state": "stale_committed"})
        );
        assert!(value.get("served_generation").is_none());
        assert!(value.get("active_generation").is_none());
        assert!(value.get("catch_up_active").is_none());
    }

    #[test]
    fn core_search_action_accepts_only_target_specific_safe_terms() {
        for target in [
            BlameTarget::File {
                path: "src/my file.rs".to_owned(),
                repository: None,
                lines: None,
            },
            BlameTarget::Commit {
                oid: "aBcD".to_owned(),
                repository: None,
            },
            BlameTarget::Commit {
                oid: "a".repeat(64),
                repository: None,
            },
            BlameTarget::PullRequest {
                selector: "42".to_owned(),
                repository: Some("forge:github.com/ctxrs/ctx".to_owned()),
            },
            BlameTarget::PullRequest {
                selector: "https://github.com/ctxrs/ctx/pull/42".to_owned(),
                repository: None,
            },
        ] {
            let expected = match &target {
                BlameTarget::File { path, .. } => path,
                BlameTarget::Commit { oid, .. } => oid,
                BlameTarget::PullRequest { selector, .. } => selector,
            };
            assert_eq!(
                BlameNextAction::core_search_for(&target).unwrap().argv,
                vec!["ctx", "search", expected, "--refresh", "off"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn core_search_action_omits_malicious_or_malformed_target_terms() {
        let mut invalid_files = vec![
            "/home/alice/private.rs".to_owned(),
            "/Users/alice/private.rs".to_owned(),
            "C:/Users/alice/private.rs".to_owned(),
            r"C:\Users\alice\private.rs".to_owned(),
            "../private.rs".to_owned(),
            "src/../private.rs".to_owned(),
            "./src/lib.rs".to_owned(),
            "src//lib.rs".to_owned(),
            "src/".to_owned(),
            "~/.ssh/id_ed25519".to_owned(),
            "$HOME/.ssh/id_ed25519".to_owned(),
            concat!("$", "{HOME}/.ssh/id_ed25519").to_owned(),
            "%USERPROFILE%/secret".to_owned(),
            "%HOMEDRIVE%%HOMEPATH%/secret".to_owned(),
            "file:/home/alice/private.rs".to_owned(),
            " src/lib.rs".to_owned(),
            "src/lib.rs ".to_owned(),
            "src/\nsecret.rs".to_owned(),
        ];
        invalid_files.push("a".repeat(MAX_BLAME_TARGET_BYTES + 1));
        for path in invalid_files {
            let target = BlameTarget::File {
                path,
                repository: None,
                lines: None,
            };
            assert!(BlameNextAction::core_search_for(&target).is_none());
        }

        for oid in [
            "abc".to_owned(),
            "a".repeat(65),
            "abcdg".to_owned(),
            "abcd\n".to_owned(),
            "/home/alice/secret".to_owned(),
        ] {
            let target = BlameTarget::Commit {
                oid,
                repository: None,
            };
            assert!(BlameNextAction::core_search_for(&target).is_none());
        }

        for (selector, repository) in [
            ("42", None),
            ("0", Some("forge:github.com/ctxrs/ctx")),
            ("01", Some("forge:github.com/ctxrs/ctx")),
            ("https://GitHub.com/ctxrs/ctx/pull/42", None),
            ("https://github.com/ctxrs/ctx/pull/42?token=secret", None),
            ("https://github.com/ctxrs/ctx/pull/42#fragment", None),
            ("https://user:token@github.com/ctxrs/ctx/pull/42", None),
            (" https://github.com/ctxrs/ctx/pull/42", None),
            ("https://github.com/ctxrs/ctx/pull/42\n", None),
        ] {
            let target = BlameTarget::PullRequest {
                selector: selector.to_owned(),
                repository: repository.map(str::to_owned),
            };
            assert!(BlameNextAction::core_search_for(&target).is_none());
        }
    }

    fn protocol_details(
        reason: ProtocolReason,
        candidates: Vec<ProtocolCandidate>,
        candidates_truncated: bool,
    ) -> ProtocolDetails {
        ProtocolDetails {
            reason,
            candidates,
            candidates_truncated,
        }
    }

    fn protocol_repository(selector: &str) -> ProtocolCandidate {
        ProtocolCandidate::Repository {
            selector: selector.to_owned(),
        }
    }

    fn valid_commit_target() -> BlameTarget {
        BlameTarget::Commit {
            oid: "abcd".to_owned(),
            repository: None,
        }
    }
}
