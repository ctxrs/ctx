use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use crate::complete_content::sqlite::{
    configure_complete_content_sqlite_connection, CompleteContentSqliteBoundError,
    CompleteContentSqliteQueryBudget,
};
use crate::native_source::{NativeLocator, NativeSqliteValue};
use crate::provider::provider_safe_path_segment;
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, open_provider_sqlite_readonly, sqlite_table_columns,
    sqlite_table_exists, ReadOnlySqliteConnection,
};
use crate::CaptureError;

use super::position::{
    decode_nanoclaw_message_locator, NanoClawMessageLocator, NanoClawMessageSource,
};
use super::project::{
    nanoclaw_project_root, NanoClawProjectDatabaseSnapshot, NanoClawSqliteSnapshot,
};
use super::projection::{nanoclaw_core_event, NanoClawCoreEvent};
use super::rows::{
    nanoclaw_hydrate_native_message, nanoclaw_hydrate_native_session,
    nanoclaw_message_digest_values, nanoclaw_session_candidate_by_rowid, nanoclaw_session_columns,
};

pub(crate) struct NanoClawCompleteRecord {
    pub(crate) provider_session_id: String,
    pub(crate) event: NanoClawCoreEvent,
    pub(crate) text: String,
    pub(crate) values: Vec<NativeSqliteValue>,
}

/// One bounded, caller-selected view of a NanoClaw project.
///
/// The same component-snapshot primitive used by capture freezes the central
/// database and exactly the inbound/outbound components named by caller
/// locators. Each request addresses one exact central session row, database
/// role, and message row; no directory search or best-effort matching occurs.
pub(crate) struct NanoClawCompleteProject {
    data_root: PathBuf,
    central_path: PathBuf,
    central_snapshot: NanoClawSqliteSnapshot,
    central: ReadOnlySqliteConnection,
    session_columns: BTreeSet<String>,
    components: BTreeMap<(i64, NanoClawMessageSource), Option<NanoClawSelectedComponent>>,
    query_budget: CompleteContentSqliteQueryBudget,
}

struct NanoClawSelectedComponent {
    agent_group_id: String,
    session_id: String,
    database: NanoClawProjectDatabaseSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NanoClawComponentAddress {
    pub(crate) session_rowid: i64,
    pub(crate) source: NanoClawMessageSource,
    pub(crate) agent_group_id: String,
    pub(crate) session_id: String,
}

/// Resolves only caller-selected compound coordinates against an already
/// frozen central database. The source-access broker uses these path-safe IDs
/// to snapshot the exact component databases needed by one resolution batch.
pub(crate) fn selected_component_addresses(
    central: &ReadOnlySqliteConnection,
    locators: &[NativeLocator],
) -> Result<Vec<NanoClawComponentAddress>, CompleteContentSqliteBoundError> {
    let session_columns = nanoclaw_session_columns(central)?;
    let mut addresses = BTreeSet::new();
    for locator in locators {
        let coordinate = decode_nanoclaw_message_locator(locator)?;
        let Some(candidate) = nanoclaw_session_candidate_by_rowid(
            central,
            &session_columns,
            coordinate.session_rowid,
        )?
        else {
            continue;
        };
        let session = nanoclaw_hydrate_native_session(central, &session_columns, candidate.rowid)?;
        if !provider_safe_path_segment(&session.agent_group_id)
            || !provider_safe_path_segment(&session.id)
        {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        addresses.insert(NanoClawComponentAddress {
            session_rowid: coordinate.session_rowid,
            source: coordinate.source,
            agent_group_id: session.agent_group_id,
            session_id: session.id,
        });
    }
    Ok(addresses.into_iter().collect())
}

impl NanoClawCompleteProject {
    pub(crate) fn open(
        data_root: &Path,
        path: &Path,
        locators: &[NativeLocator],
        query_budget: CompleteContentSqliteQueryBudget,
    ) -> Result<Self, CompleteContentSqliteBoundError> {
        let project_root = fs::canonicalize(nanoclaw_project_root(path)?)?;
        let central_path = project_root.join("data").join("v2.db");
        let central_snapshot = NanoClawSqliteSnapshot::read(&central_path)?;
        let central = open_provider_sqlite_readonly(data_root, &central_path)?;
        configure_complete_content_sqlite_connection(&central, query_budget)?;
        if !central_snapshot.revalidate(&central_path)? {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let session_columns = nanoclaw_session_columns(&central)?;
        let mut project = Self {
            data_root: data_root.to_path_buf(),
            central_path,
            central_snapshot,
            central,
            session_columns,
            components: BTreeMap::new(),
            query_budget,
        };
        for locator in locators {
            let coordinate = decode_nanoclaw_message_locator(locator)?;
            project.freeze_component(&project_root, coordinate)?;
        }
        if !project.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        Ok(project)
    }

    fn freeze_component(
        &mut self,
        project_root: &Path,
        coordinate: NanoClawMessageLocator,
    ) -> Result<(), CompleteContentSqliteBoundError> {
        let key = (coordinate.session_rowid, coordinate.source);
        if self.components.contains_key(&key) {
            return Ok(());
        }
        let Some(candidate) = nanoclaw_session_candidate_by_rowid(
            &self.central,
            &self.session_columns,
            coordinate.session_rowid,
        )?
        else {
            self.components.insert(key, None);
            return Ok(());
        };
        let session =
            nanoclaw_hydrate_native_session(&self.central, &self.session_columns, candidate.rowid)?;
        if !provider_safe_path_segment(&session.agent_group_id)
            || !provider_safe_path_segment(&session.id)
        {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let session_dir = project_root
            .join("data")
            .join("v2-sessions")
            .join(&session.agent_group_id)
            .join(&session.id);
        let database = NanoClawProjectDatabaseSnapshot::read(
            &self.data_root,
            &session_dir,
            coordinate.source,
        )?;
        if database.is_present() && !fs::canonicalize(database.path())?.starts_with(project_root) {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: database.path().to_path_buf(),
                reason: "NanoClaw message database escapes the selected project root",
            }
            .into());
        }
        self.components.insert(
            key,
            Some(NanoClawSelectedComponent {
                agent_group_id: session.agent_group_id,
                session_id: session.id,
                database,
            }),
        );
        Ok(())
    }

    pub(crate) fn resolve(
        &self,
        locator: &NativeLocator,
    ) -> Result<Option<NanoClawCompleteRecord>, CompleteContentSqliteBoundError> {
        let coordinate = decode_nanoclaw_message_locator(locator)?;
        let Some(candidate) = nanoclaw_session_candidate_by_rowid(
            &self.central,
            &self.session_columns,
            coordinate.session_rowid,
        )?
        else {
            return Ok(None);
        };
        let session =
            nanoclaw_hydrate_native_session(&self.central, &self.session_columns, candidate.rowid)?;
        if !provider_safe_path_segment(&session.agent_group_id)
            || !provider_safe_path_segment(&session.id)
        {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let Some(component) = self
            .components
            .get(&(coordinate.session_rowid, coordinate.source))
            .and_then(Option::as_ref)
        else {
            return Ok(None);
        };
        if component.agent_group_id != session.agent_group_id || component.session_id != session.id
        {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let component = &component.database;
        if !component.is_present() {
            return Ok(None);
        }
        let conn = open_provider_sqlite_readonly(&self.data_root, component.path())?;
        configure_complete_content_sqlite_connection(&conn, self.query_budget)?;
        if !component.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let table = coordinate.source.table();
        if !sqlite_table_exists(&conn, table)? {
            return Err(CaptureError::InvalidPayload(format!(
                "NanoClaw {table} component is missing its message table"
            ))
            .into());
        }
        let columns = sqlite_table_columns(&conn, table)?;
        ensure_sqlite_table_columns(&columns, table, &["id"])?;
        let message = match nanoclaw_hydrate_native_message(
            &conn,
            &columns,
            coordinate.source,
            coordinate.message_rowid,
        ) {
            Ok(values) => values,
            Err(CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let logical_values = nanoclaw_message_digest_values(&message);
        if !component.revalidate()? || !self.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let seq = message
            .seq
            .map(|value| {
                u64::try_from(value).map_err(|_| {
                    CaptureError::InvalidPayload(
                        "NanoClaw complete-content message seq must be nonnegative".to_owned(),
                    )
                })
            })
            .transpose()?;
        let (event, text) =
            nanoclaw_core_event(&session, &message, seq, chrono::DateTime::UNIX_EPOCH);
        Ok(Some(NanoClawCompleteRecord {
            provider_session_id: format!("{}/{}", session.agent_group_id, session.id),
            event,
            text,
            values: logical_values,
        }))
    }

    pub(crate) fn revalidate(&self) -> Result<bool, CompleteContentSqliteBoundError> {
        if !self.central_snapshot.revalidate(&self.central_path)? {
            return Ok(false);
        }
        for component in self.components.values().flatten() {
            if !component.database.revalidate()? {
                return Ok(false);
            }
        }
        Ok(self.central_snapshot.revalidate(&self.central_path)?)
    }
}
