use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use ctx_history_archive::{ArchiveIdentity, Manifest, Selection, SessionMember};
use ctx_history_core::LiteralFactKind;
use ctx_history_index::VerifiedIndex;

use crate::{
    Error, PublicationMode, Result, SelectionDecision, SessionScope, SharingPolicy, SharingStore,
    SourceSelection,
};

impl SharingStore {
    /// Construct a policy from the same immutable bytes left for inspection.
    /// This does not save/enable it: the explicit share command calls set_policy.
    /// Empty reviewed pins select eligible current revisions; supplied pins can
    /// only narrow that set. Every source baseline includes observed sessions
    /// even when work-root or backfill filters hold them.
    pub fn prepare_policy(
        &self,
        data_root: &Path,
        preview_destination: &Path,
        mode: PublicationMode,
        mut sources: Vec<SourceSelection>,
    ) -> Result<SharingPolicy> {
        let settings = self.settings()?.ok_or(Error::NotConnected)?;
        let previous = settings.policy;
        for source in &mut sources {
            source.baseline_revisions.clear();
        }
        let mut policy = SharingPolicy {
            revision: previous
                .as_ref()
                .map_or(Some(1), |p| p.revision.checked_add(1))
                .ok_or(Error::PolicyConflict)?,
            archive_identity: previous
                .as_ref()
                .map(|p| p.archive_identity.clone())
                .unwrap_or_else(|| ArchiveIdentity {
                    origin: uuid::Uuid::new_v4().to_string(),
                    view: uuid::Uuid::new_v4().to_string(),
                }),
            writer_epoch: previous.as_ref().map_or(1, |p| p.writer_epoch),
            mode: PublicationMode::Automatic,
            sources,
        };
        policy.validate()?;
        let mut reviewed = BTreeSet::new();
        let selections = policy.sources.clone();
        preview_committed(
            data_root,
            preview_destination,
            policy.archive_identity.clone(),
            &selections,
            |_, scope| {
                for source in &mut policy.sources {
                    if source.matches_scope(scope) {
                        source
                            .baseline_revisions
                            .insert(scope.session_id.clone(), scope.revision.clone());
                    }
                }
                if policy.select(scope) == SelectionDecision::Selected {
                    reviewed.insert(scope.revision.clone());
                }
                Ok(())
            },
        )?;
        policy.mode = match mode {
            PublicationMode::Automatic => PublicationMode::Automatic,
            PublicationMode::Reviewed { revisions } => PublicationMode::Reviewed {
                revisions: if revisions.is_empty() {
                    reviewed
                } else {
                    reviewed.intersection(&revisions).cloned().collect()
                },
            },
        };
        Ok(policy)
    }
}

/// Export a committed, pinned generation and stream its selection evidence to
/// the caller for preview/baseline approval. No provider files or network are
/// consulted. The caller owns the inspectable archive at `destination`.
pub fn preview_committed(
    data_root: &Path,
    destination: &Path,
    identity: ArchiveIdentity,
    selections: &[SourceSelection],
    mut visit: impl FnMut(&SessionMember, &SessionScope) -> Result<()>,
) -> Result<Manifest> {
    let index = open_index(data_root)?;
    export_index_control(
        &index,
        destination,
        identity,
        selections,
        &mut visit,
        &AtomicBool::new(false),
    )
}

pub(crate) fn open_index(data_root: &Path) -> Result<VerifiedIndex> {
    VerifiedIndex::open_pinned(data_root.join("search/lexical")).map_err(|_| Error::Archive)
}

pub(crate) fn export_index_control(
    index: &VerifiedIndex,
    destination: &Path,
    identity: ArchiveIdentity,
    selections: &[SourceSelection],
    visit: &mut impl FnMut(&SessionMember, &SessionScope) -> Result<()>,
    stop: &AtomicBool,
) -> Result<Manifest> {
    let mut sources = BTreeSet::new();
    for selection in selections {
        if let Some(source) = &selection.source_id {
            sources.insert(source.clone());
        } else if let Some(path) = &selection.profile_root {
            for root in index
                .manifest()
                .provider_roots()
                .iter()
                .filter(|r| &r.definition().path == path)
            {
                sources.extend(
                    index
                        .manifest()
                        .provider_root_source_tokens(&[root.definition().id.clone()], &[])
                        .map_err(|_| Error::Archive)?,
                );
            }
        }
    }
    // The archive's empty set means all, which is never an implicit sharing
    // selection. An absent selected profile/source must not expand collection.
    if sources.is_empty() {
        return Err(Error::PolicyDenied);
    }
    let manifest = ctx_history_archive::export_with_control(
        index,
        destination,
        identity,
        &Selection {
            sources,
            sessions: BTreeSet::new(),
        },
        || control(stop),
    )
    .map_err(|_| Error::Archive)?;
    let mut callback_error = None;
    ctx_history_archive::visit_members(destination, |member| {
        control(stop)?;
        let result = session_scope(index, destination, &member, stop)
            .and_then(|scope| visit(&member, &scope));
        if let Err(error) = result {
            callback_error = Some(error);
            return Err(ctx_history_archive::ArchiveError::Invalid(
                "sharing preview stopped".to_owned(),
            ));
        }
        Ok(())
    })
    .map_err(|_| callback_error.unwrap_or(Error::Archive))?;
    Ok(manifest)
}

fn session_scope(
    index: &VerifiedIndex,
    archive: &Path,
    member: &SessionMember,
    stop: &AtomicBool,
) -> Result<SessionScope> {
    let source_id = hex(&member.source.identity().digest());
    let mut profile_roots = Vec::new();
    for root in index.manifest().provider_roots() {
        let tokens = index
            .manifest()
            .provider_root_source_tokens(&[root.definition().id.clone()], &[])
            .map_err(|_| Error::Archive)?;
        if tokens.contains(&source_id) {
            profile_roots.push(root.definition().path.clone());
        }
    }
    let mut work_roots = BTreeSet::new();
    let mut first_event_unix_ms: Option<i64> = None;
    let mut missing_time = false;
    ctx_history_archive::visit_records(archive, member, |record| {
        control(stop)?;
        match record.occurred_at_unix_ms {
            Some(t) => first_event_unix_ms = Some(first_event_unix_ms.map_or(t, |old| old.min(t))),
            None => missing_time = true,
        }
        if let Some(activity) = record.content.activity {
            for fact in activity.facts {
                if matches!(
                    fact.kind,
                    LiteralFactKind::SessionCwd
                        | LiteralFactKind::ToolWorkdir
                        | LiteralFactKind::Workspace
                ) {
                    work_roots.insert(PathBuf::from(fact.value));
                }
            }
        }
        Ok(())
    })
    .map_err(|_| Error::Archive)?;
    Ok(SessionScope {
        source_id,
        profile_roots,
        session_id: hex(&member.session_id.digest()),
        revision: member.sha256.clone(),
        unknown_work_root: work_roots.is_empty(),
        work_roots: work_roots.into_iter().collect(),
        // Since-backfill cannot prove that unknown-time records are in range.
        first_event_unix_ms: if missing_time {
            None
        } else {
            first_event_unix_ms
        },
    })
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn control(stop: &AtomicBool) -> ctx_history_archive::Result<()> {
    if stop.load(Ordering::Acquire) {
        return Err(ctx_history_archive::ArchiveError::Invalid(
            "sharing stopped".into(),
        ));
    }
    Ok(())
}
