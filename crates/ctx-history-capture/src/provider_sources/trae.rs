#[allow(unused_imports)]
use super::*;

pub(crate) const TRAE_STATE_VSCDB_SOURCE_FORMAT: &str = "trae_state_vscdb";

pub(crate) const TRAE_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "Trae",
            "User",
            "workspaceStorage",
        ],
        source_format: TRAE_STATE_VSCDB_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "Trae CN",
            "User",
            "workspaceStorage",
        ],
        source_format: TRAE_STATE_VSCDB_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

pub(crate) fn trae_workspace_storage_source(
    spec: &ProviderSourceSpec,
    path: PathBuf,
) -> ProviderSource {
    let mut source = provider_source_from_parts(
        spec,
        path,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        ProviderSourceKind::NativeHistory,
    );
    source.import_support = ProviderImportSupport::Native;
    source
}

pub(crate) fn has_trae_state_vscdb_chat_history(root: &Path, max_entries: usize) -> BoundedProbe {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => return BoundedProbe::NotFound,
        Ok(metadata) if metadata.is_file() => {
            if root.file_name().and_then(|name| name.to_str()) != Some("state.vscdb") {
                return BoundedProbe::NotFound;
            }
            return has_trae_state_vscdb_chat_keys(root);
        }
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return BoundedProbe::NotFound,
        Err(err) if err.kind() == ErrorKind::NotFound => return BoundedProbe::NotFound,
        Err(_) => return BoundedProbe::IoError,
    }

    let direct = root.join("state.vscdb");
    if direct.is_file() {
        return has_trae_state_vscdb_chat_keys(&direct);
    }

    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return BoundedProbe::IoError,
    };
    let mut visited = 0usize;
    let mut saw_io_error = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                saw_io_error = true;
                continue;
            }
        };
        visited = visited.saturating_add(1);
        if visited > max_entries {
            return BoundedProbe::BudgetExhausted;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                saw_io_error = true;
                continue;
            }
        };
        if !file_type.is_dir() {
            continue;
        }
        let candidate = entry.path().join("state.vscdb");
        if !candidate.is_file() {
            continue;
        }
        match has_trae_state_vscdb_chat_keys(&candidate) {
            BoundedProbe::Found => return BoundedProbe::Found,
            BoundedProbe::IoError => saw_io_error = true,
            BoundedProbe::NotFound | BoundedProbe::BudgetExhausted => {}
        }
    }

    if saw_io_error {
        BoundedProbe::IoError
    } else {
        BoundedProbe::NotFound
    }
}

pub(crate) fn has_trae_state_vscdb_chat_keys(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        let (table_count, column_count) = conn.query_row(
            "select \
                (select count(*) from sqlite_schema where type = 'table' and name = 'ItemTable'), \
                (select count(*) from pragma_table_info('ItemTable') where name in ('key', 'value'))",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        if table_count != 1 || column_count < 2 {
            return Ok(false);
        }

        let key_count = conn.query_row(
            "select count(*) from ItemTable \
             where [key] in (
                'memento/icube-ai-agent-storage',
                'icube-ai-agent-storage-input-history',
                'chat.ChatSessionStore.index',
                'ChatStore',
                'memento/icube-ai-chat-storage-7467774676505887760',
                'memento/icube-ai-ng-chat-storage-7467774676505887760'
             ) and length(trim(cast(coalesce(value, '') as text))) > 0",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(key_count > 0)
    }) {
        Ok(true) => BoundedProbe::Found,
        Ok(false) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}
