use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Baselines are collected from the exact committed generation shown at policy
/// approval. They distinguish historical sessions from future sessions without
/// treating a missing/unknown event timestamp as proof of recency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSelection {
    pub source_id: Option<String>,
    pub profile_root: Option<PathBuf>,
    pub baseline_revisions: BTreeMap<String, String>,
    pub backfill: Backfill,
    pub include_future: bool,
    /// Explicit whole-source authorization includes sessions with unknown or
    /// mixed working directories. Otherwise every observed work root must fit.
    pub whole_source: bool,
    pub work_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Backfill {
    None,
    All,
    Since { unix_ms: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PublicationMode {
    Automatic,
    /// Full member digest, not a session ID that could approve later edits.
    Reviewed {
        revisions: BTreeSet<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharingPolicy {
    pub revision: u64,
    pub archive_identity: ctx_history_archive::ArchiveIdentity,
    pub writer_epoch: u64,
    pub mode: PublicationMode,
    pub sources: Vec<SourceSelection>,
}

/// Evidence from the committed records and local source registration. These
/// paths are used for local selection only, never dereferenced on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionScope {
    pub source_id: String,
    pub profile_roots: Vec<PathBuf>,
    pub session_id: String,
    pub revision: String,
    pub work_roots: Vec<PathBuf>,
    pub unknown_work_root: bool,
    pub first_event_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionDecision {
    Selected,
    UnselectedSource,
    ChangedProfile,
    OutsideWorkRoots,
    UnknownWorkRoot,
    BackfillExcluded,
    FutureExcluded,
    NeedsReview,
}

/// Selection of the current committed sessions, separate from queued older
/// revisions. Counts contain no transcript bytes or session identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionObservation {
    pub generation: String,
    pub policy_revision: u64,
    pub counts: BTreeMap<SelectionDecision, u64>,
}

impl SelectionObservation {
    pub fn selected(&self) -> u64 {
        self.counts
            .get(&SelectionDecision::Selected)
            .copied()
            .unwrap_or(0)
    }

    pub fn held(&self) -> u64 {
        use SelectionDecision::*;
        [
            ChangedProfile,
            OutsideWorkRoots,
            UnknownWorkRoot,
            NeedsReview,
        ]
        .iter()
        .map(|reason| self.counts.get(reason).copied().unwrap_or(0))
        .sum()
    }

    pub fn omitted(&self) -> u64 {
        use SelectionDecision::*;
        [UnselectedSource, BackfillExcluded, FutureExcluded]
            .iter()
            .map(|reason| self.counts.get(reason).copied().unwrap_or(0))
            .sum()
    }
}

impl SharingPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.revision == 0 || self.writer_epoch == 0 || self.sources.is_empty() {
            return Err(Error::InvalidConfig);
        }
        self.archive_identity
            .validate()
            .map_err(|_| Error::InvalidConfig)?;
        let mut seen = BTreeSet::new();
        for source in &self.sources {
            if (source.source_id.is_none() && source.profile_root.is_none())
                || source.source_id.as_ref().is_some_and(|id| id.is_empty())
                || !seen.insert((&source.source_id, &source.profile_root))
                || source
                    .profile_root
                    .as_ref()
                    .is_some_and(|p| !absolute_clean(p))
                || source.work_roots.iter().any(|p| !absolute_clean(p))
                || (!source.whole_source && source.work_roots.is_empty())
                || (source.whole_source && !source.work_roots.is_empty())
            {
                return Err(Error::InvalidConfig);
            }
        }
        Ok(())
    }

    pub fn select(&self, scope: &SessionScope) -> SelectionDecision {
        let mut decision = SelectionDecision::UnselectedSource;
        for source in &self.sources {
            let current = self.select_source(source, scope);
            if current == SelectionDecision::Selected {
                return current;
            }
            if current != SelectionDecision::UnselectedSource {
                decision = current;
            }
        }
        decision
    }

    fn select_source(&self, source: &SourceSelection, scope: &SessionScope) -> SelectionDecision {
        use SelectionDecision::*;
        if source
            .source_id
            .as_ref()
            .is_some_and(|id| id != &scope.source_id)
        {
            return UnselectedSource;
        }
        if source
            .profile_root
            .as_ref()
            .is_some_and(|p| !scope.profile_roots.contains(p))
        {
            return ChangedProfile;
        }
        // Explicit historical/future omissions do not require work-root review.
        match source.baseline_revisions.get(&scope.session_id) {
            Some(baseline) => {
                if match source.backfill {
                    Backfill::None => true,
                    Backfill::All => false,
                    Backfill::Since { unix_ms } => {
                        scope.first_event_unix_ms.is_none_or(|t| t < unix_ms)
                    }
                } {
                    return BackfillExcluded;
                }
                if !source.include_future && baseline != &scope.revision {
                    return FutureExcluded;
                }
            }
            None if !source.include_future => return FutureExcluded,
            None => {}
        }
        if !source.whole_source {
            if scope.unknown_work_root || scope.work_roots.is_empty() {
                return UnknownWorkRoot;
            }
            if scope.work_roots.iter().any(|root| {
                !absolute_clean(root)
                    || !source
                        .work_roots
                        .iter()
                        .any(|allowed| root.starts_with(allowed))
            }) {
                return OutsideWorkRoots;
            }
        }
        if let PublicationMode::Reviewed { revisions } = &self.mode {
            if !revisions.contains(&scope.revision) {
                return NeedsReview;
            }
        }
        Selected
    }
}

impl SourceSelection {
    pub(crate) fn matches_scope(&self, scope: &SessionScope) -> bool {
        self.source_id
            .as_ref()
            .is_none_or(|id| id == &scope.source_id)
            && self
                .profile_root
                .as_ref()
                .is_none_or(|p| scope.profile_roots.contains(p))
    }
}

fn absolute_clean(path: &Path) -> bool {
    path.is_absolute() && !path.components().any(|c| matches!(c, Component::ParentDir))
}
