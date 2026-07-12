#[cfg(ctx_semantic_fastembed)]
const CTX_ONNXRUNTIME_DYLIB_ENV: &str = "CTX_ONNXRUNTIME_DYLIB";
#[cfg(ctx_semantic_fastembed)]
const CTX_ONNXRUNTIME_DIR_ENV: &str = "CTX_ONNXRUNTIME_DIR";
#[cfg(ctx_semantic_fastembed)]
const CTX_ONNXRUNTIME_CACHE_DIR_ENV: &str = "CTX_ONNXRUNTIME_CACHE_DIR";
#[cfg(ctx_semantic_fastembed)]
const CTX_RUNTIME_DIR_ENV: &str = "CTX_RUNTIME_DIR";
#[cfg(ctx_semantic_fastembed)]
const ORT_DYLIB_PATH_ENV: &str = "ORT_DYLIB_PATH";
#[cfg(ctx_semantic_fastembed)]
const SEMANTIC_ONNXRUNTIME_VERSION: &str = "1.27.0";

#[cfg(all(ctx_semantic_fastembed, target_os = "windows"))]
const SEMANTIC_ONNXRUNTIME_DYLIB: &str = "onnxruntime.dll";
#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
const SEMANTIC_ONNXRUNTIME_DYLIB: &str = "libonnxruntime.dylib";
#[cfg(all(
    ctx_semantic_fastembed,
    not(target_os = "windows"),
    not(target_os = "macos")
))]
const SEMANTIC_ONNXRUNTIME_DYLIB: &str = "libonnxruntime.so";

#[cfg(ctx_semantic_fastembed)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct SemanticOnnxRuntimeCandidate {
    source: &'static str,
    path: PathBuf,
    try_even_if_missing: bool,
}

#[cfg(ctx_semantic_fastembed)]
#[derive(Debug, Clone, Default)]
struct SemanticOnnxRuntimeEnv {
    ctx_dylib: Option<PathBuf>,
    ort_dylib: Option<PathBuf>,
    ctx_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    exe_dir: Option<PathBuf>,
}

#[cfg(ctx_semantic_fastembed)]
impl SemanticOnnxRuntimeEnv {
    fn current() -> Self {
        Self {
            ctx_dylib: env_path(CTX_ONNXRUNTIME_DYLIB_ENV),
            ort_dylib: env_path(ORT_DYLIB_PATH_ENV),
            ctx_dir: env_path(CTX_ONNXRUNTIME_DIR_ENV),
            cache_dir: env_path(CTX_ONNXRUNTIME_CACHE_DIR_ENV),
            runtime_dir: env_path(CTX_RUNTIME_DIR_ENV)
                .or_else(|| default_data_root().ok().map(|root| root.join("runtime"))),
            exe_dir: env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf)),
        }
    }
}

#[cfg(ctx_semantic_fastembed)]
fn ensure_semantic_onnxruntime_loaded(model_cache_dir: &Path) -> Result<PathBuf> {
    static INIT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    static INIT_LOCK: Mutex<()> = Mutex::new(());

    if let Some(path) = INIT.get() {
        return Ok(path.clone());
    }

    let _lock = INIT_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    if let Some(path) = INIT.get() {
        return Ok(path.clone());
    }

    let path = load_semantic_onnxruntime(model_cache_dir, &SemanticOnnxRuntimeEnv::current())?;
    let _ = INIT.set(path.clone());
    Ok(path)
}

#[cfg(ctx_semantic_fastembed)]
fn load_semantic_onnxruntime(
    model_cache_dir: &Path,
    env: &SemanticOnnxRuntimeEnv,
) -> Result<PathBuf> {
    let mut failures = Vec::new();
    for candidate in semantic_onnxruntime_load_candidates(model_cache_dir, env) {
        if !candidate.try_even_if_missing && !candidate.path.exists() {
            continue;
        }
        match ort::init_from(&candidate.path) {
            Ok(builder) => {
                let _ = builder.commit();
                return Ok(candidate.path);
            }
            Err(error) => failures.push(format!(
                "{} {}: {error}",
                candidate.source,
                candidate.path.display()
            )),
        }
    }
    let detail = if failures.is_empty() {
        format!(
            "no ONNX Runtime dynamic library candidates were found for {}; set an absolute path with {CTX_ONNXRUNTIME_DYLIB_ENV}, {ORT_DYLIB_PATH_ENV}, {CTX_ONNXRUNTIME_DIR_ENV}, {CTX_ONNXRUNTIME_CACHE_DIR_ENV}, or {CTX_RUNTIME_DIR_ENV}",
            semantic_onnxruntime_platform_dir()
        )
    } else {
        format!(
            "failed to load ONNX Runtime dynamic library; tried {}",
            failures.join("; ")
        )
    };
    Err(anyhow!(detail))
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_onnxruntime_load_candidates(
    model_cache_dir: &Path,
    env: &SemanticOnnxRuntimeEnv,
) -> Vec<SemanticOnnxRuntimeCandidate> {
    let mut candidates = semantic_onnxruntime_candidates(model_cache_dir, env);
    let explicit_source = if env.ctx_dylib.is_some() {
        Some("ctx_env_dylib")
    } else if env.ort_dylib.is_some() {
        Some("ort_env_dylib")
    } else if env.ctx_dir.is_some() {
        Some("ctx_env_dir")
    } else {
        None
    };
    if let Some(source) = explicit_source {
        candidates.retain(|candidate| candidate.source == source);
    }
    candidates
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_onnxruntime_candidates(
    model_cache_dir: &Path,
    env: &SemanticOnnxRuntimeEnv,
) -> Vec<SemanticOnnxRuntimeCandidate> {
    let mut candidates = Vec::new();
    if let Some(path) = env.ctx_dylib.as_ref() {
        push_onnxruntime_candidate(&mut candidates, "ctx_env_dylib", path.clone(), true);
    }
    if let Some(path) = env.ort_dylib.as_ref() {
        push_onnxruntime_candidate(&mut candidates, "ort_env_dylib", path.clone(), true);
    }
    if let Some(path) = env.ctx_dir.as_ref() {
        push_onnxruntime_candidate(
            &mut candidates,
            "ctx_env_dir",
            path.join(SEMANTIC_ONNXRUNTIME_DYLIB),
            true,
        );
    }
    if let Some(path) = env.cache_dir.as_ref() {
        push_onnxruntime_cache_candidates(&mut candidates, "ctx_runtime_cache", path);
    }
    if let Some(path) = env.runtime_dir.as_ref() {
        push_onnxruntime_cache_candidates(&mut candidates, "ctx_installed_runtime", path);
    }
    if let Some(path) = semantic_onnxruntime_selected_data_root(model_cache_dir) {
        push_onnxruntime_cache_candidates(
            &mut candidates,
            "ctx_selected_data_root_runtime",
            &path.join("runtime"),
        );
    }
    push_onnxruntime_cache_candidates(
        &mut candidates,
        "ctx_default_runtime_cache",
        &semantic_onnxruntime_default_cache_dir(model_cache_dir),
    );
    if let Some(path) = env.exe_dir.as_ref() {
        push_onnxruntime_candidate(
            &mut candidates,
            "exe_dir",
            path.join(SEMANTIC_ONNXRUNTIME_DYLIB),
            false,
        );
        push_onnxruntime_candidate(
            &mut candidates,
            "exe_onnxruntime_platform_dir",
            path.join("onnxruntime")
                .join(semantic_onnxruntime_platform_dir())
                .join(SEMANTIC_ONNXRUNTIME_DYLIB),
            false,
        );
        push_onnxruntime_cache_candidates(&mut candidates, "exe_onnxruntime_cache", path);
        push_onnxruntime_candidate(
            &mut candidates,
            "exe_onnxruntime_dir",
            path.join("onnxruntime").join(SEMANTIC_ONNXRUNTIME_DYLIB),
            false,
        );
        push_onnxruntime_candidate(
            &mut candidates,
            "exe_lib_dir",
            path.join("lib").join(SEMANTIC_ONNXRUNTIME_DYLIB),
            false,
        );
        if let Some(parent) = path.parent() {
            push_onnxruntime_candidate(
                &mut candidates,
                "install_lib_dir",
                parent.join("lib").join(SEMANTIC_ONNXRUNTIME_DYLIB),
                false,
            );
        }
    }
    candidates
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_onnxruntime_selected_data_root(model_cache_dir: &Path) -> Option<&Path> {
    (model_cache_dir.file_name().and_then(|name| name.to_str())
        == Some("semantic-model-cache"))
    .then(|| model_cache_dir.parent())
    .flatten()
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_onnxruntime_default_cache_dir(model_cache_dir: &Path) -> PathBuf {
    if let Some(parent) = semantic_onnxruntime_selected_data_root(model_cache_dir) {
        return parent.join("semantic-runtime");
    }
    model_cache_dir.join("semantic-runtime")
}

#[cfg(ctx_semantic_fastembed)]
fn push_onnxruntime_cache_candidates(
    candidates: &mut Vec<SemanticOnnxRuntimeCandidate>,
    source: &'static str,
    root: &Path,
) {
    let platform = semantic_onnxruntime_platform_dir();
    push_onnxruntime_candidate(
        candidates,
        source,
        root.join("onnxruntime")
            .join(SEMANTIC_ONNXRUNTIME_VERSION)
            .join(platform)
            .join("lib")
            .join(SEMANTIC_ONNXRUNTIME_DYLIB),
        false,
    );
    push_onnxruntime_candidate(
        candidates,
        source,
        root.join("onnxruntime")
            .join(SEMANTIC_ONNXRUNTIME_VERSION)
            .join(platform)
            .join(SEMANTIC_ONNXRUNTIME_DYLIB),
        false,
    );
    push_onnxruntime_candidate(
        candidates,
        source,
        root.join(platform).join(SEMANTIC_ONNXRUNTIME_DYLIB),
        false,
    );
    push_onnxruntime_candidate(
        candidates,
        source,
        root.join(SEMANTIC_ONNXRUNTIME_DYLIB),
        false,
    );
}

#[cfg(ctx_semantic_fastembed)]
fn push_onnxruntime_candidate(
    candidates: &mut Vec<SemanticOnnxRuntimeCandidate>,
    source: &'static str,
    path: PathBuf,
    try_even_if_missing: bool,
) {
    if path.is_absolute()
        && !candidates
            .iter()
            .any(|candidate| candidate.path == path)
    {
        candidates.push(SemanticOnnxRuntimeCandidate {
            source,
            path,
            try_even_if_missing,
        });
    }
}

#[cfg(all(ctx_semantic_fastembed, target_os = "linux", target_arch = "x86_64"))]
fn semantic_onnxruntime_platform_dir() -> &'static str {
    "linux-x64"
}

#[cfg(all(ctx_semantic_fastembed, target_os = "linux", target_arch = "aarch64"))]
fn semantic_onnxruntime_platform_dir() -> &'static str {
    "linux-aarch64"
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos", target_arch = "x86_64"))]
fn semantic_onnxruntime_platform_dir() -> &'static str {
    "macos-x64"
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos", target_arch = "aarch64"))]
fn semantic_onnxruntime_platform_dir() -> &'static str {
    "macos-arm64"
}

#[cfg(all(ctx_semantic_fastembed, target_os = "windows", target_arch = "x86_64"))]
fn semantic_onnxruntime_platform_dir() -> &'static str {
    "windows-x64"
}

#[cfg(all(ctx_semantic_fastembed, target_os = "freebsd", target_arch = "x86_64"))]
fn semantic_onnxruntime_platform_dir() -> &'static str {
    "freebsd-x64"
}

#[cfg(test)]
#[cfg(ctx_semantic_fastembed)]
mod ort_runtime_tests {
    use super::*;

    fn test_absolute_path(path: &str) -> PathBuf {
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\ctx-test")
        } else {
            PathBuf::from("/tmp/ctx-test")
        };
        root.join(path)
    }

    #[test]
    fn onnxruntime_candidates_prefer_explicit_dylib_env() {
        let env = SemanticOnnxRuntimeEnv {
            ctx_dylib: Some(test_absolute_path("custom").join(SEMANTIC_ONNXRUNTIME_DYLIB)),
            ort_dylib: Some(test_absolute_path("ort").join(SEMANTIC_ONNXRUNTIME_DYLIB)),
            ctx_dir: Some(test_absolute_path("ctx-dir")),
            cache_dir: None,
            runtime_dir: None,
            exe_dir: None,
        };
        let candidates = semantic_onnxruntime_candidates(&test_absolute_path("model-cache"), &env);
        assert_eq!(candidates[0].source, "ctx_env_dylib");
        assert_eq!(candidates[1].source, "ort_env_dylib");
        assert_eq!(candidates[2].source, "ctx_env_dir");
        assert!(candidates[0].try_even_if_missing);
    }

    #[test]
    fn onnxruntime_load_candidates_do_not_fallback_from_explicit_dylib() {
        let explicit = test_absolute_path("explicit").join(SEMANTIC_ONNXRUNTIME_DYLIB);
        let env = SemanticOnnxRuntimeEnv {
            ctx_dylib: Some(explicit.clone()),
            ort_dylib: Some(test_absolute_path("ort").join(SEMANTIC_ONNXRUNTIME_DYLIB)),
            ctx_dir: Some(test_absolute_path("ctx-dir")),
            cache_dir: Some(test_absolute_path("cache")),
            runtime_dir: Some(test_absolute_path("runtime")),
            exe_dir: Some(test_absolute_path("bin")),
        };
        let candidates =
            semantic_onnxruntime_load_candidates(&test_absolute_path("model-cache"), &env);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source, "ctx_env_dylib");
        assert_eq!(candidates[0].path, explicit);
    }

    #[test]
    fn onnxruntime_candidates_include_platform_cache_dir() {
        let env = SemanticOnnxRuntimeEnv {
            cache_dir: Some(test_absolute_path("runtime-cache")),
            ..SemanticOnnxRuntimeEnv::default()
        };
        let candidates = semantic_onnxruntime_candidates(&test_absolute_path("model-cache"), &env);
        assert!(candidates.iter().any(|candidate| {
            candidate.path
                == test_absolute_path("runtime-cache")
                    .join("onnxruntime")
                    .join(SEMANTIC_ONNXRUNTIME_VERSION)
                    .join(semantic_onnxruntime_platform_dir())
                    .join("lib")
                    .join(SEMANTIC_ONNXRUNTIME_DYLIB)
        }));
    }

    #[test]
    fn onnxruntime_candidates_include_default_ctx_cache_dir() {
        let model_cache = test_absolute_path("ctx-data/semantic-model-cache");
        let candidates =
            semantic_onnxruntime_candidates(&model_cache, &SemanticOnnxRuntimeEnv::default());
        assert!(candidates.iter().any(|candidate| {
            candidate.path
                == test_absolute_path("ctx-data")
                    .join("semantic-runtime")
                    .join("onnxruntime")
                    .join(SEMANTIC_ONNXRUNTIME_VERSION)
                    .join(semantic_onnxruntime_platform_dir())
                    .join("lib")
                    .join(SEMANTIC_ONNXRUNTIME_DYLIB)
        }));
    }

    #[test]
    fn onnxruntime_candidates_include_selected_data_root_upgrade_dir() {
        let model_cache = test_absolute_path("custom-data-root/semantic-model-cache");
        let candidates =
            semantic_onnxruntime_candidates(&model_cache, &SemanticOnnxRuntimeEnv::default());
        assert!(candidates.iter().any(|candidate| {
            candidate.source == "ctx_selected_data_root_runtime"
                && candidate.path
                    == test_absolute_path("custom-data-root")
                        .join("runtime")
                        .join("onnxruntime")
                        .join(SEMANTIC_ONNXRUNTIME_VERSION)
                        .join(semantic_onnxruntime_platform_dir())
                        .join("lib")
                        .join(SEMANTIC_ONNXRUNTIME_DYLIB)
        }));
    }

    #[test]
    fn onnxruntime_candidates_include_installer_runtime_dir() {
        let env = SemanticOnnxRuntimeEnv {
            runtime_dir: Some(test_absolute_path("ctx-runtime")),
            ..SemanticOnnxRuntimeEnv::default()
        };
        let candidates = semantic_onnxruntime_candidates(&test_absolute_path("model-cache"), &env);
        assert!(candidates.iter().any(|candidate| {
            candidate.path
                == test_absolute_path("ctx-runtime")
                    .join("onnxruntime")
                    .join(SEMANTIC_ONNXRUNTIME_VERSION)
                    .join(semantic_onnxruntime_platform_dir())
                    .join("lib")
                    .join(SEMANTIC_ONNXRUNTIME_DYLIB)
        }));
    }

    #[test]
    fn onnxruntime_sidecar_paths_document_macos_x64_and_freebsd_layout() {
        assert_eq!(
            PathBuf::from("/cache")
                .join("onnxruntime")
                .join(SEMANTIC_ONNXRUNTIME_VERSION)
                .join("macos-x64")
                .join("lib")
                .join("libonnxruntime.dylib"),
            PathBuf::from("/cache/onnxruntime/1.27.0/macos-x64/lib/libonnxruntime.dylib")
        );
        assert_eq!(
            PathBuf::from("/cache")
                .join("onnxruntime")
                .join(SEMANTIC_ONNXRUNTIME_VERSION)
                .join("freebsd-x64")
                .join("lib")
                .join("libonnxruntime.so"),
            PathBuf::from("/cache/onnxruntime/1.27.0/freebsd-x64/lib/libonnxruntime.so")
        );
    }

    #[test]
    fn onnxruntime_candidates_deduplicate_paths() {
        let dylib = test_absolute_path("onnxruntime").join(SEMANTIC_ONNXRUNTIME_DYLIB);
        let env = SemanticOnnxRuntimeEnv {
            ctx_dylib: Some(dylib.clone()),
            ort_dylib: Some(dylib.clone()),
            ..SemanticOnnxRuntimeEnv::default()
        };
        let candidates = semantic_onnxruntime_candidates(Path::new(""), &env);
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| candidate.path == dylib)
                .count(),
            1
        );
    }

    #[test]
    fn onnxruntime_candidates_reject_relative_paths() {
        let env = SemanticOnnxRuntimeEnv {
            ctx_dylib: Some(PathBuf::from(SEMANTIC_ONNXRUNTIME_DYLIB)),
            ort_dylib: Some(PathBuf::from("runtime").join(SEMANTIC_ONNXRUNTIME_DYLIB)),
            ctx_dir: Some(PathBuf::from("runtime")),
            cache_dir: Some(PathBuf::from("cache")),
            runtime_dir: Some(PathBuf::from("runtime")),
            exe_dir: Some(PathBuf::from("bin")),
        };

        assert!(semantic_onnxruntime_candidates(Path::new("model-cache"), &env).is_empty());
        assert!(semantic_onnxruntime_load_candidates(Path::new("model-cache"), &env).is_empty());
    }
}
