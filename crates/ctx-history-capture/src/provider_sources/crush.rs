#[allow(unused_imports)]
use super::*;

pub(crate) const CRUSH_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".local", "share", "crush", "crush.db"],
    source_format: "crush_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

pub(crate) fn crush_db_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "crush_sqlite",
        ProviderSourceKind::NativeHistory,
    )
}

pub(crate) fn crush_config_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = env_path("CRUSH_GLOBAL_CONFIG") {
        paths.push(path);
    }
    paths.push(home.join(".config").join("crush").join("crush.json"));
    paths
}

pub(crate) fn crush_config_data_dir(config_path: &Path, home: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(config_path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let raw = value
        .pointer("/options/data_directory")
        .or_else(|| value.pointer("/options/dataDirectory"))
        .or_else(|| value.get("data_directory"))
        .or_else(|| value.get("dataDirectory"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    let relative_base = config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.to_path_buf());
    Some(resolve_pi_config_path(raw, home, &relative_base))
}
