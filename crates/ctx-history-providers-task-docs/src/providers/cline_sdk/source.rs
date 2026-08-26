use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Component, Path, PathBuf},
};

use ctx_history_source_io::{OpenedProviderSourceFile, ProviderSourceRoot, SourceIoError};
use ctx_history_source_sqlite::{open_provider_sqlite_readonly, SqliteIoError};
use rusqlite::Connection;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::provider_safe_path_segment;

pub(super) const INDEX_PATH: &str = "sessions/sessions.index.json";
pub(super) const DATABASE_PATH: &str = "db/sessions.db";
pub(super) const MAX_CLINE_SESSIONS: usize = 65_536;
pub(super) const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MAX_MESSAGES_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_SQLITE_CATALOG_MATERIALIZED_BYTES: usize = 8 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 32 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 4 * 1024;
const SQLITE_CATALOG_ROW_OVERHEAD_BYTES: usize = 16 * 64;
const TREE_DOMAIN: &[u8] = b"ctx.cline.sdk.compound-tree.v1\0";
const LEAF_DOMAIN: &[u8] = b"ctx.cline.sdk.compound-leaf.v1\0";
const INDEX_FENCE_DOMAIN: &[u8] = b"ctx.cline.sdk.index-fence.v1\0";
const DATABASE_FENCE_DOMAIN: &[u8] = b"ctx.cline.sdk.database-fence.v1\0";

pub(super) type BoundLeafFiles = (Option<Vec<u8>>, Option<Vec<u8>>);

#[derive(Debug, Error)]
pub(super) enum ClineSdkError {
    #[error(transparent)]
    SourceIo(#[from] SourceIoError),
    #[error(transparent)]
    SqliteIo(#[from] SqliteIoError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid Cline SDK session store: {0}")]
    Invalid(String),
    #[error("the selected Cline SDK data root has no sessions.index.json or db/sessions.db")]
    MissingCatalog,
    #[error("Cline SDK source changed during capture")]
    SourceChanged,
}

pub(super) type Result<T> = std::result::Result<T, ClineSdkError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileEvidence {
    pub(super) relative_path: PathBuf,
    pub(super) length: u64,
    pub(super) token: [u8; 32],
}

impl FileEvidence {
    fn from_open(relative_path: PathBuf, file: &OpenedProviderSourceFile) -> Self {
        Self {
            relative_path,
            length: file.len(),
            token: file.ordinary_file_token(),
        }
    }

    fn hash_into(&self, digest: &mut Sha256) {
        hash_path(digest, &self.relative_path);
        digest.update(self.length.to_be_bytes());
        digest.update(self.token);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SessionMetadata {
    pub(super) model: Option<String>,
    pub(super) provider: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) workspace_root: Option<String>,
    pub(super) parent_session_id: Option<String>,
    pub(super) parent_agent_id: Option<String>,
    pub(super) agent_id: Option<String>,
    pub(super) conversation_id: Option<String>,
    pub(super) is_subagent: Option<bool>,
    pub(super) started_at: Option<String>,
    pub(super) updated_at: Option<String>,
    pub(super) fork_parent: bool,
    pub(super) index_row: Option<Value>,
    pub(super) database_row: Option<Value>,
    pub(super) manifest: Option<Value>,
    pub(super) malformed_manifest: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionLeaf {
    pub(super) provider_session_id: String,
    pub(super) source_key_digest: [u8; 32],
    pub(super) catalog_evidence: [u8; 32],
    pub(super) catalog_binding_failure: Option<String>,
    pub(super) manifest_relative_path: PathBuf,
    pub(super) manifest: Option<FileEvidence>,
    pub(super) messages: Option<FileEvidence>,
    pub(super) messages_relative_path: PathBuf,
    pub(super) metadata: SessionMetadata,
}

impl SessionLeaf {
    pub(super) fn fingerprint(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(LEAF_DOMAIN);
        hash_text(&mut digest, &self.provider_session_id);
        digest.update(self.source_key_digest);
        digest.update(self.catalog_evidence);
        hash_path(&mut digest, &self.manifest_relative_path);
        hash_optional_file(&mut digest, self.manifest.as_ref());
        hash_optional_file(&mut digest, self.messages.as_ref());
        hash_path(&mut digest, &self.messages_relative_path);
        if let Some(detail) = self.catalog_binding_failure.as_deref() {
            digest.update(b"ctx.cline.sdk.catalog-binding-failure.v1\0");
            hash_text(&mut digest, detail);
        }
        digest.finalize().into()
    }
}

#[derive(Debug)]
pub(super) struct ClineSdkTreeSnapshot {
    pub(super) authority: ProviderSourceRoot,
    pub(super) leaves: Vec<SessionLeaf>,
    pub(super) tree_fingerprint: [u8; 32],
}

#[derive(Debug, Default)]
struct CatalogEntry {
    index_row: Option<Value>,
    database_row: Option<Value>,
}

#[derive(Debug)]
struct CatalogRows {
    rows: BTreeMap<String, Value>,
}

#[derive(Debug)]
struct CatalogAttempt {
    rows: Option<CatalogRows>,
    fence: [u8; 32],
    error: Option<ClineSdkError>,
    present: bool,
}

impl CatalogAttempt {
    fn missing(domain: &[u8]) -> Self {
        Self {
            rows: None,
            fence: catalog_fence(domain, 0, None),
            error: None,
            present: false,
        }
    }

    fn valid(domain: &[u8], evidence: [u8; 32], rows: BTreeMap<String, Value>) -> Self {
        Self {
            rows: Some(CatalogRows { rows }),
            fence: catalog_fence(domain, 1, Some(evidence)),
            error: None,
            present: true,
        }
    }

    fn invalid(domain: &[u8], evidence: Option<[u8; 32]>, error: ClineSdkError) -> Self {
        Self {
            rows: None,
            fence: catalog_fence(domain, 2, evidence),
            error: Some(error),
            present: true,
        }
    }
}

pub(super) fn discover_cline_sdk_tree(
    root: &Path,
    data_root: &Path,
) -> Result<ClineSdkTreeSnapshot> {
    let authority = ProviderSourceRoot::open(root)?;
    let mut index = read_index_catalog(&authority);
    let mut database = read_database_catalog(&authority, data_root);
    if index.rows.is_none() && database.rows.is_none() {
        if !index.present && !database.present {
            return Err(ClineSdkError::MissingCatalog);
        }
        return Err(index
            .error
            .take()
            .or_else(|| database.error.take())
            .unwrap_or_else(|| ClineSdkError::Invalid("no usable Cline SDK catalog".into())));
    }

    let mut catalog = BTreeMap::<String, CatalogEntry>::new();
    if let Some(index) = index.rows.take() {
        for (session_id, row) in index.rows {
            catalog.entry(session_id).or_default().index_row = Some(row);
        }
    }
    if let Some(database) = database.rows.take() {
        for (session_id, row) in database.rows {
            catalog.entry(session_id).or_default().database_row = Some(row);
        }
    }
    if catalog.len() > MAX_CLINE_SESSIONS {
        return Err(ClineSdkError::Invalid(format!(
            "catalog exceeds the {MAX_CLINE_SESSIONS} session limit"
        )));
    }

    let mut leaves = Vec::with_capacity(catalog.len());
    for (session_id, entry) in catalog {
        leaves.push(bind_session_leaf(&authority, session_id, entry)?);
    }
    leaves.sort_by(|left, right| left.provider_session_id.cmp(&right.provider_session_id));

    let mut tree = Sha256::new();
    tree.update(TREE_DOMAIN);
    tree.update(authority.authority_fingerprint());
    tree.update(index.fence);
    tree.update(database.fence);
    tree.update((leaves.len() as u64).to_be_bytes());
    for leaf in &leaves {
        tree.update(leaf.fingerprint());
    }
    authority.revalidate_same_object()?;
    Ok(ClineSdkTreeSnapshot {
        authority,
        leaves,
        tree_fingerprint: tree.finalize().into(),
    })
}

fn read_index_catalog(authority: &ProviderSourceRoot) -> CatalogAttempt {
    let bytes = match read_optional_bytes(authority, Path::new(INDEX_PATH), MAX_INDEX_BYTES) {
        Ok(Some((bytes, _))) => bytes,
        Ok(None) => return CatalogAttempt::missing(INDEX_FENCE_DOMAIN),
        Err(error) => return CatalogAttempt::invalid(INDEX_FENCE_DOMAIN, None, error),
    };
    let evidence = Sha256::digest(&bytes).into();
    match parse_index_catalog(&bytes) {
        Ok(rows) => CatalogAttempt::valid(INDEX_FENCE_DOMAIN, evidence, rows),
        Err(error) => CatalogAttempt::invalid(INDEX_FENCE_DOMAIN, Some(evidence), error),
    }
}

fn parse_index_catalog(bytes: &[u8]) -> Result<BTreeMap<String, Value>> {
    let value: Value = serde_json::from_slice(bytes)?;
    validate_json_bounds(&value, 0, &mut 0)?;
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(ClineSdkError::Invalid(
            "sessions.index.json must have version 1".into(),
        ));
    }
    let sessions = value
        .get("sessions")
        .and_then(Value::as_object)
        .ok_or_else(|| ClineSdkError::Invalid("sessions.index.json is missing sessions".into()))?;
    if sessions.len() > MAX_CLINE_SESSIONS {
        return Err(ClineSdkError::Invalid(format!(
            "sessions.index.json exceeds the {MAX_CLINE_SESSIONS} session limit"
        )));
    }
    let mut rows = BTreeMap::new();
    for (key, row) in sessions {
        validate_session_id(key)?;
        let object = row.as_object().ok_or_else(|| {
            ClineSdkError::Invalid(format!("index row for {key:?} is not an object"))
        })?;
        if let Some(declared) = object.get("sessionId").and_then(Value::as_str) {
            if declared != key {
                return Err(ClineSdkError::Invalid(format!(
                    "index key {key:?} conflicts with sessionId {declared:?}"
                )));
            }
        }
        rows.insert(key.clone(), row.clone());
    }
    Ok(rows)
}

fn read_database_catalog(authority: &ProviderSourceRoot, data_root: &Path) -> CatalogAttempt {
    let main_evidence = match open_optional_evidence(authority, Path::new(DATABASE_PATH)) {
        Ok(Some(evidence)) => evidence,
        Ok(None) => return CatalogAttempt::missing(DATABASE_FENCE_DOMAIN),
        Err(error) => return CatalogAttempt::invalid(DATABASE_FENCE_DOMAIN, None, error),
    };
    let main_digest = file_evidence_digest(&main_evidence);
    let path = authority.named_path().join(DATABASE_PATH);
    let connection = match open_provider_sqlite_readonly(data_root, &path) {
        Ok(connection) => connection,
        Err(error) => {
            return CatalogAttempt::invalid(DATABASE_FENCE_DOMAIN, Some(main_digest), error.into())
        }
    };
    let query_result = query_database_rows(&connection);
    let finish_result = connection.finish();
    let revision = match finish_result {
        Ok(evidence) => database_evidence_digest(main_digest, *evidence.revision()),
        Err(error) => {
            return CatalogAttempt::invalid(DATABASE_FENCE_DOMAIN, Some(main_digest), error.into())
        }
    };
    match query_result {
        Ok(rows) => CatalogAttempt::valid(DATABASE_FENCE_DOMAIN, revision, rows),
        Err(error) => CatalogAttempt::invalid(DATABASE_FENCE_DOMAIN, Some(revision), error),
    }
}

fn query_database_rows(connection: &Connection) -> Result<BTreeMap<String, Value>> {
    let columns = sqlite_columns(connection, "sessions")?;
    if !columns.contains("session_id") {
        return Err(ClineSdkError::Invalid(
            "sessions.db sessions table is missing session_id".into(),
        ));
    }
    const OPTIONAL: &[&str] = &[
        "started_at",
        "updated_at",
        "provider",
        "model",
        "cwd",
        "workspace_root",
        "parent_session_id",
        "parent_agent_id",
        "agent_id",
        "conversation_id",
        "is_subagent",
        "messages_path",
    ];
    let expressions = OPTIONAL
        .iter()
        .map(|column| {
            if columns.contains(*column) {
                format!("CAST({column} AS TEXT)")
            } else {
                "NULL".to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let materialized_bytes = std::iter::once("session_id")
        .chain(OPTIONAL.iter().copied())
        .map(|column| {
            if columns.contains(column) {
                format!("COALESCE(octet_length(CAST({column} AS TEXT)), 0)")
            } else {
                "0".to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" + ");
    let sql = format!(
        "SELECT {materialized_bytes}, CAST(session_id AS TEXT), {expressions} FROM sessions ORDER BY session_id LIMIT {}",
        MAX_CLINE_SESSIONS + 1
    );
    let mut statement = connection.prepare(&sql)?;
    let mut query = statement.query([])?;
    let mut rows = BTreeMap::new();
    let mut retained_bytes = 0_usize;
    while let Some(row) = query.next()? {
        if rows.len() >= MAX_CLINE_SESSIONS {
            return Err(ClineSdkError::Invalid(format!(
                "sessions.db exceeds the {MAX_CLINE_SESSIONS} session limit"
            )));
        }
        let row_bytes = usize::try_from(row.get::<_, i64>(0)?).map_err(|_| {
            ClineSdkError::Invalid("sessions.db reported an invalid materialized byte count".into())
        })?;
        retained_bytes = retained_bytes
            .checked_add(row_bytes)
            .and_then(|value| value.checked_add(SQLITE_CATALOG_ROW_OVERHEAD_BYTES))
            .ok_or_else(|| {
                ClineSdkError::Invalid("sessions.db materialized byte count overflowed".into())
            })?;
        if retained_bytes > MAX_SQLITE_CATALOG_MATERIALIZED_BYTES {
            return Err(ClineSdkError::Invalid(format!(
                "sessions.db exceeds the {MAX_SQLITE_CATALOG_MATERIALIZED_BYTES} byte aggregate materialization limit"
            )));
        }
        let session_id: String = row.get(1)?;
        validate_session_id(&session_id)?;
        if rows.contains_key(&session_id) {
            return Err(ClineSdkError::Invalid(format!(
                "sessions.db contains duplicate session_id {session_id:?}"
            )));
        }
        let mut object = Map::new();
        object.insert("session_id".into(), Value::String(session_id.clone()));
        for (offset, column) in OPTIONAL.iter().enumerate() {
            let value: Option<String> = row.get(offset + 2)?;
            if let Some(value) = value {
                object.insert((*column).into(), Value::String(value));
            }
        }
        rows.insert(session_id, Value::Object(object));
    }
    Ok(rows)
}

fn sqlite_columns(connection: &Connection, table: &str) -> Result<BTreeSet<String>> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    if names.is_empty() {
        return Err(ClineSdkError::Invalid(format!(
            "sessions.db has no {table} table"
        )));
    }
    Ok(names)
}

fn bind_session_leaf(
    authority: &ProviderSourceRoot,
    session_id: String,
    entry: CatalogEntry,
) -> Result<SessionLeaf> {
    validate_session_id(&session_id)?;
    let manifest_relative = PathBuf::from("sessions")
        .join(&session_id)
        .join(format!("{session_id}.json"));
    let manifest_read = read_optional_bytes(authority, &manifest_relative, MAX_MANIFEST_BYTES)?;
    let manifest = manifest_read.as_ref().map(|(_, evidence)| evidence.clone());
    let (manifest_value, malformed_manifest) = match manifest_read.as_ref() {
        Some((bytes, _)) => match serde_json::from_slice::<Value>(bytes) {
            Ok(value)
                if validate_json_bounds(&value, 0, &mut 0).is_ok()
                    && manifest_session_matches(&value, &session_id) =>
            {
                (Some(value), false)
            }
            _ => (None, true),
        },
        None => (None, false),
    };

    let mut metadata = metadata_from_catalog(entry.index_row.as_ref(), false);
    overlay_metadata(
        &mut metadata,
        metadata_from_catalog(entry.database_row.as_ref(), true),
    );
    overlay_manifest_metadata(&mut metadata, manifest_value.as_ref());
    let catalog_messages_path = string_field(entry.database_row.as_ref(), "messages_path")
        .or_else(|| string_field(entry.index_row.as_ref(), "messagesPath"));
    let messages_path = catalog_messages_path
        .clone()
        .or_else(|| string_field(manifest_value.as_ref(), "messages_path"));
    let messages_binding = normalize_messages_path(
        authority.named_path(),
        &session_id,
        messages_path.as_deref(),
    );
    // The catalog key already established exact source ownership. Isolate only
    // that row's invalid path binding; manifest fallbacks and artifact I/O keep
    // their existing route-fatal behavior.
    let (messages_relative_path, messages, catalog_binding_failure) = match messages_binding {
        Ok(messages_relative_path) => {
            let messages = open_optional_evidence(authority, &messages_relative_path)?;
            (messages_relative_path, messages, None)
        }
        Err(ClineSdkError::Invalid(detail)) if catalog_messages_path.is_some() => (
            canonical_messages_path(&session_id),
            None,
            Some(format!(
                "Cline SDK catalog row {session_id:?} has an invalid messages path: {detail}"
            )),
        ),
        Err(error) => return Err(error),
    };

    let mut catalog_digest = Sha256::new();
    catalog_digest.update(b"ctx.cline.sdk.catalog-evidence.v1\0");
    hash_optional_json(&mut catalog_digest, entry.index_row.as_ref());
    hash_optional_json(&mut catalog_digest, entry.database_row.as_ref());
    metadata.index_row = entry.index_row;
    metadata.database_row = entry.database_row;
    metadata.manifest = manifest_value;
    metadata.malformed_manifest = malformed_manifest;
    let source_key_digest = Sha256::digest(session_id.as_bytes()).into();
    Ok(SessionLeaf {
        provider_session_id: session_id,
        source_key_digest,
        catalog_evidence: catalog_digest.finalize().into(),
        catalog_binding_failure,
        manifest_relative_path: manifest_relative,
        manifest,
        messages,
        messages_relative_path,
        metadata,
    })
}

fn metadata_from_catalog(value: Option<&Value>, database: bool) -> SessionMetadata {
    let field =
        |snake: &str, camel: &str| string_field(value, if database { snake } else { camel });
    SessionMetadata {
        model: field("model", "model"),
        provider: field("provider", "provider"),
        cwd: field("cwd", "cwd"),
        workspace_root: field("workspace_root", "workspaceRoot"),
        parent_session_id: field("parent_session_id", "parentSessionId"),
        parent_agent_id: field("parent_agent_id", "parentAgentId"),
        agent_id: field("agent_id", "agentId"),
        conversation_id: field("conversation_id", "conversationId"),
        is_subagent: string_field(
            value,
            if database {
                "is_subagent"
            } else {
                "isSubagent"
            },
        )
        .and_then(|value| parse_bool(&value))
        .or_else(|| {
            value
                .and_then(|value| {
                    value.get(if database {
                        "is_subagent"
                    } else {
                        "isSubagent"
                    })
                })
                .and_then(Value::as_bool)
        }),
        started_at: field("started_at", "startedAt"),
        updated_at: field("updated_at", "updatedAt"),
        ..SessionMetadata::default()
    }
}

fn overlay_metadata(target: &mut SessionMetadata, source: SessionMetadata) {
    macro_rules! overlay {
        ($field:ident) => {
            if source.$field.is_some() {
                target.$field = source.$field;
            }
        };
    }
    overlay!(model);
    overlay!(provider);
    overlay!(cwd);
    overlay!(workspace_root);
    overlay!(parent_session_id);
    overlay!(parent_agent_id);
    overlay!(agent_id);
    overlay!(conversation_id);
    overlay!(is_subagent);
    overlay!(started_at);
    overlay!(updated_at);
}

fn overlay_manifest_metadata(metadata: &mut SessionMetadata, manifest: Option<&Value>) {
    let Some(manifest) = manifest else { return };
    macro_rules! fallback {
        ($field:ident, $name:literal) => {
            if metadata.$field.is_none() {
                metadata.$field = string_field(Some(manifest), $name);
            }
        };
    }
    fallback!(model, "model");
    fallback!(provider, "provider");
    fallback!(cwd, "cwd");
    fallback!(workspace_root, "workspace_root");
    fallback!(started_at, "started_at");
    if metadata.parent_session_id.is_none() {
        metadata.parent_session_id = manifest
            .pointer("/metadata/fork/forkedFromSessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        metadata.fork_parent = metadata.parent_session_id.is_some();
    }
}

fn manifest_session_matches(value: &Value, session_id: &str) -> bool {
    value
        .get("session_id")
        .and_then(Value::as_str)
        .is_none_or(|declared| declared == session_id)
}

fn normalize_messages_path(root: &Path, session_id: &str, raw: Option<&str>) -> Result<PathBuf> {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Ok(canonical_messages_path(session_id));
    };
    if raw.len() > MAX_PATH_BYTES {
        return Err(ClineSdkError::Invalid("messages_path is too long".into()));
    }
    let path = Path::new(raw);
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .map(Path::to_path_buf)
            .map_err(|_| {
                ClineSdkError::Invalid("absolute messages_path escapes the data root".into())
            })?
    } else {
        let mut components = path.components();
        match components.next() {
            Some(Component::Normal(first)) if first == "sessions" => path.to_path_buf(),
            Some(Component::Normal(first)) if first == session_id => {
                PathBuf::from("sessions").join(path)
            }
            Some(Component::Normal(_)) if path.components().count() == 1 => {
                PathBuf::from("sessions").join(session_id).join(path)
            }
            _ => {
                return Err(ClineSdkError::Invalid(
                    "relative messages_path is not session-scoped".into(),
                ))
            }
        }
    };
    if relative.as_os_str().len() > MAX_PATH_BYTES
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(ClineSdkError::Invalid(
            "messages_path contains unsafe traversal".into(),
        ));
    }
    Ok(relative)
}

fn canonical_messages_path(session_id: &str) -> PathBuf {
    PathBuf::from("sessions")
        .join(session_id)
        .join(format!("{session_id}.messages.json"))
}

pub(super) fn read_bound_leaf_files(
    authority: &ProviderSourceRoot,
    leaf: &SessionLeaf,
) -> Result<BoundLeafFiles> {
    let manifest = read_expected_file(authority, leaf.manifest.as_ref(), MAX_MANIFEST_BYTES)?;
    let messages = read_expected_file(authority, leaf.messages.as_ref(), MAX_MESSAGES_BYTES)?;
    if leaf.manifest.is_none()
        && open_optional_evidence(authority, &leaf.manifest_relative_path)?.is_some()
    {
        return Err(ClineSdkError::SourceChanged);
    }
    if leaf.messages.is_none()
        && open_optional_evidence(authority, &leaf.messages_relative_path)?.is_some()
    {
        return Err(ClineSdkError::SourceChanged);
    }
    authority.revalidate_same_object()?;
    Ok((manifest, messages))
}

fn read_expected_file(
    authority: &ProviderSourceRoot,
    expected: Option<&FileEvidence>,
    maximum: usize,
) -> Result<Option<Vec<u8>>> {
    let Some(expected) = expected else {
        return Ok(None);
    };
    let file = authority.open_file(&expected.relative_path)?;
    let current = FileEvidence::from_open(expected.relative_path.clone(), &file);
    if &current != expected {
        return Err(ClineSdkError::SourceChanged);
    }
    let bytes = file.read_all_bounded(maximum)?;
    if u64::try_from(bytes.len()).ok() != Some(expected.length) {
        return Err(ClineSdkError::SourceChanged);
    }
    Ok(Some(bytes))
}

fn read_optional_bytes(
    authority: &ProviderSourceRoot,
    relative: &Path,
    maximum: usize,
) -> Result<Option<(Vec<u8>, FileEvidence)>> {
    let Some(file) = open_optional_file(authority, relative)? else {
        return Ok(None);
    };
    let evidence = FileEvidence::from_open(relative.to_path_buf(), &file);
    let bytes = file.read_all_bounded(maximum)?;
    Ok(Some((bytes, evidence)))
}

fn open_optional_evidence(
    authority: &ProviderSourceRoot,
    relative: &Path,
) -> Result<Option<FileEvidence>> {
    Ok(open_optional_file(authority, relative)?
        .as_ref()
        .map(|file| FileEvidence::from_open(relative.to_path_buf(), file)))
}

fn open_optional_file(
    authority: &ProviderSourceRoot,
    relative: &Path,
) -> Result<Option<OpenedProviderSourceFile>> {
    match authority.open_file(relative) {
        Ok(file) => Ok(Some(file)),
        Err(SourceIoError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.len() > 512 || !provider_safe_path_segment(session_id) {
        return Err(ClineSdkError::Invalid(format!(
            "unsafe provider session_id {session_id:?}"
        )));
    }
    Ok(())
}

fn string_field(value: Option<&Value>, name: &str) -> Option<String> {
    let value = value?.get(name)?;
    match value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

pub(super) fn validate_json_bounds(value: &Value, depth: usize, seen: &mut usize) -> Result<()> {
    const MAX_DEPTH: usize = 128;
    const MAX_ELEMENTS: usize = 262_144;
    *seen = seen.saturating_add(1);
    if depth > MAX_DEPTH || *seen > MAX_ELEMENTS {
        return Err(ClineSdkError::Invalid(
            "JSON exceeds its depth or element budget".into(),
        ));
    }
    match value {
        Value::Array(values) => {
            for value in values {
                validate_json_bounds(value, depth + 1, seen)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_json_bounds(value, depth + 1, seen)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn hash_optional_file(digest: &mut Sha256, file: Option<&FileEvidence>) {
    match file {
        Some(file) => {
            digest.update([1]);
            file.hash_into(digest);
        }
        None => digest.update([0]),
    }
}

fn hash_optional_digest(digest: &mut Sha256, value: Option<[u8; 32]>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value);
        }
        None => digest.update([0]),
    }
}

fn catalog_fence(domain: &[u8], state: u8, evidence: Option<[u8; 32]>) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update([state]);
    hash_optional_digest(&mut digest, evidence);
    digest.finalize().into()
}

fn file_evidence_digest(evidence: &FileEvidence) -> [u8; 32] {
    let mut digest = Sha256::new();
    evidence.hash_into(&mut digest);
    digest.finalize().into()
}

fn database_evidence_digest(main: [u8; 32], revision: [u8; 32]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(main);
    digest.update(revision);
    digest.finalize().into()
}

fn hash_optional_json(digest: &mut Sha256, value: Option<&Value>) {
    match value {
        Some(value) => {
            digest.update([1]);
            let encoded = serde_json::to_vec(value).unwrap_or_default();
            digest.update((encoded.len() as u64).to_be_bytes());
            digest.update(encoded);
        }
        None => digest.update([0]),
    }
}

fn hash_path(digest: &mut Sha256, path: &Path) {
    let bytes = path.as_os_str().as_encoded_bytes();
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn hash_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}
