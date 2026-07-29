pub(super) fn semantic_env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

pub(crate) fn semantic_health_findings(data_root: &Path) -> Vec<String> {
    let mut findings = Vec::new();
    let semantic_lock = semantic_worker_lock_path(data_root);
    if semantic_lock.exists() && pid_lock_file_is_orphaned(&semantic_lock) {
        findings.push(format!(
            "semantic worker lock is stale: {}",
            semantic_lock.display()
        ));
    }
    let daemon_lock = daemon_lock_path(data_root);
    if daemon_lock.exists() && pid_lock_file_is_orphaned(&daemon_lock) {
        findings.push(format!("daemon lock is stale: {}", daemon_lock.display()));
    }
    if let Some(status) = read_semantic_worker_status(data_root) {
        if status.get("status").and_then(Value::as_str) == Some("failed") {
            let error = json_string(&status, "last_error").unwrap_or_else(|| "unknown".to_owned());
            findings.push(format!("semantic worker last failed: {error}"));
        }
    }
    if let Some(status) = read_daemon_status(data_root) {
        if status.get("status").and_then(Value::as_str) == Some("failed") {
            let error = json_string(&status, "last_error").unwrap_or_else(|| "unknown".to_owned());
            findings.push(format!("daemon last failed: {error}"));
        }
    }
    let vector_path = source_backed_semantic_vector_path(data_root);
    if vector_path.exists() {
        match SemanticVectorStore::open_read_only(&vector_path) {
            Ok(Some(vector_store)) => match vector_store.plaintext_value_count() {
                Ok(0) => {}
                Ok(count) => findings.push(format!(
                    "semantic vector sidecar contains {count} plaintext value(s); run daemon maintenance to scrub it"
                )),
                Err(error) => findings.push(format!(
                    "semantic vector sidecar plaintext check failed: {error:#}"
                )),
            },
            Ok(None) => {}
            Err(error) => findings.push(format!(
                "semantic vector sidecar is unreadable at {}: {error:#}",
                vector_path.display()
            )),
        }
    }
    findings
}

pub(super) fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

pub(super) fn json_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|value| value.as_i64())
}

pub(super) fn json_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
}

pub(super) fn json_usize(value: &Value, key: &str) -> Option<usize> {
    value
        .get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
}

pub(super) fn create_private_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("create private directory {}", path.display()))?;
    secure_private_dir_permissions(path)?;
    Ok(())
}

pub(super) fn private_create_new_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

#[cfg(unix)]
pub(super) fn private_create_new_lock_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
pub(super) fn private_create_new_lock_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
pub(super) fn private_open_existing_lock_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
pub(super) fn private_open_existing_lock_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ctx process lock is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn private_open_existing_lock_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new().read(true).write(true).open(path)
}

pub(super) fn secure_private_dir_permissions(path: &Path) -> Result<()> {
    restrict_private_directory(path)
        .with_context(|| format!("secure private directory {}", path.display()))?;
    Ok(())
}

pub(super) fn secure_private_file_permissions(path: &Path) -> Result<()> {
    restrict_private_file(path)
        .with_context(|| format!("secure private file {}", path.display()))?;
    Ok(())
}

pub(super) fn secure_semantic_vector_permissions(path: &Path) -> Result<()> {
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        if candidate.exists() {
            restrict_private_file(&candidate)
                .with_context(|| format!("secure semantic vector file {}", candidate.display()))?;
        }
    }
    Ok(())
}

pub(super) fn semantic_query_text(query: &str, terms: &[String]) -> String {
    let mut parts = Vec::new();
    if !query.trim().is_empty() {
        parts.push(query.trim().to_owned());
    }
    parts.extend(
        terms
            .iter()
            .map(|term| term.trim())
            .filter(|term| !term.is_empty())
            .map(str::to_owned),
    );
    parts.join(" ")
}

pub(super) fn semantic_hybrid_coverage_ready(
    embedded_items: usize,
    searchable_items: usize,
    dirty_items: usize,
) -> bool {
    if searchable_items == 0 {
        return true;
    }
    embedded_items >= searchable_items && dirty_items == 0
}

pub(super) fn semantic_status_needs_exact_sidecar_stats(
    searchable_items: usize,
    dirty_items: usize,
    stats: SemanticSidecarStats,
) -> bool {
    if searchable_items == 0 || dirty_items > 0 {
        return false;
    }
    stats.embedded_items >= searchable_items
}

pub(super) fn daemon_query_embedding(
    data_root: &Path,
    semantic_text: &str,
) -> Result<Option<(Vec<f32>, u64)>> {
    let Some(response) = daemon_query_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "embed_query",
            "model_key": semantic_model_key(),
            "text": semantic_text,
        })),
        StdDuration::from_secs(30),
        1024 * 1024,
    )?
    else {
        return Ok(None);
    };
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("daemon query failed");
        return Err(anyhow!("{message}"));
    }
    let model_key = response
        .get("model_key")
        .and_then(Value::as_str)
        .unwrap_or("");
    if model_key != semantic_model_key() {
        return Err(anyhow!("daemon query response model key mismatch"));
    }
    let query_embed_ms = response
        .get("query_embed_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let embedding = response
        .get("embedding")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("daemon query response missing embedding"))?
        .iter()
        .map(|value| {
            value
                .as_f64()
                .map(|value| value as f32)
                .ok_or_else(|| anyhow!("daemon query embedding contains a non-number"))
        })
        .collect::<Result<Vec<_>>>()?;
    if embedding.len() != SEMANTIC_DIMENSIONS {
        return Err(anyhow!(
            "daemon query embedding returned {} dimensions, expected {}",
            embedding.len(),
            SEMANTIC_DIMENSIONS
        ));
    }
    Ok(Some((embedding, query_embed_ms)))
}

#[cfg(ctx_semantic_fastembed)]
#[derive(Debug, Clone)]
pub(super) struct SemanticEmbedPolicy {
    pub(super) threads: usize,
    pub(super) batch_size: usize,
    pub(super) memory_budget_bytes: u64,
    pub(super) total_memory_bytes: Option<u64>,
    pub(super) available_memory_bytes: Option<u64>,
    pub(super) active_percent: u8,
    pub(super) compute_class: SemanticComputeClass,
    pub(super) source: &'static str,
}

#[cfg(ctx_semantic_fastembed)]
impl SemanticEmbedPolicy {
    pub(super) fn status_json(&self) -> Value {
        compact_json(json!({
            "source": self.source,
            "threads": self.threads,
            "batch_size": self.batch_size,
            "memory_budget_bytes": self.memory_budget_bytes,
            "total_memory_bytes": self.total_memory_bytes,
            "available_memory_bytes": self.available_memory_bytes,
            "active_percent": self.active_percent,
            "compute_class": match self.compute_class {
                SemanticComputeClass::Cpu => "cpu",
                SemanticComputeClass::Accelerator => "accelerator",
            },
        }))
    }
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_embed_policy() -> SemanticEmbedPolicy {
    semantic_embed_policy_for(SemanticComputeClass::Cpu)
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_embed_policy_for(
    compute_class: SemanticComputeClass,
) -> SemanticEmbedPolicy {
    semantic_embed_policy_from_env_and_resources(compute_class, SemanticSystemResources::current())
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_embed_policy_status_json() -> Value {
    semantic_embed_policy().status_json()
}

#[cfg(not(ctx_semantic_fastembed))]
pub(super) fn semantic_embed_policy_status_json() -> Value {
    compact_json(json!({
        "source": "unsupported",
    }))
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_embedder_policy_status_json(embedder: &Option<SemanticEmbedder>) -> Value {
    embedder
        .as_ref()
        .map(|embedder| embedder.policy.status_json())
        .unwrap_or_else(semantic_embed_policy_status_json)
}

#[cfg(not(ctx_semantic_fastembed))]
pub(super) fn semantic_embedder_policy_status_json(_embedder: &Option<SemanticEmbedder>) -> Value {
    semantic_embed_policy_status_json()
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_embedder_runtime_status_json(
    embedder: &Option<SemanticEmbedder>,
) -> Option<Value> {
    embedder
        .as_ref()
        .map(|embedder| embedder.runtime_info().to_json())
}

#[cfg(not(ctx_semantic_fastembed))]
pub(super) fn semantic_embedder_runtime_status_json(
    _embedder: &Option<SemanticEmbedder>,
) -> Option<Value> {
    None
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_embed_policy_from_env_and_resources(
    compute_class: SemanticComputeClass,
    resources: SemanticSystemResources,
) -> SemanticEmbedPolicy {
    let quiet = semantic_quiet_policy(resources, compute_class);
    let mut policy = SemanticEmbedPolicy {
        threads: quiet.threads,
        batch_size: quiet.batch_size,
        memory_budget_bytes: quiet.memory_budget_bytes,
        total_memory_bytes: resources.total_memory_bytes,
        available_memory_bytes: resources.available_memory_bytes,
        active_percent: quiet.active_percent,
        compute_class,
        source: "dynamic_quiet",
    };
    let mut source = "dynamic_quiet";
    if let Some(threads) = env_usize("CTX_SEMANTIC_THREADS") {
        policy.threads = threads.min(SEMANTIC_EMBED_THREADS_MAX);
        source = "env_override";
    }
    if let Some(batch_size) = env_usize("CTX_SEMANTIC_EMBED_BATCH") {
        policy.batch_size = batch_size.min(SEMANTIC_EMBED_BATCH_MAX);
        source = "env_override";
    }
    policy.source = source;
    policy
}

pub(crate) fn semantic_worker_cache_dir(data_root: &Path) -> PathBuf {
    let env = SemanticCacheEnv::current();
    semantic_worker_cache_dir_from_env(data_root, &env)
}

#[derive(Debug, Clone, Default)]
pub(super) struct SemanticCacheEnv {
    pub(super) hf_home: Option<PathBuf>,
    pub(super) semantic_cache_dir: Option<PathBuf>,
    pub(super) fastembed_cache_dir: Option<PathBuf>,
    pub(super) hf_hub_cache: Option<PathBuf>,
    pub(super) xdg_cache_home: Option<PathBuf>,
    pub(super) home: Option<PathBuf>,
    pub(super) current_dir: Option<PathBuf>,
}

impl SemanticCacheEnv {
    pub(super) fn current() -> Self {
        Self {
            hf_home: env_path("HF_HOME"),
            semantic_cache_dir: env_path("CTX_SEMANTIC_CACHE_DIR"),
            fastembed_cache_dir: env_path("FASTEMBED_CACHE_DIR"),
            hf_hub_cache: env_path("HF_HUB_CACHE"),
            xdg_cache_home: env_path("XDG_CACHE_HOME"),
            home: env_path("HOME"),
            current_dir: env::current_dir().ok(),
        }
    }
}

pub(super) fn env_path(name: &str) -> Option<PathBuf> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(super) fn semantic_worker_cache_dir_from_env(
    data_root: &Path,
    env: &SemanticCacheEnv,
) -> PathBuf {
    if let Some(path) = env.semantic_cache_dir.as_ref() {
        return path.clone();
    }
    if let Some(path) = env.fastembed_cache_dir.as_ref() {
        return path.clone();
    }
    if let Some(path) = env.hf_hub_cache.as_ref() {
        return path.clone();
    }
    if let Some(path) = env.hf_home.as_ref() {
        return path.clone();
    }

    semantic_worker_default_cache_candidates(data_root, env)
        .into_iter()
        .find(|path| semantic_model_cache_available(path))
        .unwrap_or_else(|| data_root.join("semantic-model-cache"))
}

pub(super) fn semantic_worker_default_cache_candidates(
    data_root: &Path,
    env: &SemanticCacheEnv,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    cache_paths::push_unique_path(&mut candidates, data_root.join("semantic-model-cache"));
    if let Some(current_dir) = env.current_dir.as_ref() {
        cache_paths::push_unique_path(&mut candidates, current_dir.join(".fastembed_cache"));
    }
    if let Some(xdg_cache_home) = env.xdg_cache_home.as_ref() {
        cache_paths::push_unique_path(&mut candidates, xdg_cache_home.join("fastembed"));
        cache_paths::push_unique_path(
            &mut candidates,
            xdg_cache_home.join("huggingface").join("hub"),
        );
        cache_paths::push_unique_path(&mut candidates, xdg_cache_home.join("huggingface"));
    }
    if let Some(home) = env.home.as_ref() {
        let cache = home.join(".cache");
        cache_paths::push_unique_path(&mut candidates, home.join(".fastembed_cache"));
        cache_paths::push_unique_path(&mut candidates, cache.join("fastembed"));
        cache_paths::push_unique_path(&mut candidates, cache.join("huggingface").join("hub"));
        cache_paths::push_unique_path(&mut candidates, cache.join("huggingface"));
    }
    candidates
}

pub(super) fn semantic_model_cache_available(cache_dir: &Path) -> bool {
    semantic_model_cache_snapshot_dir(cache_dir).is_some()
        || semantic_accelerator_model_cache_available(cache_dir)
        || semantic_coreml_model_cache_available(cache_dir)
}

pub(super) fn semantic_accelerator_model_cache_available(cache_dir: &Path) -> bool {
    semantic_ort_model_cache_snapshot_dir(cache_dir, SemanticOrtModelVariant::AcceleratorO4Fp16)
        .is_some()
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn semantic_coreml_model_cache_available(cache_dir: &Path) -> bool {
    coreml_bundle_cache_available(cache_dir)
}

#[cfg(not(any(target_os = "macos", test)))]
pub(super) fn semantic_coreml_model_cache_available(_cache_dir: &Path) -> bool {
    false
}

pub(super) fn semantic_model_acquisition_status_json(cache_dir: &Path) -> Value {
    let cpu_available = semantic_model_cache_snapshot_dir(cache_dir).is_some();
    let accelerator_available = semantic_accelerator_model_cache_available(cache_dir);
    #[cfg(any(target_os = "macos", test))]
    let coreml = coreml_acquisition_status_json(cache_dir);
    #[cfg(not(any(target_os = "macos", test)))]
    let coreml = json!({
        "cache_status": "unsupported",
        "descriptor_provisioned": false,
        "network_scope": "daemon_only",
    });
    compact_json(json!({
        "network_scope": "daemon_only",
        "cpu": {
            "cache_status": if cpu_available { "present" } else { "missing" },
            "verification": "sha256_on_load",
            "source_revision": SEMANTIC_MODEL_REVISION,
        },
        "accelerator": {
            "cache_status": if accelerator_available { "present" } else { "missing" },
            "model_variant": SemanticOrtModelVariant::AcceleratorO4Fp16.as_str(),
            "verification": "sha256_on_load",
            "source_revision": SEMANTIC_MODEL_REVISION,
        },
        "coreml": coreml,
    }))
}

pub(super) fn semantic_model_acquisition_integrity_error(error: &anyhow::Error) -> bool {
    if error
        .downcast_ref::<SemanticCpuModelIntegrityError>()
        .is_some()
    {
        return true;
    }
    #[cfg(any(target_os = "macos", test))]
    {
        model_acquisition_integrity_error(error)
    }
    #[cfg(not(any(target_os = "macos", test)))]
    false
}

pub(super) fn semantic_model_cache_snapshot_dir(cache_dir: &Path) -> Option<PathBuf> {
    semantic_ort_model_cache_snapshot_dir(cache_dir, SemanticOrtModelVariant::CpuFp32)
}

fn semantic_ort_model_cache_snapshot_dir(
    cache_dir: &Path,
    variant: SemanticOrtModelVariant,
) -> Option<PathBuf> {
    if !semantic_embedding_supported() {
        return None;
    }
    if cache_dir.as_os_str().is_empty() {
        return None;
    }
    for model_root in cache_paths::semantic_model_cache_roots(cache_dir) {
        if let Some(snapshot) = semantic_ort_model_snapshot_from_root(&model_root, variant) {
            return Some(snapshot);
        }
    }
    None
}

fn semantic_ort_model_snapshot_from_root(
    model_root: &Path,
    variant: SemanticOrtModelVariant,
) -> Option<PathBuf> {
    let snapshot = model_root.join("snapshots").join(SEMANTIC_MODEL_REVISION);
    if !snapshot.is_dir() {
        return None;
    }
    if variant.required_files().all(|file| {
        fs::metadata(snapshot.join(file.path))
            .map(|metadata| metadata.is_file() && metadata.len() == file.size)
            .unwrap_or(false)
    }) {
        Some(snapshot)
    } else {
        None
    }
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_embedding_supported() -> bool {
    true
}

#[cfg(not(ctx_semantic_fastembed))]
pub(super) fn semantic_embedding_supported() -> bool {
    false
}

pub(super) fn env_usize(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration as StdDuration,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::compact_json;
use anyhow::{anyhow, Context, Result};
use ctx_history_core::platform_security::{restrict_private_directory, restrict_private_file};
use serde_json::{json, Value};

use super::{
    cache_paths,
    model_contract::{
        semantic_model_key, SemanticCpuModelIntegrityError, SemanticOrtModelVariant,
        SEMANTIC_DIMENSIONS, SEMANTIC_MODEL_REVISION,
    },
    model_runtime::SemanticEmbedder,
    paths_status::{
        daemon_lock_path, pid_lock_file_is_orphaned, read_daemon_status,
        read_semantic_worker_status, semantic_worker_lock_path,
    },
    query_service::daemon_query_request,
    resource_policy::{semantic_quiet_policy, SemanticComputeClass, SemanticSystemResources},
    runtime_limits::{SEMANTIC_EMBED_BATCH_MAX, SEMANTIC_EMBED_THREADS_MAX},
    vector_store::{source_backed_semantic_vector_path, SemanticSidecarStats, SemanticVectorStore},
};

#[cfg(any(target_os = "macos", test))]
use super::model_acquisition::{
    coreml_acquisition_status_json, coreml_bundle_cache_available,
    model_acquisition_integrity_error,
};
