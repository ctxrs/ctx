//! Diagnostic computation belongs to execution; the model contains only its data.
use crate::errors::{AdapterError, protocol_error};
use ctx_attribution_model::*;
use ctx_history_snapshot_reader::SnapshotError;

pub fn import_all() -> BlameNextAction {
    BlameNextAction {
        kind: BlameNextActionKind::ImportAll,
        argv: ["ctx", "import", "--all"].map(str::to_owned).to_vec(),
    }
}

fn diagnostic(
    code: &'static str,
    reason: BlameDiagnosticReason,
    message: &'static str,
    next_action: Option<BlameNextAction>,
) -> BlameDiagnostic {
    BlameDiagnostic {
        error: code,
        error_code: code,
        reason,
        message,
        retryable: false,
        freshness: None,
        next_action,
        candidates: Vec::new(),
        candidates_truncated: false,
    }
}

pub(crate) fn projection_diagnostic(
    currentness: CoreProjectionCurrentness,
) -> Option<BlameDiagnostic> {
    use BlameDiagnosticReason::*;
    let (code, reason, message) = match currentness {
        CoreProjectionCurrentness::Current => return None,
        CoreProjectionCurrentness::NotMaterialized => (
            "not_materialized",
            ProjectionAbsent,
            "Blame data has not been indexed yet.",
        ),
        CoreProjectionCurrentness::Partial => (
            "partial",
            ProjectionPartial,
            "Blame indexing is incomplete.",
        ),
        CoreProjectionCurrentness::Stale => (
            "stale_source",
            ProjectionStale,
            "Blame data is behind the current history.",
        ),
        CoreProjectionCurrentness::NeedsRebuild => (
            "needs_rebuild",
            ProjectionIncompatible,
            "Blame data must be rebuilt for this version.",
        ),
    };
    Some(diagnostic(code, reason, message, Some(import_all())))
}

pub(crate) fn from_adapter_error(error: &AdapterError) -> BlameDiagnostic {
    from_protocol_error(&protocol_error(error))
}

pub(crate) fn from_protocol_error(error: &ProtocolError) -> BlameDiagnostic {
    use BlameDiagnosticReason as R;
    use ErrorClass as E;
    let (code, reason, message) = match error.class {
        E::NotMaterialized => (
            "not_materialized",
            R::ProjectionAbsent,
            "Blame data has not been indexed yet.",
        ),
        E::ProtocolMismatch | E::RebuildRequired => (
            "protocol_mismatch",
            R::ProjectionIncompatible,
            "The attribution index is incompatible with this ctx version.",
        ),
        E::MissingSource => (
            "source_unavailable",
            R::SourceUnavailable,
            "The source data required for this blame request is unavailable.",
        ),
        E::MissingRepository => (
            "repository_unavailable",
            R::RepositoryNotBound,
            "The repository required for this blame request is unavailable.",
        ),
        E::ResourceNotFound => (
            "resource_not_found",
            R::TargetNotIndexed,
            "No indexed agent evidence matches this blame target.",
        ),
        E::StaleFact => (
            "stale_fact",
            R::EvidenceStale,
            "The indexed evidence or continuation is stale. Restart this blame query.",
        ),
        E::LineOutOfRange => (
            "line_out_of_range",
            R::LineOutOfRange,
            "The requested line range is outside the committed file.",
        ),
        E::StaleSnapshot => (
            "stale_snapshot",
            R::RepositoryChanged,
            "The repository changed while ctx was evaluating blame.",
        ),
        E::Ambiguous => (
            "ambiguous",
            R::TargetOrRepositoryAmbiguous,
            "The blame target or repository selector is ambiguous.",
        ),
        E::OperationUnavailable => (
            "operation_unavailable",
            R::OperationNotCovered,
            "This attribution index does not cover the requested blame operation.",
        ),
        E::Corrupt => (
            "corrupt_graph",
            R::GraphCorrupt,
            "The attribution index cannot be read safely.",
        ),
        E::InvalidRequest => (
            "invalid_request",
            R::RequestInvalid,
            "The blame request or continuation is invalid. Repeat the query without --cursor.",
        ),
        E::Bounds => (
            "invalid_request",
            R::InvalidBounds,
            "The blame request exceeds a supported bound.",
        ),
        E::Sequence | E::Internal => (
            "invalid_response",
            R::GraphCorrupt,
            "The attribution result failed validation.",
        ),
    };
    let repair = matches!(
        error.class,
        E::NotMaterialized | E::ProtocolMismatch | E::RebuildRequired | E::Corrupt
    );
    let mut result = diagnostic(code, reason, message, repair.then(import_all));
    result.retryable = error.retryable;
    // Only canonical validated details can affect reasons/candidates; free-form errors stay internal.
    if error.validate().is_ok()
        && let Some(details) = &error.details
    {
        result.reason = details.reason;
        result.message = match details.reason {
            R::RepositorySelectorNotIndexed => {
                "No indexed repository matches the requested selector."
            }
            R::RepositoryNotBound => "The blame target is not bound to a repository.",
            R::CheckoutUnavailable => {
                "The repository checkout required for this blame request is unavailable."
            }
            R::GitUnavailable => "Git is unavailable for this blame request.",
            R::RepositoryAmbiguous => "More than one repository matches this blame target.",
            R::TargetAmbiguous => "More than one target matches this blame request.",
            R::CommitRewriteAmbiguous => {
                "More than one surviving commit matches the requested rewritten commit."
            }
            R::FileBlameNotCovered => "This attribution index does not cover file blame.",
            R::CommitBlameNotCovered => "This attribution index does not cover commit blame.",
            R::PullRequestBlameNotCovered => {
                "This attribution index does not cover pull request blame."
            }
            _ => message,
        };
        result.candidates = details.candidates.clone();
        result.candidates_truncated = details.candidates_truncated;
    }
    result
}

pub(crate) fn from_snapshot_error(error: &SnapshotError) -> BlameDiagnostic {
    use BlameDiagnosticReason as R;
    match error {
        SnapshotError::SchemaMismatch { .. } | SnapshotError::FingerprintMismatch { .. } => {
            diagnostic(
                "protocol_mismatch",
                R::ProjectionIncompatible,
                "The retained Core snapshot is incompatible with this ctx version.",
                Some(import_all()),
            )
        }
        SnapshotError::Corrupt(_) => diagnostic(
            "corrupt_core",
            R::GraphCorrupt,
            "The retained Core snapshot cannot be read safely.",
            None,
        ),
        SnapshotError::Bounds(_) => diagnostic(
            "bounds",
            R::InvalidBounds,
            "The retained Core snapshot exceeds a supported bound.",
            None,
        ),
        SnapshotError::UnsafePath(_) => diagnostic(
            "unsafe_path",
            R::RequestInvalid,
            "The retained Core snapshot path failed safety validation.",
            None,
        ),
        SnapshotError::NotFound(_) => diagnostic(
            "source_unavailable",
            R::SourceUnavailable,
            "The retained history required for this blame request is unavailable.",
            Some(import_all()),
        ),
        SnapshotError::LeaseConflict(_) | SnapshotError::ConcurrentGenerationChange(_) => {
            let mut result = diagnostic(
                "source_busy",
                R::SourceUnavailable,
                "The retained Core snapshot is temporarily busy.",
                None,
            );
            result.retryable = true;
            result
        }
    }
}

pub(crate) fn with_search_action(
    mut diagnostic: BlameDiagnostic,
    target: &BlameTarget,
) -> BlameDiagnostic {
    if matches!(
        diagnostic.reason,
        BlameDiagnosticReason::TargetNotIndexed
            | BlameDiagnosticReason::OperationNotCovered
            | BlameDiagnosticReason::FileBlameNotCovered
            | BlameDiagnosticReason::CommitBlameNotCovered
            | BlameDiagnosticReason::PullRequestBlameNotCovered
    ) {
        diagnostic.next_action = core_search_for(target);
    }
    diagnostic
}

pub(crate) fn core_search_for(target: &BlameTarget) -> Option<BlameNextAction> {
    let term = safe_core_search_term(target)?;
    Some(BlameNextAction {
        kind: BlameNextActionKind::SearchCore,
        argv: vec![
            "ctx".into(),
            "search".into(),
            term.to_owned(),
            "--refresh".into(),
            "off".into(),
        ],
    })
}

pub(crate) fn core_search_for_resolved(target: &ResolvedBlameTarget) -> Option<BlameNextAction> {
    let target = match target {
        ResolvedBlameTarget::File {
            path,
            repository,
            requested_lines,
        } => BlameTarget::File {
            path: path.clone(),
            repository: Some(repository.display.clone()),
            lines: requested_lines.clone(),
        },
        ResolvedBlameTarget::Commit { commit, repository } => BlameTarget::Commit {
            oid: commit.display.clone(),
            repository: Some(repository.display.clone()),
        },
        ResolvedBlameTarget::PullRequest {
            selector,
            repository,
            ..
        } => BlameTarget::PullRequest {
            selector: selector.clone(),
            repository: Some(repository.display.clone()),
        },
    };
    core_search_for(&target)
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
