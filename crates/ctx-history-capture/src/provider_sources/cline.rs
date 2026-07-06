#[allow(unused_imports)]
use super::*;

pub(crate) const CLINE_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".cline", "data"],
        source_format: "cline_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code",
            "User",
            "globalStorage",
            "saoudrizwan.claude-dev",
        ],
        source_format: "cline_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code - Insiders",
            "User",
            "globalStorage",
            "saoudrizwan.claude-dev",
        ],
        source_format: "cline_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

pub(crate) const ROO_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code",
            "User",
            "globalStorage",
            "rooveterinaryinc.roo-cline",
        ],
        source_format: "roo_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code",
            "User",
            "globalStorage",
            "RooVeterinaryInc.roo-cline",
        ],
        source_format: "roo_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code - Insiders",
            "User",
            "globalStorage",
            "rooveterinaryinc.roo-cline",
        ],
        source_format: "roo_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

pub(crate) fn discover_cline_task_json_sources(
    home: &Path,
    spec: &ProviderSourceSpec,
) -> Vec<ProviderSource> {
    let mut sources = Vec::new();
    if let Some(path) = env_path_with_home("CLINE_DATA_DIR", home) {
        sources.push(task_json_source(spec, path));
    }
    if let Some(path) = env_path_with_home("CLINE_DIR", home) {
        sources.push(task_json_source(spec, path.join("data")));
    }
    if let Some(path) = env_path_with_home("CLINE_SESSION_DATA_DIR", home) {
        sources.push(task_json_source(spec, path.clone()));
        if let Some(parent) = path.parent() {
            sources.push(task_json_source(spec, parent.to_path_buf()));
        }
    }
    if let Some(path) = env_path_with_home("CLINE_DB_DATA_DIR", home) {
        if let Some(parent) = path.parent() {
            sources.push(task_json_source(spec, parent.to_path_buf()));
        } else {
            sources.push(task_json_source(spec, path));
        }
    }
    sources
}

pub(crate) fn discover_roo_task_json_sources(
    home: &Path,
    spec: &ProviderSourceSpec,
) -> Vec<ProviderSource> {
    let mut sources = Vec::new();
    for env_name in ["ROO_CODE_DATA_DIR", "ROO_DATA_DIR", "ROO_CLINE_DATA_DIR"] {
        if let Some(path) = env_path_with_home(env_name, home) {
            sources.push(task_json_source(spec, path));
        }
    }
    for settings_path in vscode_settings_paths(home) {
        if let Some(path) = roo_custom_storage_path(&settings_path, home) {
            sources.push(task_json_source(spec, path));
        }
    }
    sources
}

pub(crate) fn task_json_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        match spec.provider {
            CaptureProvider::RooCode => "roo_task_directory_json",
            _ => "cline_task_directory_json",
        },
        ProviderSourceKind::NativeHistory,
    )
}

pub(crate) fn roo_custom_storage_path(settings_path: &Path, home: &Path) -> Option<PathBuf> {
    let settings = fs::read_to_string(settings_path).ok()?;
    let value: Value = serde_json::from_str(&settings).ok()?;
    let path = value
        .get("roo-cline.customStoragePath")
        .or_else(|| value.pointer("/roo-cline/customStoragePath"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    let relative_base = settings_path.parent().unwrap_or(home);
    Some(resolve_pi_config_path(path, home, relative_base))
}
