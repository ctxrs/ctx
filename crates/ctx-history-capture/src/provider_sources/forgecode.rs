#[allow(unused_imports)]
use super::*;

pub(crate) const FORGECODE_DEFAULTS: &[ProviderDefaultLocation] = &[];

pub(crate) fn discover_forgecode_sources(
    home: &Path,
    spec: &ProviderSourceSpec,
) -> Vec<ProviderSource> {
    if let Some(path) = env_path_with_home("FORGE_CONFIG", home) {
        return vec![forgecode_db_source(spec, path.join(".forge.db"))];
    }

    let legacy = home.join("forge");
    let base = if legacy.try_exists().unwrap_or(false) {
        legacy
    } else {
        home.join(".forge")
    };
    vec![forgecode_db_source(spec, base.join(".forge.db"))]
}

pub(crate) fn forgecode_db_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "forgecode_sqlite",
        ProviderSourceKind::NativeHistory,
    )
}

pub(crate) fn has_forgecode_conversations_table(path: &Path) -> BoundedProbe {
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
            "select count(*) from sqlite_schema where type = 'table' and name = 'conversations'",
            [],
            |row| row.get::<_, i64>(0),
        )
    }) {
        Ok(count) if count > 0 => BoundedProbe::Found,
        Ok(_) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}
