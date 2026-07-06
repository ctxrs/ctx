#[allow(unused_imports)]
use super::*;

pub(crate) const DEEPAGENTS_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".deepagents", ".state", "sessions.db"],
    source_format: "deepagents_sessions_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

pub(crate) fn has_deepagents_checkpoint_tables(path: &Path) -> BoundedProbe {
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
            "select count(*) from sqlite_schema \
             where type = 'table' and name in ('checkpoints', 'writes')",
            [],
            |row| row.get::<_, i64>(0),
        )
    }) {
        Ok(2) => BoundedProbe::Found,
        Ok(_) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}
