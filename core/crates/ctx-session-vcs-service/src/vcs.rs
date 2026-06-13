use anyhow::Error;
use async_trait::async_trait;
use ctx_core::ids::{SessionId, WorktreeId};
use ctx_core::models::{DiffUnavailableReason, Session, SessionGitStatusSummary, Worktree};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionVcsDiffQuery {
    pub base_commit_sha: Option<String>,
    pub target_branch: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionVcsApplyAction {
    Accept,
    Reject,
}

impl SessionVcsApplyAction {
    fn reverse_patch(self) -> bool {
        matches!(self, Self::Reject)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionVcsDiff {
    pub diff: String,
    pub available: bool,
    pub unavailable_reason: Option<DiffUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionVcsDiffSummary {
    pub base_commit_sha: String,
    pub head_commit_sha: String,
    pub file_count: i64,
    pub line_additions: i64,
    pub line_deletions: i64,
    pub available: bool,
    pub unavailable_reason: Option<DiffUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionVcsGitStatus {
    pub raw: String,
    pub summary_line: String,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub detached: bool,
    pub staged: i64,
    pub unstaged: i64,
    pub untracked: i64,
    pub entries: Vec<SessionVcsGitStatusEntry>,
    pub entries_truncated: bool,
    pub entries_total_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionVcsGitStatusEntry {
    pub path: String,
    pub orig_path: Option<String>,
    pub index_status: String,
    pub worktree_status: String,
}

#[derive(Debug)]
pub enum SessionVcsError {
    NotFound,
    InvalidExplicitTarget(String),
    BadPatch(Error),
    Internal(Error),
}

#[derive(Debug, Clone)]
pub struct SessionVcsContext {
    pub session: Session,
    pub worktree: Worktree,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionVcsDiffBaseQuery {
    pub base_commit_sha: Option<String>,
    pub target_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionVcsDiffBaseResolution {
    pub base_commit_sha: String,
    pub unavailable_reason: Option<DiffUnavailableReason>,
    pub explicit_target: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionVcsDiffSummaryCounts {
    pub file_count: i64,
    pub line_additions: i64,
    pub line_deletions: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionVcsGitStatusSnapshot {
    pub raw: String,
    pub summary_line: String,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub detached: bool,
    pub staged: i64,
    pub unstaged: i64,
    pub untracked: i64,
    pub entries: Vec<SessionVcsGitStatusEntry>,
    pub entries_total_count: i64,
    pub entries_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionVcsDiffSummaryMismatch {
    pub snapshot_rev: i64,
    pub snapshot_file_count: Option<i64>,
    pub snapshot_line_additions: Option<i64>,
    pub snapshot_line_deletions: Option<i64>,
    pub actual_file_count: i64,
    pub actual_line_additions: i64,
    pub actual_line_deletions: i64,
}

#[async_trait]
pub trait SessionVcsDataPlane {
    async fn load_session_vcs_parts(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Option<(Session, Worktree)>>;

    async fn persist_session_git_status_summary(
        &self,
        session_id: SessionId,
        worktree_id: WorktreeId,
        summary: &SessionGitStatusSummary,
    ) -> anyhow::Result<()>;

    async fn worktree_has_vcs_repo(&self, worktree: &Worktree) -> anyhow::Result<bool>;

    async fn load_git_status_snapshot(
        &self,
        worktree: &Worktree,
        include_untracked_files: bool,
        include_entries: bool,
    ) -> anyhow::Result<SessionVcsGitStatusSnapshot>;

    async fn resolve_worktree_commit(
        &self,
        worktree: &Worktree,
        revision: &str,
    ) -> anyhow::Result<String>;

    async fn diff_worktree_for_session(
        &self,
        worktree: &Worktree,
        base_commit_sha: &str,
    ) -> anyhow::Result<String>;

    async fn diff_worktree_summary_for_session(
        &self,
        worktree: &Worktree,
        base_commit_sha: &str,
    ) -> anyhow::Result<SessionVcsDiffSummaryCounts>;

    async fn resolve_worktree_diff_base(
        &self,
        worktree: &Worktree,
        query: SessionVcsDiffBaseQuery,
    ) -> SessionVcsDiffBaseResolution;

    async fn apply_worktree_vcs_session_patch(
        &self,
        worktree: &Worktree,
        patch: &str,
        reverse_patch: bool,
    ) -> anyhow::Result<()>;

    async fn session_vcs_diff_summary_mismatch(
        &self,
        worktree: &Worktree,
        base_commit_sha: &str,
        counts: SessionVcsDiffSummaryCounts,
    ) -> Option<SessionVcsDiffSummaryMismatch>;

    async fn emit_compat_payload_reject_counter(&self, surface: &'static str, issue: &'static str);

    fn is_no_vcs_repo_error(&self, error: &Error) -> bool;
}

pub struct SessionVcsService<'a, D: ?Sized> {
    data_plane: &'a D,
}

enum PreparedSessionDiffRequest {
    Available {
        ctx: SessionVcsContext,
        base_commit_sha: String,
    },
    NoRepo {
        ctx: SessionVcsContext,
    },
    Unavailable {
        ctx: SessionVcsContext,
        base_commit_sha: String,
        reason: DiffUnavailableReason,
    },
}

impl<'a, D: ?Sized> SessionVcsService<'a, D>
where
    D: SessionVcsDataPlane + Sync,
{
    pub fn new(data_plane: &'a D) -> Self {
        Self { data_plane }
    }

    pub async fn get_session_vcs_diff(
        &self,
        session_id: SessionId,
        query: SessionVcsDiffQuery,
    ) -> Result<SessionVcsDiff, SessionVcsError> {
        let (ctx, base_commit_sha) = match self
            .prepare_session_diff_request(session_id, query, "sessions.diff")
            .await?
        {
            PreparedSessionDiffRequest::Available {
                ctx,
                base_commit_sha,
            } => (ctx, base_commit_sha),
            PreparedSessionDiffRequest::NoRepo { .. } => {
                return Ok(session_vcs_diff_unavailable(DiffUnavailableReason::NoRepo));
            }
            PreparedSessionDiffRequest::Unavailable { reason, .. } => {
                return Ok(session_vcs_diff_unavailable(reason));
            }
        };
        let diff = match self
            .data_plane
            .diff_worktree_for_session(&ctx.worktree, &base_commit_sha)
            .await
        {
            Ok(diff) => diff,
            Err(err) if self.data_plane.is_no_vcs_repo_error(&err) => {
                return Ok(session_vcs_diff_unavailable(DiffUnavailableReason::NoRepo));
            }
            Err(err) => return Err(SessionVcsError::Internal(err)),
        };
        Ok(SessionVcsDiff {
            diff,
            available: true,
            unavailable_reason: None,
        })
    }

    pub async fn get_session_vcs_diff_summary(
        &self,
        session_id: SessionId,
        query: SessionVcsDiffQuery,
    ) -> Result<SessionVcsDiffSummary, SessionVcsError> {
        match self
            .prepare_session_diff_request(session_id, query, "sessions.diff_summary")
            .await?
        {
            PreparedSessionDiffRequest::Available {
                ctx,
                base_commit_sha,
            } => {
                self.session_vcs_diff_summary_available(&ctx.worktree, base_commit_sha)
                    .await
            }
            PreparedSessionDiffRequest::NoRepo { ctx } => Ok(session_vcs_diff_summary_unavailable(
                ctx.worktree.base_commit_sha.clone(),
                ctx.worktree.base_commit_sha,
                DiffUnavailableReason::NoRepo,
            )),
            PreparedSessionDiffRequest::Unavailable {
                ctx,
                base_commit_sha,
                reason,
            } => {
                let head_commit_sha = self
                    .resolve_head_commit_sha_or_base(&ctx.worktree, &base_commit_sha)
                    .await;
                Ok(session_vcs_diff_summary_unavailable(
                    base_commit_sha,
                    head_commit_sha,
                    reason,
                ))
            }
        }
    }

    pub async fn apply_session_vcs_diff_patch(
        &self,
        session_id: SessionId,
        action: SessionVcsApplyAction,
        patch: &str,
    ) -> Result<SessionVcsDiff, SessionVcsError> {
        let ctx = self.load_session_vcs_context(session_id).await?;
        self.data_plane
            .apply_worktree_vcs_session_patch(&ctx.worktree, patch, action.reverse_patch())
            .await
            .map_err(SessionVcsError::BadPatch)?;

        let resolution = self
            .resolve_session_diff_base(&ctx.worktree, SessionVcsDiffQuery::default())
            .await?;
        if let Some(unavailable_reason) = resolution.unavailable_reason.clone() {
            self.data_plane
                .emit_compat_payload_reject_counter("sessions.diff_apply", "no_target_branch")
                .await;
            return Ok(session_vcs_diff_unavailable(unavailable_reason));
        }
        let diff = self
            .data_plane
            .diff_worktree_for_session(&ctx.worktree, &resolution.base_commit_sha)
            .await
            .map_err(SessionVcsError::Internal)?;
        Ok(SessionVcsDiff {
            diff,
            available: true,
            unavailable_reason: None,
        })
    }

    pub async fn get_session_vcs_git_status(
        &self,
        session_id: SessionId,
    ) -> Result<SessionVcsGitStatus, SessionVcsError> {
        let ctx = self.load_session_vcs_context(session_id).await?;
        let snapshot = self
            .data_plane
            .load_git_status_snapshot(&ctx.worktree, true, true)
            .await
            .map_err(SessionVcsError::Internal)?;
        let summary = session_git_status_summary_from_snapshot(&snapshot);
        let status = SessionVcsGitStatus {
            raw: snapshot.raw,
            summary_line: snapshot.summary_line,
            branch: snapshot.branch,
            upstream: snapshot.upstream,
            ahead: snapshot.ahead,
            behind: snapshot.behind,
            detached: snapshot.detached,
            staged: snapshot.staged,
            unstaged: snapshot.unstaged,
            untracked: snapshot.untracked,
            entries: snapshot.entries,
            entries_truncated: snapshot.entries_truncated,
            entries_total_count: snapshot.entries_total_count,
        };
        if let Err(err) = self
            .data_plane
            .persist_session_git_status_summary(ctx.session.id, ctx.worktree.id, &summary)
            .await
        {
            tracing::warn!(
                session_id = %ctx.session.id.0,
                "git status summary persist failed: {err:?}"
            );
        }
        Ok(status)
    }

    async fn prepare_session_diff_request(
        &self,
        session_id: SessionId,
        query: SessionVcsDiffQuery,
        compat_route: &'static str,
    ) -> Result<PreparedSessionDiffRequest, SessionVcsError> {
        let ctx = self.load_session_vcs_context(session_id).await?;
        if !self
            .data_plane
            .worktree_has_vcs_repo(&ctx.worktree)
            .await
            .map_err(SessionVcsError::Internal)?
        {
            return Ok(PreparedSessionDiffRequest::NoRepo { ctx });
        }
        let resolution = self.resolve_session_diff_base(&ctx.worktree, query).await?;
        if let Some(reason) = resolution.unavailable_reason.clone() {
            self.data_plane
                .emit_compat_payload_reject_counter(compat_route, "no_target_branch")
                .await;
            return Ok(PreparedSessionDiffRequest::Unavailable {
                ctx,
                base_commit_sha: resolution.base_commit_sha,
                reason,
            });
        }
        Ok(PreparedSessionDiffRequest::Available {
            ctx,
            base_commit_sha: resolution.base_commit_sha,
        })
    }

    async fn load_session_vcs_context(
        &self,
        session_id: SessionId,
    ) -> Result<SessionVcsContext, SessionVcsError> {
        let (session, worktree) = self
            .data_plane
            .load_session_vcs_parts(session_id)
            .await
            .map_err(SessionVcsError::Internal)?
            .ok_or(SessionVcsError::NotFound)?;
        Ok(SessionVcsContext { session, worktree })
    }

    async fn resolve_session_diff_base(
        &self,
        worktree: &Worktree,
        query: SessionVcsDiffQuery,
    ) -> Result<SessionVcsDiffBaseResolution, SessionVcsError> {
        let resolution = self
            .data_plane
            .resolve_worktree_diff_base(
                worktree,
                SessionVcsDiffBaseQuery {
                    base_commit_sha: query.base_commit_sha,
                    target_branch: query.target_branch,
                },
            )
            .await;
        if resolution.explicit_target {
            if let Some(error) = resolution.error.clone() {
                return Err(SessionVcsError::InvalidExplicitTarget(error));
            }
        }
        Ok(resolution)
    }

    async fn session_vcs_diff_summary_available(
        &self,
        worktree: &Worktree,
        base_commit_sha: String,
    ) -> Result<SessionVcsDiffSummary, SessionVcsError> {
        let summary_counts = match self
            .data_plane
            .diff_worktree_summary_for_session(worktree, &base_commit_sha)
            .await
        {
            Ok(counts) => Ok(counts),
            Err(err) if self.data_plane.is_no_vcs_repo_error(&err) => {
                Err(DiffUnavailableReason::NoRepo)
            }
            Err(err) => return Err(SessionVcsError::Internal(err)),
        };
        let head_commit_sha = self
            .resolve_head_commit_sha_or_base(worktree, &base_commit_sha)
            .await;
        match summary_counts {
            Ok(counts) => {
                if let Some(mismatch) = self
                    .data_plane
                    .session_vcs_diff_summary_mismatch(worktree, &base_commit_sha, counts)
                    .await
                {
                    tracing::warn!(
                        worktree_id = %worktree.id.0,
                        snapshot_rev = mismatch.snapshot_rev,
                        base_commit_sha = %base_commit_sha,
                        snapshot_file_count = ?mismatch.snapshot_file_count,
                        snapshot_additions = ?mismatch.snapshot_line_additions,
                        snapshot_deletions = ?mismatch.snapshot_line_deletions,
                        summary_file_count = mismatch.actual_file_count,
                        summary_additions = mismatch.actual_line_additions,
                        summary_deletions = mismatch.actual_line_deletions,
                        "worktree vcs snapshot summary mismatch"
                    );
                }
                Ok(SessionVcsDiffSummary {
                    base_commit_sha,
                    head_commit_sha,
                    file_count: counts.file_count,
                    line_additions: counts.line_additions,
                    line_deletions: counts.line_deletions,
                    available: true,
                    unavailable_reason: None,
                })
            }
            Err(unavailable_reason) => Ok(session_vcs_diff_summary_unavailable(
                base_commit_sha,
                head_commit_sha,
                unavailable_reason,
            )),
        }
    }

    async fn resolve_head_commit_sha_or_base(
        &self,
        worktree: &Worktree,
        base_commit_sha: &str,
    ) -> String {
        self.data_plane
            .resolve_worktree_commit(worktree, "HEAD")
            .await
            .unwrap_or_else(|_| base_commit_sha.to_string())
    }
}

fn session_vcs_diff_unavailable(reason: DiffUnavailableReason) -> SessionVcsDiff {
    SessionVcsDiff {
        diff: String::new(),
        available: false,
        unavailable_reason: Some(reason),
    }
}

fn session_vcs_diff_summary_unavailable(
    base_commit_sha: String,
    head_commit_sha: String,
    reason: DiffUnavailableReason,
) -> SessionVcsDiffSummary {
    SessionVcsDiffSummary {
        base_commit_sha,
        head_commit_sha,
        file_count: 0,
        line_additions: 0,
        line_deletions: 0,
        available: false,
        unavailable_reason: Some(reason),
    }
}

fn session_git_status_summary_from_snapshot(
    snapshot: &SessionVcsGitStatusSnapshot,
) -> SessionGitStatusSummary {
    SessionGitStatusSummary {
        summary_line: snapshot.summary_line.clone(),
        branch: snapshot.branch.clone(),
        upstream: snapshot.upstream.clone(),
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        detached: snapshot.detached,
        staged: snapshot.staged,
        unstaged: snapshot.unstaged,
        untracked: snapshot.untracked,
    }
}

#[cfg(test)]
mod tests;
