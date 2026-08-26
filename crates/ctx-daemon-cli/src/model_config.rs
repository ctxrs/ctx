use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use ctx_history_platform::default_data_root;
use ctx_semantic_model::{
    semantic_model_cache_available, SemanticBackendPreference, SemanticCoreMlComputeMode,
    SemanticModelConfig, SemanticModelPaths, SemanticOnnxRuntimePaths,
};

#[derive(Debug, Clone, Default)]
struct SemanticHostEnvironment {
    hf_home: Option<PathBuf>,
    semantic_cache_dir: Option<PathBuf>,
    fastembed_cache_dir: Option<PathBuf>,
    hf_hub_cache: Option<PathBuf>,
    xdg_cache_home: Option<PathBuf>,
    home: Option<PathBuf>,
    current_dir: Option<PathBuf>,
    ctx_onnxruntime_dylib: Option<PathBuf>,
    ort_dylib_path: Option<PathBuf>,
    ctx_onnxruntime_dir: Option<PathBuf>,
    ctx_onnxruntime_cache_dir: Option<PathBuf>,
    installed_runtime_dir: Option<PathBuf>,
    executable_dir: Option<PathBuf>,
    backend_preference: Option<String>,
    coreml_compute_mode: Option<String>,
    thread_override: Option<usize>,
    batch_size_override: Option<usize>,
    deprecated_model_onnx_present: bool,
}

impl SemanticHostEnvironment {
    fn current() -> Self {
        Self {
            hf_home: env_path("HF_HOME"),
            semantic_cache_dir: env_path("CTX_SEMANTIC_CACHE_DIR"),
            fastembed_cache_dir: env_path("FASTEMBED_CACHE_DIR"),
            hf_hub_cache: env_path("HF_HUB_CACHE"),
            xdg_cache_home: env_path("XDG_CACHE_HOME"),
            home: env_path("HOME"),
            current_dir: env::current_dir().ok(),
            ctx_onnxruntime_dylib: env_path("CTX_ONNXRUNTIME_DYLIB"),
            ort_dylib_path: env_path("ORT_DYLIB_PATH"),
            ctx_onnxruntime_dir: env_path("CTX_ONNXRUNTIME_DIR"),
            ctx_onnxruntime_cache_dir: env_path("CTX_ONNXRUNTIME_CACHE_DIR"),
            installed_runtime_dir: env_path("CTX_RUNTIME_DIR")
                .or_else(|| default_data_root().ok().map(|root| root.join("runtime"))),
            executable_dir: env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf)),
            backend_preference: env::var("CTX_INTERNAL_SEMANTIC_BACKEND").ok(),
            coreml_compute_mode: env::var("CTX_SEMANTIC_COREML_NATIVE_COMPUTE").ok(),
            thread_override: env_usize("CTX_SEMANTIC_THREADS"),
            batch_size_override: env_usize("CTX_SEMANTIC_EMBED_BATCH"),
            deprecated_model_onnx_present: env::var_os("CTX_SEMANTIC_MODEL_ONNX").is_some(),
        }
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn env_usize(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

pub fn semantic_worker_cache_dir(data_root: &Path) -> PathBuf {
    semantic_worker_cache_dir_from_environment(data_root, &SemanticHostEnvironment::current())
}

fn semantic_worker_cache_dir_from_environment(
    data_root: &Path,
    environment: &SemanticHostEnvironment,
) -> PathBuf {
    if let Some(path) = [
        environment.semantic_cache_dir.as_ref(),
        environment.fastembed_cache_dir.as_ref(),
        environment.hf_hub_cache.as_ref(),
        environment.hf_home.as_ref(),
    ]
    .into_iter()
    .flatten()
    .next()
    {
        return path.clone();
    }
    semantic_worker_default_cache_candidates(data_root, environment)
        .into_iter()
        .find(|path| semantic_model_cache_available(path))
        .unwrap_or_else(|| data_root.join("semantic-model-cache"))
}

fn semantic_worker_default_cache_candidates(
    data_root: &Path,
    environment: &SemanticHostEnvironment,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    push_unique_path(&mut candidates, data_root.join("semantic-model-cache"));
    if let Some(current_dir) = environment.current_dir.as_ref() {
        push_unique_path(&mut candidates, current_dir.join(".fastembed_cache"));
    }
    if let Some(xdg_cache_home) = environment.xdg_cache_home.as_ref() {
        push_unique_path(&mut candidates, xdg_cache_home.join("fastembed"));
        push_unique_path(
            &mut candidates,
            xdg_cache_home.join("huggingface").join("hub"),
        );
        push_unique_path(&mut candidates, xdg_cache_home.join("huggingface"));
    }
    if let Some(home) = environment.home.as_ref() {
        let cache = home.join(".cache");
        push_unique_path(&mut candidates, home.join(".fastembed_cache"));
        push_unique_path(&mut candidates, cache.join("fastembed"));
        push_unique_path(&mut candidates, cache.join("huggingface").join("hub"));
        push_unique_path(&mut candidates, cache.join("huggingface"));
    }
    candidates
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

pub fn semantic_runtime_cache_dir(data_root: &Path) -> PathBuf {
    semantic_runtime_cache_dir_for_model_cache(&semantic_worker_cache_dir(data_root))
}

fn semantic_runtime_cache_dir_for_model_cache(model_cache_dir: &Path) -> PathBuf {
    if selected_data_root(model_cache_dir).is_some() {
        return model_cache_dir
            .parent()
            .map(|parent| parent.join("runtime"))
            .unwrap_or_else(|| model_cache_dir.join("semantic-runtime"));
    }
    model_cache_dir.join("semantic-runtime")
}

pub(crate) fn semantic_model_config(data_root: &Path) -> SemanticModelConfig {
    semantic_model_config_from_environment(data_root, &SemanticHostEnvironment::current())
}

fn semantic_model_config_from_environment(
    data_root: &Path,
    environment: &SemanticHostEnvironment,
) -> SemanticModelConfig {
    let model_cache_dir = semantic_worker_cache_dir_from_environment(data_root, environment);
    let default_runtime_cache_dir = selected_data_root(&model_cache_dir)
        .map(|root| root.join("semantic-runtime"))
        .unwrap_or_else(|| model_cache_dir.join("semantic-runtime"));
    let selected_data_root_runtime_dir =
        selected_data_root(&model_cache_dir).map(|root| root.join("runtime"));
    let runtime_paths = SemanticOnnxRuntimePaths::new(default_runtime_cache_dir)
        .with_ctx_dylib(environment.ctx_onnxruntime_dylib.clone())
        .with_ort_dylib(environment.ort_dylib_path.clone())
        .with_ctx_dir(environment.ctx_onnxruntime_dir.clone())
        .with_cache_dir(environment.ctx_onnxruntime_cache_dir.clone())
        .with_installed_runtime_dir(environment.installed_runtime_dir.clone())
        .with_selected_data_root_runtime_dir(selected_data_root_runtime_dir)
        .with_executable_dir(environment.executable_dir.clone());
    let mut config =
        SemanticModelConfig::new(SemanticModelPaths::new(model_cache_dir, runtime_paths))
            .with_thread_override(environment.thread_override)
            .with_batch_size_override(environment.batch_size_override)
            .with_deprecated_model_onnx_present(environment.deprecated_model_onnx_present);
    config = match parse_backend_preference(environment.backend_preference.as_deref()) {
        Ok(preference) => config.with_backend_preference(preference),
        Err(error) => config.with_backend_preference_error(error.to_string()),
    };
    match parse_coreml_compute_mode(environment.coreml_compute_mode.as_deref()) {
        Ok(mode) => config.with_coreml_compute_mode(mode),
        Err(error) => config.with_coreml_compute_mode_error(error.to_string()),
    }
}

fn selected_data_root(model_cache_dir: &Path) -> Option<&Path> {
    (model_cache_dir.file_name().and_then(|name| name.to_str()) == Some("semantic-model-cache"))
        .then(|| model_cache_dir.parent())
        .flatten()
}

fn parse_backend_preference(value: Option<&str>) -> Result<SemanticBackendPreference> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("auto") => Ok(SemanticBackendPreference::Auto),
        Some("cpu") => Ok(SemanticBackendPreference::Cpu),
        Some("coreml") => Ok(SemanticBackendPreference::CoreMl),
        Some("cuda") => Ok(SemanticBackendPreference::Cuda),
        Some("windowsml") => Ok(SemanticBackendPreference::WindowsMl),
        Some(value) => Err(anyhow!(
            "unsupported CTX_INTERNAL_SEMANTIC_BACKEND value {value:?}; expected auto, cpu, coreml, cuda, or windowsml"
        )),
    }
}

fn parse_coreml_compute_mode(value: Option<&str>) -> Result<SemanticCoreMlComputeMode> {
    match value.unwrap_or("all").trim().to_ascii_lowercase().as_str() {
        "all" => Ok(SemanticCoreMlComputeMode::All),
        "ane" | "cpu-ane" => Ok(SemanticCoreMlComputeMode::CpuAndNeuralEngine),
        "gpu" | "cpu-gpu" => Ok(SemanticCoreMlComputeMode::CpuAndGpu),
        "cpu" => Ok(SemanticCoreMlComputeMode::CpuOnly),
        value => Err(anyhow!(
            "unsupported CTX_SEMANTIC_COREML_NATIVE_COMPUTE mode {value:?}"
        )),
    }
}

#[cfg(test)]
#[path = "model_config_tests.rs"]
mod tests;
