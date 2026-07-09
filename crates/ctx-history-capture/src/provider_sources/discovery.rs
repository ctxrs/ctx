use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use serde_json::Value;

use super::{
    probes::{default_location_import_probe, BoundedProbe},
    reasons::{empty_source_reason, probe_io_error_reason, unknown_source_reason},
    specs::{provider_source_spec, PROVIDER_SPECS, TRAE_STATE_VSCDB_SOURCE_FORMAT},
    types::{
        ProviderCatalogSupport, ProviderDefaultLocation, ProviderImportSupport, ProviderSource,
        ProviderSourceKind, ProviderSourceSpec, ProviderSourceStatus,
    },
};

pub fn discover_provider_sources(home: &Path) -> Vec<ProviderSource> {
    dedupe_sources(
        PROVIDER_SPECS
            .iter()
            .flat_map(|spec| discover_provider_sources_for_spec(home, spec))
            .collect(),
    )
}

pub fn discover_provider_sources_for_provider(
    home: &Path,
    provider: CaptureProvider,
) -> Vec<ProviderSource> {
    dedupe_sources(
        PROVIDER_SPECS
            .iter()
            .filter(|spec| spec.provider == provider)
            .flat_map(|spec| discover_provider_sources_for_spec(home, spec))
            .collect(),
    )
}

fn discover_provider_sources_for_spec(
    home: &Path,
    spec: &ProviderSourceSpec,
) -> Vec<ProviderSource> {
    if spec.provider == CaptureProvider::Kilo {
        return discover_kilo_sources(home, spec);
    }
    if spec.provider == CaptureProvider::MiMoCode {
        return discover_mimocode_sources(home, spec);
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

fn discover_forgecode_sources(home: &Path, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
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

fn forgecode_db_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "forgecode_sqlite",
        ProviderSourceKind::NativeHistory,
    )
}

fn discover_kilo_sources(home: &Path, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
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

fn resolve_kilo_db_path(path: PathBuf, home: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        kilo_data_dir(home).join(path)
    }
}

fn kilo_data_dir(home: &Path) -> PathBuf {
    env_path("XDG_DATA_HOME")
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join("kilo")
}

fn kilo_channel_db_paths(data_dir: &Path) -> Vec<PathBuf> {
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

fn discover_mimocode_sources(home: &Path, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
    if let Some(raw) = env::var_os("MIMOCODE_DB").filter(|value| !value.is_empty()) {
        if raw.to_string_lossy().trim() == ":memory:" {
            return Vec::new();
        }
        return vec![mimocode_db_source(
            spec,
            resolve_mimocode_db_path(PathBuf::from(raw), home),
        )];
    }

    let data_dir = mimocode_data_dir(home);
    let mut sources = vec![mimocode_db_source(spec, data_dir.join("mimocode.db"))];

    if !env_truthy("MIMOCODE_DISABLE_CHANNEL_DB") {
        sources.extend(
            mimocode_channel_db_paths(&data_dir)
                .into_iter()
                .map(|path| mimocode_db_source(spec, path)),
        );
    }

    sources
}

fn resolve_mimocode_db_path(path: PathBuf, home: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        mimocode_data_dir(home).join(path)
    }
}

fn mimocode_data_dir(home: &Path) -> PathBuf {
    if let Some(path) = env_path_with_home("MIMOCODE_HOME", home) {
        path.join("data")
    } else {
        env_path("XDG_DATA_HOME")
            .unwrap_or_else(|| home.join(".local").join("share"))
            .join("mimocode")
    }
}

fn mimocode_channel_db_paths(data_dir: &Path) -> Vec<PathBuf> {
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
        if name.starts_with("mimocode-") && name.ends_with(".db") {
            paths.push(path);
        }
    }
    paths.sort();
    paths
}

fn mimocode_db_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "mimocode_sqlite",
        ProviderSourceKind::NativeHistory,
    )
}

fn env_truthy(name: &str) -> bool {
    env::var(name)
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true"))
        .unwrap_or(false)
}

fn discover_cline_task_json_sources(home: &Path, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
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

fn discover_roo_task_json_sources(home: &Path, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
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

fn task_json_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
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

fn vscode_settings_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        home.join(".config/Code/User/settings.json"),
        home.join(".config/Code - Insiders/User/settings.json"),
        home.join(".vscode-server/data/User/settings.json"),
        home.join(".vscode-server-insiders/data/User/settings.json"),
    ];
    if let Some(appdata) = env_path("APPDATA") {
        paths.push(appdata.join("Code/User/settings.json"));
        paths.push(appdata.join("Code - Insiders/User/settings.json"));
    }
    paths
}

fn roo_custom_storage_path(settings_path: &Path, home: &Path) -> Option<PathBuf> {
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

fn discover_pi_custom_session_sources(
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

pub(super) fn discover_pi_custom_session_sources_with_project_settings(
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

fn pi_session_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "pi_session_jsonl",
        ProviderSourceKind::NativeHistory,
    )
}

fn crush_db_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "crush_sqlite",
        ProviderSourceKind::NativeHistory,
    )
}

fn goose_db_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "goose_sessions_sqlite",
        ProviderSourceKind::NativeHistory,
    )
}

fn crush_config_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = env_path("CRUSH_GLOBAL_CONFIG") {
        paths.push(path);
    }
    paths.push(home.join(".config").join("crush").join("crush.json"));
    paths
}

fn crush_config_data_dir(config_path: &Path, home: &Path) -> Option<PathBuf> {
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

fn pi_agent_dir(home: &Path) -> PathBuf {
    env_path_with_home("PI_CODING_AGENT_DIR", home).unwrap_or_else(|| home.join(".pi/agent"))
}

fn pi_settings_session_dir(
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

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn env_path_with_home(name: &str, home: &Path) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| resolve_home_relative_path(&value.to_string_lossy(), home, home))
}

fn env_path_resolved(name: &str, home: &Path) -> Option<PathBuf> {
    let relative_base = env::current_dir().unwrap_or_else(|_| home.to_path_buf());
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| resolve_home_relative_path(&value.to_string_lossy(), home, &relative_base))
}

fn resolve_pi_config_path(value: &str, home: &Path, relative_base: &Path) -> PathBuf {
    resolve_home_relative_path(value, home, relative_base)
}

fn resolve_home_relative_path(value: &str, home: &Path, relative_base: &Path) -> PathBuf {
    let trimmed = value.trim();
    if trimmed == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return home.join(rest);
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        relative_base.join(path)
    }
}

fn current_dir_ancestors_with(matches: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let Ok(current_dir) = env::current_dir() else {
        return Vec::new();
    };
    current_dir
        .ancestors()
        .filter(|candidate| matches(candidate))
        .map(Path::to_path_buf)
        .collect()
}

fn dedupe_sources(sources: Vec<ProviderSource>) -> Vec<ProviderSource> {
    let mut seen = HashSet::new();
    sources
        .into_iter()
        .filter(|source| seen.insert((source.provider, source.path.clone(), source.source_format)))
        .collect()
}

fn provider_source_from_parts(
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
    source_kind: ProviderSourceKind,
) -> ProviderSource {
    let location = ProviderDefaultLocation {
        path_components: &[],
        source_format,
        source_kind,
    };
    provider_source_from_location(spec, &location, path)
}

fn trae_workspace_storage_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    let mut source = provider_source_from_parts(
        spec,
        path,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        ProviderSourceKind::NativeHistory,
    );
    source.import_support = ProviderImportSupport::Native;
    source
}

pub fn provider_source_for_path(provider: CaptureProvider, path: PathBuf) -> ProviderSource {
    let unknown_spec = ProviderSourceSpec {
        provider,
        display_name: "unknown",
        default_locations: &[],
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: Some("provider is not registered for native local-history import"),
    };
    let spec = provider_source_spec(provider).unwrap_or(&unknown_spec);
    let exists = path.exists();

    let source_format = match provider {
        CaptureProvider::Codex if path.is_dir() => "codex_session_jsonl_tree",
        CaptureProvider::Codex => {
            if path.file_name().and_then(|name| name.to_str()) == Some("history.jsonl") {
                "codex_history_jsonl"
            } else {
                "codex_session_jsonl"
            }
        }
        CaptureProvider::Pi => "pi_session_jsonl",
        CaptureProvider::Claude => "claude_projects_jsonl_tree",
        CaptureProvider::OpenCode => "opencode_sqlite",
        CaptureProvider::Kilo => "kilo_sqlite",
        CaptureProvider::KiroCli => "kiro_cli_sqlite",
        CaptureProvider::MiMoCode => "mimocode_sqlite",
        CaptureProvider::Crush => "crush_sqlite",
        CaptureProvider::Goose => "goose_sessions_sqlite",
        CaptureProvider::Antigravity => "antigravity_cli_transcript_jsonl_tree",
        CaptureProvider::Gemini => "gemini_cli_chat_recording_jsonl",
        CaptureProvider::Tabnine => "tabnine_cli_chat_recording_jsonl",
        CaptureProvider::Cursor
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") =>
        {
            "cursor_agent_transcript_jsonl"
        }
        CaptureProvider::Cursor => "cursor_agent_transcript_jsonl_tree",
        CaptureProvider::Windsurf
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") =>
        {
            "windsurf_cascade_hook_transcript_jsonl"
        }
        CaptureProvider::Windsurf => "windsurf_cascade_hook_transcript_jsonl_tree",
        CaptureProvider::Zed => "zed_threads_sqlite",
        CaptureProvider::CopilotCli => "copilot_cli_session_events_jsonl",
        CaptureProvider::FactoryAiDroid => "factory_ai_droid_sessions_jsonl",
        CaptureProvider::QwenCode if path.is_dir() => "qwen_code_chat_jsonl_tree",
        CaptureProvider::QwenCode => "qwen_code_chat_jsonl",
        CaptureProvider::KimiCodeCli if path.is_dir() => "kimi_code_cli_wire_jsonl_tree",
        CaptureProvider::KimiCodeCli => "kimi_code_cli_wire_jsonl",
        CaptureProvider::Auggie => "auggie_session_json",
        CaptureProvider::Junie if path.is_dir() => "junie_session_events_jsonl_tree",
        CaptureProvider::Junie => "junie_session_events_jsonl",
        CaptureProvider::Firebender => "firebender_chat_history_sqlite",
        CaptureProvider::ForgeCode => "forgecode_sqlite",
        CaptureProvider::DeepAgents => "deepagents_sessions_sqlite",
        CaptureProvider::MistralVibe if path.is_dir() => "mistral_vibe_session_jsonl_tree",
        CaptureProvider::MistralVibe => "mistral_vibe_session_jsonl",
        CaptureProvider::Mux if path.is_dir() => "mux_session_jsonl_tree",
        CaptureProvider::Mux => "mux_session_jsonl",
        CaptureProvider::RovoDev => "rovodev_session_json_tree",
        CaptureProvider::OpenClaw => "openclaw_session_jsonl_tree",
        CaptureProvider::Hermes => "hermes_state_sqlite",
        CaptureProvider::NanoClaw => "nanoclaw_project",
        CaptureProvider::AstrBot => "astrbot_data_v4_sqlite",
        CaptureProvider::Shelley => "shelley_sqlite",
        CaptureProvider::Continue => "continue_cli_sessions_json",
        CaptureProvider::OpenHands => "openhands_file_events",
        CaptureProvider::Cline => "cline_task_directory_json",
        CaptureProvider::RooCode => "roo_task_directory_json",
        CaptureProvider::Lingma => "lingma_sqlite",
        CaptureProvider::Trae => "trae_state_vscdb",
        CaptureProvider::Qoder if path.is_dir() => "qoder_transcript_jsonl_tree",
        CaptureProvider::Qoder => "qoder_transcript_jsonl",
        CaptureProvider::Warp => "warp_sqlite",
        CaptureProvider::CodeBuddy => "codebuddy_history_json",
        _ => "unsupported",
    };
    let explicit_import_support = spec.import_support;
    let source_kind = if explicit_import_support.is_importable() {
        ProviderSourceKind::NativeHistory
    } else {
        ProviderSourceKind::DetectionOnly
    };

    ProviderSource {
        provider,
        exists,
        path,
        source_format,
        source_kind,
        import_support: explicit_import_support,
        catalog_support: spec.catalog_support,
        status: if matches!(explicit_import_support, ProviderImportSupport::Unsupported) {
            ProviderSourceStatus::Unsupported
        } else if exists {
            ProviderSourceStatus::Available
        } else {
            ProviderSourceStatus::Missing
        },
        unsupported_reason: spec.unsupported_reason,
    }
}

fn provider_source_from_location(
    spec: &ProviderSourceSpec,
    location: &ProviderDefaultLocation,
    path: PathBuf,
) -> ProviderSource {
    let path_exists = path.try_exists();
    let exists = path_exists.as_ref().copied().unwrap_or(true);
    let (status, unsupported_reason) =
        if matches!(spec.import_support, ProviderImportSupport::Unsupported) {
            (ProviderSourceStatus::Unsupported, spec.unsupported_reason)
        } else {
            match path_exists {
                Ok(false) => (ProviderSourceStatus::Missing, spec.unsupported_reason),
                Err(_) => (
                    ProviderSourceStatus::Unknown,
                    probe_io_error_reason(spec.provider),
                ),
                Ok(true) => match default_location_import_probe(spec.provider, location, &path) {
                    BoundedProbe::Found => {
                        (ProviderSourceStatus::Available, spec.unsupported_reason)
                    }
                    BoundedProbe::NotFound => (
                        ProviderSourceStatus::Empty,
                        empty_source_reason(spec.provider),
                    ),
                    BoundedProbe::BudgetExhausted => (
                        ProviderSourceStatus::Unknown,
                        unknown_source_reason(spec.provider),
                    ),
                    BoundedProbe::IoError => (
                        ProviderSourceStatus::Unknown,
                        probe_io_error_reason(spec.provider),
                    ),
                },
            }
        };
    ProviderSource {
        provider: spec.provider,
        path,
        exists,
        source_format: location.source_format,
        source_kind: location.source_kind,
        import_support: spec.import_support,
        catalog_support: spec.catalog_support,
        status,
        unsupported_reason,
    }
}
