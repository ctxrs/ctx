#[allow(unused_imports)]
use super::*;

pub(crate) const KILO_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".local", "share", "kilo", "kilo.db"],
    source_format: "kilo_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

pub(crate) fn discover_provider_sources_for_spec(
    home: &Path,
    spec: &ProviderSourceSpec,
) -> Vec<ProviderSource> {
    if spec.provider == CaptureProvider::Kilo {
        return discover_kilo_sources(home, spec);
    }
    if spec.provider == CaptureProvider::ForgeCode {
        return discover_forgecode_sources(home, spec);
    }

    let mut sources = spec
        .default_locations
        .iter()
        .map(|location| {
            let path = location
                .path_components
                .iter()
                .fold(home.to_path_buf(), |path, component| path.join(component));
            let mut source = provider_source_from_location(spec, location, path);
            if spec.provider == CaptureProvider::Trae {
                source.import_support = ProviderImportSupport::Native;
            }
            source
        })
        .collect::<Vec<_>>();

    match spec.provider {
        CaptureProvider::OpenClaw => {
            if let Some(path) = env_path("OPENCLAW_STATE_DIR") {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "openclaw_session_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Pi => {
            sources.extend(discover_pi_custom_session_sources(home, spec));
        }
        CaptureProvider::Crush => {
            if let Some(path) = env_path("CRUSH_GLOBAL_DATA") {
                sources.push(crush_db_source(spec, path.join("crush.db")));
            }
            if let Some(path) = env_path("XDG_DATA_HOME") {
                sources.push(crush_db_source(spec, path.join("crush").join("crush.db")));
            }
            for config_path in crush_config_paths(home) {
                if let Some(data_dir) = crush_config_data_dir(&config_path, home) {
                    let relative_base = config_path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| home.to_path_buf());
                    let data_dir =
                        resolve_pi_config_path(&data_dir.to_string_lossy(), home, &relative_base);
                    sources.push(crush_db_source(spec, data_dir.join("crush.db")));
                }
            }
            for root in current_dir_ancestors_with(|candidate| {
                candidate.join(".crush").join("crush.db").is_file()
                    || candidate.join("crush.json").is_file()
                    || candidate.join(".crush.json").is_file()
            }) {
                sources.push(crush_db_source(spec, root.join(".crush").join("crush.db")));
                for config_name in ["crush.json", ".crush.json"] {
                    let config_path = root.join(config_name);
                    if let Some(data_dir) = crush_config_data_dir(&config_path, home) {
                        let data_dir =
                            resolve_pi_config_path(&data_dir.to_string_lossy(), home, &root);
                        sources.push(crush_db_source(spec, data_dir.join("crush.db")));
                    }
                }
            }
        }
        CaptureProvider::KiroCli => {
            if let Some(path) = env_path("XDG_DATA_HOME") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("kiro-cli").join("data.sqlite3"),
                    "kiro_cli_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Goose => {
            if let Some(path) = env_path("GOOSE_PATH_ROOT") {
                sources.push(goose_db_source(
                    spec,
                    path.join("data").join("sessions").join("sessions.db"),
                ));
            }
            if let Some(path) = env_path("XDG_DATA_HOME") {
                sources.push(goose_db_source(
                    spec,
                    path.join("goose").join("sessions").join("sessions.db"),
                ));
                sources.push(goose_db_source(
                    spec,
                    path.join("Block")
                        .join("goose")
                        .join("sessions")
                        .join("sessions.db"),
                ));
            }
        }
        CaptureProvider::Zed => {
            if let Some(path) = env_path("XDG_DATA_HOME") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("zed").join("threads").join("threads.db"),
                    "zed_threads_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Hermes => {
            if let Some(path) = env_path("HERMES_HOME") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("state.db"),
                    "hermes_state_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::QwenCode => {
            if let Some(path) = env_path_resolved("QWEN_RUNTIME_DIR", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("projects"),
                    "qwen_code_chat_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
            if let Some(path) = env_path_resolved("QWEN_HOME", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("projects"),
                    "qwen_code_chat_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::KimiCodeCli => {
            if let Some(path) = env_path_resolved("KIMI_CODE_HOME", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "kimi_code_cli_wire_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Auggie => {}
        CaptureProvider::Junie => {
            if let Some(path) = env_path_resolved("JUNIE_SESSIONS_DIR", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "junie_session_events_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
            if let Some(path) = env_path_resolved("JUNIE_HOME", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("sessions"),
                    "junie_session_events_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Firebender => {
            for root in current_dir_ancestors_with(|candidate| {
                candidate
                    .join(".idea")
                    .join("firebender")
                    .join("chat_history.db")
                    .is_file()
            }) {
                sources.push(provider_source_from_parts(
                    spec,
                    root.join(".idea")
                        .join("firebender")
                        .join("chat_history.db"),
                    "firebender_chat_history_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::MistralVibe => {
            if let Some(path) = env_path_resolved("VIBE_HOME", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("logs").join("session"),
                    "mistral_vibe_session_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Mux => {
            if let Some(path) = env_path_resolved("MUX_ROOT", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("sessions"),
                    "mux_session_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::NanoClaw => {
            for root in current_dir_ancestors_with(|candidate| {
                candidate.join("data").join("v2.db").is_file()
                    && candidate.join("data").join("v2-sessions").is_dir()
            }) {
                sources.push(provider_source_from_parts(
                    spec,
                    root,
                    "nanoclaw_project",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::AstrBot => {
            if let Some(path) = env_path("ASTRBOT_ROOT") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("data").join("data_v4.db"),
                    "astrbot_data_v4_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
            for root in current_dir_ancestors_with(|candidate| {
                candidate.join("data").join("data_v4.db").is_file()
            }) {
                sources.push(provider_source_from_parts(
                    spec,
                    root.join("data").join("data_v4.db"),
                    "astrbot_data_v4_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Shelley => {
            if let Some(path) = env_path("SHELLEY_DB") {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "shelley_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Continue => {
            if let Some(path) = env_path("CONTINUE_GLOBAL_DIR") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("sessions"),
                    "continue_cli_sessions_json",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::OpenHands => {
            if let Some(path) = env_path("OH_PERSISTENCE_DIR") {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "openhands_file_events",
                    ProviderSourceKind::NativeHistory,
                ));
            }
            if let Some(path) = env_path("FILE_STORE_PATH") {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "openhands_file_events",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Cline => {
            sources.extend(discover_cline_task_json_sources(home, spec));
        }
        CaptureProvider::RooCode => {
            sources.extend(discover_roo_task_json_sources(home, spec));
        }
        CaptureProvider::Warp => {
            if let Some(path) = env_path("XDG_STATE_HOME") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("warp-terminal").join("warp.sqlite"),
                    "warp_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
            if let Some(path) = env_path("LOCALAPPDATA") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("warp")
                        .join("Warp")
                        .join("data")
                        .join("warp.sqlite"),
                    "warp_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::CodeBuddy => {
            if let Some(path) = env_path("LOCALAPPDATA") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("CodeBuddyExtension"),
                    "codebuddy_history_json",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Trae => {
            if let Some(path) = env_path("APPDATA") {
                sources.push(trae_workspace_storage_source(
                    spec,
                    path.join("Trae").join("User").join("workspaceStorage"),
                ));
                sources.push(trae_workspace_storage_source(
                    spec,
                    path.join("Trae CN").join("User").join("workspaceStorage"),
                ));
            }
        }
        _ => {}
    }

    sources
}

pub(crate) fn discover_kilo_sources(home: &Path, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
    if let Some(raw) = env::var_os("KILO_DB").filter(|value| !value.is_empty()) {
        if raw.to_string_lossy().trim() == ":memory:" {
            return Vec::new();
        }
        return vec![provider_source_from_parts(
            spec,
            resolve_kilo_db_path(PathBuf::from(raw), home),
            "kilo_sqlite",
            ProviderSourceKind::NativeHistory,
        )];
    }

    let data_dir = kilo_data_dir(home);
    let mut sources = vec![provider_source_from_parts(
        spec,
        data_dir.join("kilo.db"),
        "kilo_sqlite",
        ProviderSourceKind::NativeHistory,
    )];

    if !env_truthy("KILO_DISABLE_CHANNEL_DB") {
        sources.extend(kilo_channel_db_paths(&data_dir).into_iter().map(|path| {
            provider_source_from_parts(spec, path, "kilo_sqlite", ProviderSourceKind::NativeHistory)
        }));
    }

    sources
}

pub(crate) fn resolve_kilo_db_path(path: PathBuf, home: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        kilo_data_dir(home).join(path)
    }
}

pub(crate) fn kilo_data_dir(home: &Path) -> PathBuf {
    env_path("XDG_DATA_HOME")
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join("kilo")
}

pub(crate) fn kilo_channel_db_paths(data_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(entries) = fs::read_dir(data_dir) else {
        return paths;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|file_type| file_type.is_file()) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("kilo-") && name.ends_with(".db") {
            paths.push(path);
        }
    }
    paths.sort();
    paths
}
