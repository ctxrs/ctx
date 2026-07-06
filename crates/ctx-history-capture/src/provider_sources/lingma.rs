#[allow(unused_imports)]
use super::*;

pub(crate) const LINGMA_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[
            ".lingma",
            "vscode",
            "sharedClientCache",
            "cache",
            "db",
            "local.db",
        ],
        source_format: "lingma_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".lingma",
            "vscode-insiders",
            "sharedClientCache",
            "cache",
            "db",
            "local.db",
        ],
        source_format: "lingma_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

pub(crate) fn has_lingma_chat_record_table(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.query_row(
            "select count(*) from pragma_table_info('chat_record') \
             where name in ('session_id', 'request_id', 'chat_prompt', 'summary', \
                            'error_result', 'gmt_create', 'extra')",
            [],
            |row| row.get::<_, i64>(0),
        )
    }) {
        Ok(count) if count >= 7 => BoundedProbe::Found,
        Ok(_) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}
