#[allow(unused_imports)]
use super::*;

pub(crate) const PI_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".pi", "agent", "sessions"],
        source_format: "pi_session_jsonl",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[".omp", "agent", "sessions"],
        source_format: "pi_session_jsonl",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

pub(crate) fn discover_pi_custom_session_sources(
    home: &Path,
    spec: &ProviderSourceSpec,
) -> Vec<ProviderSource> {
    let project_settings_dirs = env::current_dir()
        .ok()
        .map(|current_dir| {
            current_dir
                .ancestors()
                .map(|candidate| candidate.join(".pi"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    discover_pi_custom_session_sources_with_project_settings(home, spec, &project_settings_dirs)
}

pub(crate) fn discover_pi_custom_session_sources_with_project_settings(
    home: &Path,
    spec: &ProviderSourceSpec,
    project_settings_dirs: &[PathBuf],
) -> Vec<ProviderSource> {
    let mut sources = Vec::new();
    if let Some(path) = env_path_with_home("PI_CODING_AGENT_SESSION_DIR", home) {
        sources.push(pi_session_source(spec, path));
    }

    let agent_dir = pi_agent_dir(home);
    if let Some(path) = pi_settings_session_dir(&agent_dir.join("settings.json"), home, &agent_dir)
    {
        sources.push(pi_session_source(spec, path));
    }

    for project_settings_dir in project_settings_dirs {
        if let Some(path) = pi_settings_session_dir(
            &project_settings_dir.join("settings.json"),
            home,
            project_settings_dir,
        ) {
            sources.push(pi_session_source(spec, path));
        }
    }

    sources
}

pub(crate) fn pi_session_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "pi_session_jsonl",
        ProviderSourceKind::NativeHistory,
    )
}

pub(crate) fn pi_agent_dir(home: &Path) -> PathBuf {
    env_path_with_home("PI_CODING_AGENT_DIR", home).unwrap_or_else(|| home.join(".pi/agent"))
}

pub(crate) fn pi_settings_session_dir(
    settings_path: &Path,
    home: &Path,
    relative_base: &Path,
) -> Option<PathBuf> {
    let settings = fs::read_to_string(settings_path).ok()?;
    let value: Value = serde_json::from_str(&settings).ok()?;
    let session_dir = value
        .get("sessionDir")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    Some(resolve_pi_config_path(session_dir, home, relative_base))
}

pub(crate) fn resolve_pi_config_path(value: &str, home: &Path, relative_base: &Path) -> PathBuf {
    resolve_home_relative_path(value, home, relative_base)
}
