//! Complete event selection and whole-conversation scope on one pinned reader.

pub mod summary;

use crate::{
    hex, invalid, ArchiveIdentity, ExportReceipt, Result, Selection, StableEntityId, VerifiedIndex,
};
use ctx_history_index::{
    CoreEventRangeDomain, CoreEventRangeFilters, CoreEventRangeSelection,
    DEFAULT_CORE_EVENT_PAGE_BUDGET,
};
use rusqlite::{params, Connection};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use summary::{ConversationPreview, ExportPreview, ScopeCounts};

const PAGE_SIZE: usize = 64;
const PREVIEW_CONVERSATIONS: usize = 100;

pub struct ExportPlan {
    root: PathBuf,
    index: VerifiedIndex,
    explicit: Selection,
    filtered: bool,
    db: Connection,
    preview: ExportPreview,
}

impl ExportPlan {
    pub fn preview(&self) -> &ExportPreview {
        &self.preview
    }

    /// Publication uses the same immutable generation and exact resolved set.
    pub fn export(&self, destination: &Path) -> Result<ExportReceipt> {
        if self.preview.selected_conversations == 0 {
            return Err(invalid("no matching conversations; no archive created"));
        }
        crate::retained::export_pinned(
            &self.root,
            &self.index,
            destination,
            self.preview.identity.clone(),
            &crate::selection::SessionSelection {
                explicit: &self.explicit,
                resolved: Some(&self.db),
                filtered: self.filtered,
            },
        )
    }
}

pub fn plan_export(
    root: &Path,
    identity: ArchiveIdentity,
    explicit: &Selection,
    query: &CoreEventRangeSelection,
) -> Result<ExportPlan> {
    plan_export_with_control(root, identity, explicit, query, || Ok(()))
}

/// Any interrupted traversal fails without returning a partial selection.
pub fn plan_export_with_control(
    root: &Path,
    identity: ArchiveIdentity,
    explicit: &Selection,
    query: &CoreEventRangeSelection,
    mut control: impl FnMut() -> Result<()>,
) -> Result<ExportPlan> {
    identity.validate()?;
    explicit.validate()?;
    control()?;
    let index = VerifiedIndex::open_pinned(root.join("search/lexical"))?;
    let origins = original_identities(root, &index)?;
    let identities: BTreeSet<_> = origins
        .values()
        .map(|i| (i.origin.clone(), i.view.clone()))
        .collect();
    if !identities.is_empty()
        && !identities.contains(&(identity.origin.clone(), identity.view.clone()))
    {
        return Err(invalid("--origin and --view must select an original identity retained in this archive-owned root; restored history cannot be relabeled"));
    }
    let excluded_identities = identities
        .into_iter()
        .filter(|(origin, view)| origin != &identity.origin || view != &identity.view)
        .map(|(origin, view)| ArchiveIdentity { origin, view })
        .collect();
    // SQLite's temporary database uses OS delete-on-close semantics, including
    // abrupt process termination. Its bounded cache can still spill to disk.
    let db = Connection::open("")?;
    db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-2048;
        CREATE TABLE selected_sessions(id TEXT PRIMARY KEY, source TEXT NOT NULL, stable TEXT NOT NULL, matches INTEGER NOT NULL);
        CREATE INDEX selected_sources ON selected_sessions(source); BEGIN;")?;
    let mut cursor = None;
    loop {
        control()?;
        let page = index.core_event_range_page_with_budget(
            query,
            cursor.as_ref(),
            PAGE_SIZE,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )?;
        for event in page.items {
            control()?;
            let record = &event.core_record;
            if !explicit.includes_source(&record.source)
                || !explicit.includes_session(record.session_id)
                || origins
                    .get(&record.source.identity().digest())
                    .is_some_and(|origin| origin != &identity)
            {
                continue;
            }
            db.execute(
                "INSERT INTO selected_sessions VALUES (?1,?2,?3,1)
                ON CONFLICT(id) DO UPDATE SET matches=matches+1",
                params![
                    hex(&record.session_id.digest()),
                    hex(&record.source.identity().digest()),
                    serde_json::to_string(&record.session_id)?
                ],
            )?;
        }
        if page.terminal {
            break;
        }
        cursor = Some(
            page.next_cursor
                .ok_or_else(|| invalid("incomplete event selection: no continuation cursor"))?,
        );
    }
    db.execute_batch("COMMIT")?;
    let mut preview = ExportPreview {
        generation_id: index.generation_id().to_owned(),
        identity,
        excluded_identities,
        workspace: query.filters().workspace.clone(),
        since_unix_ms: query.since_unix_ms(),
        until_unix_ms: query.until_unix_ms(),
        selection_complete: true,
        selected_conversations: 0,
        scope: ScopeCounts::default(),
        conversations: Vec::new(),
        conversations_truncated: false,
    };
    {
        let mut sessions =
            db.prepare("SELECT stable,matches FROM selected_sessions ORDER BY id")?;
        let mut rows = sessions.query([])?;
        while let Some(row) = rows.next()? {
            control()?;
            let session: StableEntityId = serde_json::from_str(&row.get::<_, String>(0)?)?;
            let mut conversation = ConversationPreview::new(session, row.get(1)?);
            let mut cursor = None;
            loop {
                control()?;
                let page = index.core_session_event_page_with_budget(
                    session.as_uuid(),
                    cursor.as_ref(),
                    PAGE_SIZE,
                    DEFAULT_CORE_EVENT_PAGE_BUDGET,
                )?;
                for event in page.items {
                    control()?;
                    conversation.observe(&event, query);
                }
                if page.terminal {
                    break;
                }
                cursor = Some(page.next_cursor.ok_or_else(|| {
                    invalid("incomplete conversation scope: no continuation cursor")
                })?);
            }
            preview.selected_conversations += 1;
            preview.scope.add(&conversation.scope);
            if preview.conversations.len() < PREVIEW_CONVERSATIONS {
                preview.conversations.push(conversation);
            } else {
                preview.conversations_truncated = true;
            }
        }
    }
    control()?;
    Ok(ExportPlan {
        root: root.to_owned(),
        index,
        explicit: explicit.clone(),
        filtered: query.domain() != CoreEventRangeDomain::All
            || query.filters() != &CoreEventRangeFilters::default(),
        db,
        preview,
    })
}

fn original_identities(
    root: &Path,
    index: &VerifiedIndex,
) -> Result<BTreeMap<[u8; 32], ArchiveIdentity>> {
    let owned = crate::is_archive_root(root)?;
    let mut origins = BTreeMap::new();
    for certificate in &index.manifest().sources {
        let observation = certificate.observation();
        if !owned {
            if observation.source().source_format() == "ctx_archive_v1" {
                return Err(invalid(
                    "restored history requires its archive-owned root for faithful export",
                ));
            }
            continue;
        }
        if observation.source().source_format() != "ctx_archive_v1" {
            return Err(invalid(
                "archive-owned export requires committed original-member references",
            ));
        }
        let member = crate::retained::OwnedMember::decode(observation)?;
        let identity = if let Some(identity) = member.identity {
            identity
        } else {
            if !crate::is_digest(&member.snapshot) {
                return Err(invalid("invalid committed original-member reference"));
            }
            let manifest = crate::io::read_manifest(&root.join("archives").join(&member.snapshot))?;
            if manifest.snapshot_id()? != member.snapshot {
                return Err(invalid(
                    "owned archive manifest differs from committed snapshot",
                ));
            }
            manifest.identity
        };
        origins.insert(observation.source().identity().digest(), identity);
    }
    Ok(origins)
}
