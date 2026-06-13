use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const LOCAL_MODEL_ID: &str = "ggml-org/Qwen3-1.7B-GGUF";
pub const LOCAL_MODEL_FILE: &str = "Qwen3-1.7B-Q4_K_M.gguf";
pub const LOCAL_MODEL_VERSION: &str = "Qwen3-1.7B-Q4_K_M";
pub const LOCAL_MODEL_URL: &str =
    "https://huggingface.co/ggml-org/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q4_K_M.gguf";

pub const LLAMA_CPP_VERSION: &str = "b7847";

#[derive(Debug, Clone, Copy)]
pub enum RuntimeArchiveKind {
    TarGz,
    Zip,
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeDownloadSpec {
    pub version: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub archive_kind: RuntimeArchiveKind,
}

pub fn runtime_download_spec() -> Option<RuntimeDownloadSpec> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(RuntimeDownloadSpec {
            version: LLAMA_CPP_VERSION,
            url: "https://api.ctx.rs/storage/v1/object/public/releases/runtimes/llama.cpp/b7847/macos/aarch64/sha256/bf19a461d787561e4a25f8ea20904cfe183e5aaa7d744fba9224f5c6204688f6/llama-b7847-bin-macos-arm64.tar.gz",
            sha256: "bf19a461d787561e4a25f8ea20904cfe183e5aaa7d744fba9224f5c6204688f6",
            archive_kind: RuntimeArchiveKind::TarGz,
        }),
        ("macos", "x86_64") => Some(RuntimeDownloadSpec {
            version: LLAMA_CPP_VERSION,
            url: "https://api.ctx.rs/storage/v1/object/public/releases/runtimes/llama.cpp/b7847/macos/x86_64/sha256/3bb896d28cdffa2532f5fe62378cd388abe448cdf253a24fafd7a97abc9f1fc0/llama-b7847-bin-macos-x64.tar.gz",
            sha256: "3bb896d28cdffa2532f5fe62378cd388abe448cdf253a24fafd7a97abc9f1fc0",
            archive_kind: RuntimeArchiveKind::TarGz,
        }),
        ("linux", "x86_64") => Some(RuntimeDownloadSpec {
            version: LLAMA_CPP_VERSION,
            url: "https://api.ctx.rs/storage/v1/object/public/releases/runtimes/llama.cpp/b7847/linux/x86_64/sha256/5e002408611bd9ac991753fd986f7ceaad437a09582a5ff7da85fd4e2cfac117/llama-b7847-bin-ubuntu-x64.tar.gz",
            sha256: "5e002408611bd9ac991753fd986f7ceaad437a09582a5ff7da85fd4e2cfac117",
            archive_kind: RuntimeArchiveKind::TarGz,
        }),
        ("windows", "x86_64") => Some(RuntimeDownloadSpec {
            version: LLAMA_CPP_VERSION,
            url: "https://api.ctx.rs/storage/v1/object/public/releases/runtimes/llama.cpp/b7847/windows/x86_64/sha256/429881f5294f5af94c26f235daa0c2975bf087fe467966b2ea10412167fae595/llama-b7847-bin-win-cpu-x64.zip",
            sha256: "429881f5294f5af94c26f235daa0c2975bf087fe467966b2ea10412167fae595",
            archive_kind: RuntimeArchiveKind::Zip,
        }),
        _ => None,
    }
}

pub fn runtime_dir(data_root: &Path) -> PathBuf {
    data_root
        .join("runtimes")
        .join("llama.cpp")
        .join(LLAMA_CPP_VERSION)
}

pub fn runtime_binary_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

pub fn runtime_binary_path(data_root: &Path) -> PathBuf {
    runtime_dir(data_root).join(runtime_binary_name())
}

pub fn find_runtime_binary(data_root: &Path) -> Option<PathBuf> {
    let runtime_root = runtime_dir(data_root);
    if !runtime_root.exists() {
        return None;
    }
    let bin_name = runtime_binary_name();
    let mut queue = VecDeque::from([(runtime_root, 0usize)]);
    while let Some((dir, depth)) = queue.pop_front() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == bin_name)
                && path.is_file()
            {
                return Some(path);
            }
            if depth < 3 && entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                queue.push_back((path, depth + 1));
            }
        }
    }
    None
}

pub fn model_dir(data_root: &Path) -> PathBuf {
    data_root.join("models").join("title_generation")
}

pub fn model_path(data_root: &Path) -> PathBuf {
    model_dir(data_root).join(LOCAL_MODEL_FILE)
}

pub fn model_metadata_path(data_root: &Path) -> PathBuf {
    model_dir(data_root).join("metadata.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelMetadata {
    pub id: String,
    pub version: String,
    pub sha256: String,
    pub size: u64,
    pub installed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TitleGenerationLocalRuntimeStatus {
    pub version: String,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TitleGenerationLocalModelStatus {
    pub model_id: String,
    pub file_name: String,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TitleGenerationLocalStatus {
    pub ready: bool,
    pub runtime: TitleGenerationLocalRuntimeStatus,
    pub model: TitleGenerationLocalModelStatus,
}

pub async fn load_model_metadata(data_root: &Path) -> Option<LocalModelMetadata> {
    let path = model_metadata_path(data_root);
    let content = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str(&content).ok()
}

pub async fn write_model_metadata(data_root: &Path, meta: &LocalModelMetadata) -> Result<()> {
    let path = model_metadata_path(data_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    let bytes = serde_json::to_vec_pretty(meta)?;
    tokio::fs::write(&path, bytes)
        .await
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn resolve_model_path(data_root: &Path, model_id: &str) -> Result<PathBuf> {
    if model_id.trim().is_empty() {
        return Err(anyhow!("local model id is empty"));
    }
    if model_id.trim() != LOCAL_MODEL_ID {
        return Err(anyhow!("unknown local model id: {model_id}"));
    }
    let path = model_path(data_root);
    if !path.exists() {
        return Err(anyhow!("local model not installed"));
    }
    Ok(path)
}

pub fn resolve_runtime_binary(data_root: &Path) -> Result<PathBuf> {
    find_runtime_binary(data_root)
        .filter(|path| path.exists())
        .ok_or_else(|| anyhow!("llama-server runtime not installed"))
}

pub async fn local_status(data_root: &Path) -> Result<TitleGenerationLocalStatus> {
    let runtime_path = find_runtime_binary(data_root);
    let runtime_installed = runtime_path.is_some();
    let runtime = TitleGenerationLocalRuntimeStatus {
        version: LLAMA_CPP_VERSION.to_string(),
        installed: runtime_installed,
        path: runtime_path.map(|path| path.to_string_lossy().to_string()),
    };

    let model_path = model_path(data_root);
    let model_installed = model_path.exists();
    let meta = load_model_metadata(data_root).await;
    let model = TitleGenerationLocalModelStatus {
        model_id: LOCAL_MODEL_ID.to_string(),
        file_name: LOCAL_MODEL_FILE.to_string(),
        installed: model_installed,
        version: meta.as_ref().map(|m| m.version.clone()),
        sha256: meta.as_ref().map(|m| m.sha256.clone()),
        size_bytes: meta.as_ref().map(|m| m.size),
        installed_at: meta.as_ref().map(|m| m.installed_at),
    };

    Ok(TitleGenerationLocalStatus {
        ready: runtime_installed && model_installed,
        runtime,
        model,
    })
}
