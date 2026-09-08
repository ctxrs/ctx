use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    ctx_semantic_fastembed,
    test
))]
use std::fs;

use crate::configuration::{SemanticModelPaths, SemanticOnnxRuntimePaths};
use anyhow::{anyhow, Context, Result};

#[cfg(ctx_semantic_fastembed)]
mod cuda_dependencies;
mod local_install;
pub use local_install::{
    detected_accelerator_backend, identify_runtime_archive, install_operator_runtime,
    installed_runtime_report, supported_local_runtime_backends, SemanticRuntimeBackend,
    SemanticRuntimeInstall, SemanticRuntimeReport,
};
#[cfg(ctx_semantic_fastembed)]
mod verified_runtime;
#[cfg(ctx_semantic_fastembed)]
pub(super) use verified_runtime::{
    installed_accelerator_runtime_identity, revalidate_loaded_accelerator_runtime,
};
#[cfg(ctx_semantic_fastembed)]
use verified_runtime::{
    runtime_install_claim, validate_runtime_candidate, verified_runtime_candidates,
    RuntimeInstallClaim, RUNTIME_INSTALL_MANIFEST,
};

#[cfg(ctx_semantic_fastembed)]
pub(super) const SEMANTIC_ONNXRUNTIME_VERSION: &str = "1.27.0";
#[cfg(all(ctx_semantic_fastembed, target_os = "windows"))]
pub(super) const SEMANTIC_ONNXRUNTIME_DYLIB: &str = "onnxruntime.dll";
#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) const SEMANTIC_ONNXRUNTIME_DYLIB: &str = "libonnxruntime.dylib";
#[cfg(all(
    ctx_semantic_fastembed,
    not(target_os = "windows"),
    not(target_os = "macos")
))]
pub(super) const SEMANTIC_ONNXRUNTIME_DYLIB: &str = "libonnxruntime.so";

/// The CPU library as the sidecar, its manifest, and the file contract spell
/// it. Kept beside the bare library name because the contract needs a `'static`
/// path and `concat!` cannot join a `const`; the two are asserted equal in
/// `cpu_runtime_contract_mirrors_the_published_sidecar`.
#[cfg(all(ctx_semantic_fastembed, target_os = "windows"))]
pub(super) const SEMANTIC_ONNXRUNTIME_LIB_ENTRY: &str = "lib/onnxruntime.dll";
#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) const SEMANTIC_ONNXRUNTIME_LIB_ENTRY: &str = "lib/libonnxruntime.dylib";
#[cfg(all(
    ctx_semantic_fastembed,
    not(target_os = "windows"),
    not(target_os = "macos")
))]
pub(super) const SEMANTIC_ONNXRUNTIME_LIB_ENTRY: &str = "lib/libonnxruntime.so";

#[cfg(ctx_semantic_fastembed)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OnnxRuntimeFlavor {
    Cpu,
    Cuda,
    WindowsMl,
}

#[cfg(ctx_semantic_fastembed)]
impl OnnxRuntimeFlavor {
    pub(super) fn runtime_name(self) -> &'static str {
        match self {
            Self::Cpu | Self::Cuda => "onnxruntime",
            Self::WindowsMl => "windows-ml",
        }
    }

    pub(super) fn version(self) -> &'static str {
        match self {
            Self::Cpu | Self::Cuda => SEMANTIC_ONNXRUNTIME_VERSION,
            Self::WindowsMl => "2.1.74",
        }
    }

    pub(super) fn platform_dir(self) -> Result<&'static str> {
        match self {
            Self::Cpu => Ok(semantic_onnxruntime_platform_dir()),
            Self::Cuda => {
                #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
                {
                    Ok("linux-x64-cuda12")
                }
                #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
                {
                    Err(anyhow!("the CUDA semantic runtime requires Linux x86_64"))
                }
            }
            Self::WindowsMl => {
                #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
                {
                    Ok("windows-x64")
                }
                #[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
                {
                    Err(anyhow!(
                        "the Windows ML semantic runtime requires Windows x86_64"
                    ))
                }
            }
        }
    }

    pub(super) fn asset_name(self) -> &'static str {
        match self {
            Self::Cpu => "ctx-onnxruntime-cpu",
            Self::Cuda => "ctx-onnxruntime-linux-x64-cuda12.tar.zst",
            Self::WindowsMl => "ctx-windowsml-windows-x64.zip",
        }
    }
}

#[cfg(ctx_semantic_fastembed)]
#[derive(Debug, Clone)]
pub(super) struct LoadedOnnxRuntime {
    pub(super) path: PathBuf,
    pub(super) artifact_identity: String,
    flavor: OnnxRuntimeFlavor,
}

#[cfg(ctx_semantic_fastembed)]
static LOADED_RUNTIME: OnceLock<LoadedOnnxRuntime> = OnceLock::new();
#[cfg(ctx_semantic_fastembed)]
static RUNTIME_INIT_LOCK: Mutex<()> = Mutex::new(());

#[cfg(ctx_semantic_fastembed)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SemanticOnnxRuntimeCandidate {
    source: &'static str,
    path: PathBuf,
    try_even_if_missing: bool,
}

#[cfg(ctx_semantic_fastembed)]
fn format_runtime_load_error(error: &ort::LoadDynamicError) -> String {
    // `ort::LoadDynamicError` deliberately does not expose its `Dlopen`
    // libloading error through `Error::source()`, so walk that one public
    // variant explicitly. The libloading source holds the native loader text.
    let native_cause = match error {
        ort::LoadDynamicError::Dlopen { error, .. } => {
            std::error::Error::source(error).map(|native_cause| native_cause.to_string())
        }
        _ => None,
    };
    let mut formatted = error.to_string();
    if let Some(native_cause) =
        native_cause.filter(|native_cause| !formatted.contains(native_cause))
    {
        formatted.push_str(": ");
        formatted.push_str(&native_cause);
    }
    formatted
}

#[cfg(ctx_semantic_fastembed)]
fn format_runtime_load_failure(source: &str, path: &Path, error: &ort::LoadDynamicError) -> String {
    format!(
        "{source} {}: {}",
        path.display(),
        format_runtime_load_error(error)
    )
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn ensure_semantic_onnxruntime_loaded(paths: &SemanticModelPaths) -> Result<PathBuf> {
    if let Some(runtime) = LOADED_RUNTIME.get() {
        if runtime.flavor != OnnxRuntimeFlavor::Cpu {
            return same_verified_runtime(runtime, runtime.flavor).map(|runtime| runtime.path);
        }
        return Ok(runtime.path.clone());
    }
    let _lock = RUNTIME_INIT_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(runtime) = LOADED_RUNTIME.get() {
        if runtime.flavor != OnnxRuntimeFlavor::Cpu {
            return same_verified_runtime(runtime, runtime.flavor).map(|runtime| runtime.path);
        }
        return Ok(runtime.path.clone());
    }
    let runtime = load_semantic_onnxruntime(paths.model_cache_dir(), paths.onnx_runtime())?;
    let path = runtime.path.clone();
    let _ = LOADED_RUNTIME.set(runtime);
    Ok(path)
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn loaded_runtime_artifact_identity() -> Option<String> {
    LOADED_RUNTIME
        .get()
        .map(|runtime| runtime.artifact_identity.clone())
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn ensure_semantic_accelerator_runtime_loaded(
    paths: &SemanticModelPaths,
    flavor: OnnxRuntimeFlavor,
) -> Result<LoadedOnnxRuntime> {
    if flavor == OnnxRuntimeFlavor::Cpu {
        return Err(anyhow!(
            "accelerator runtime loader does not accept the CPU flavor"
        ));
    }
    if let Some(runtime) = LOADED_RUNTIME.get() {
        return same_verified_runtime(runtime, flavor);
    }
    let _lock = RUNTIME_INIT_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(runtime) = LOADED_RUNTIME.get() {
        return same_verified_runtime(runtime, flavor);
    }
    let runtime = load_verified_accelerator_runtime(paths, flavor)?;
    let _ = LOADED_RUNTIME.set(runtime.clone());
    Ok(runtime)
}

#[cfg(ctx_semantic_fastembed)]
fn same_verified_runtime(
    runtime: &LoadedOnnxRuntime,
    requested: OnnxRuntimeFlavor,
) -> Result<LoadedOnnxRuntime> {
    if runtime.flavor != requested {
        return Err(anyhow!(
            "ONNX Runtime was already initialized for {:?}; refusing an in-process switch to {requested:?}",
            runtime.flavor
        ));
    }
    let identity = validate_runtime_candidate(&runtime.path, requested)?;
    if identity != runtime.artifact_identity {
        return Err(anyhow!(
            "verified ONNX Runtime artifact identity changed after loading"
        ));
    }
    Ok(runtime.clone())
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn load_semantic_onnxruntime(
    model_cache_dir: &Path,
    paths: &SemanticOnnxRuntimePaths,
) -> Result<LoadedOnnxRuntime> {
    load_semantic_onnxruntime_with(model_cache_dir, paths, open_semantic_onnxruntime)
}

/// Split from `load_semantic_onnxruntime` so the candidate order, the
/// fall-through rules, and the identity each layout records stay testable
/// without initializing ORT, whose dynamic load is process-global and is only
/// attempted once per process.
#[cfg(ctx_semantic_fastembed)]
fn load_semantic_onnxruntime_with(
    model_cache_dir: &Path,
    paths: &SemanticOnnxRuntimePaths,
    open: impl Fn(&str, &Path) -> Result<(), String>,
) -> Result<LoadedOnnxRuntime> {
    let mut failures = Vec::new();
    if let Some((path, identity)) = preferred_verified_cpu_runtime(paths)? {
        match open(VERIFIED_RUNTIME_SOURCE, &path) {
            Ok(()) => {
                let reloaded = validate_runtime_candidate(&path, OnnxRuntimeFlavor::Cpu)
                    .context("revalidate the verified CPU runtime after dynamic load")?;
                if reloaded != identity {
                    return Err(anyhow!(
                        "verified CPU runtime identity changed during dynamic load"
                    ));
                }
                return Ok(LoadedOnnxRuntime {
                    path,
                    artifact_identity: identity,
                    flavor: OnnxRuntimeFlavor::Cpu,
                });
            }
            Err(failure) => failures.push(failure),
        }
    }
    for candidate in semantic_onnxruntime_load_candidates(model_cache_dir, paths) {
        if !candidate.try_even_if_missing && !candidate.path.exists() {
            continue;
        }
        match open(candidate.source, &candidate.path) {
            Ok(()) => {
                // A plain layout carries no provenance to report: it is the
                // hand-extracted or explicit-metadata install ctx has always
                // accepted, so its identity stays the library path.
                let artifact_identity = format!("legacy-cpu|path={}", candidate.path.display());
                return Ok(LoadedOnnxRuntime {
                    path: candidate.path,
                    artifact_identity,
                    flavor: OnnxRuntimeFlavor::Cpu,
                });
            }
            Err(failure) => failures.push(failure),
        }
    }
    let detail = if failures.is_empty() {
        // The direct-library variables only ever select a CPU runtime: the
        // accelerator loader searches runtime roots for a canonical layout and
        // validates an installer manifest, so advertising them without that
        // distinction sent GPU installs after an unreachable fix.
        format!(
            "no ONNX Runtime dynamic library candidates were found for {platform}; install the ONNX Runtime {version} CPU sidecar with `ctx semantic runtime install`, place it under <runtime-root>/onnxruntime/{version}/{platform}/lib, or set an absolute CPU library path with CTX_ONNXRUNTIME_DYLIB, ORT_DYLIB_PATH, or CTX_ONNXRUNTIME_DIR. Those three variables select a CPU runtime only; an accelerator runtime loads exclusively from an installer-provisioned runtime directory carrying its {manifest} manifest, searched under CTX_RUNTIME_DIR or CTX_ONNXRUNTIME_CACHE_DIR",
            platform = semantic_onnxruntime_platform_dir(),
            version = SEMANTIC_ONNXRUNTIME_VERSION,
            manifest = RUNTIME_INSTALL_MANIFEST,
        )
    } else {
        format!(
            "failed to load ONNX Runtime dynamic library; tried {}",
            failures.join("; ")
        )
    };
    Err(anyhow!(detail))
}

/// Failure label for the verified CPU install, distinct from the plain
/// candidate sources so a load report says which layout was rejected.
#[cfg(ctx_semantic_fastembed)]
const VERIFIED_RUNTIME_SOURCE: &str = "ctx_verified_runtime";

/// The direct-library variables are the operator's own choice of file, so any
/// one of them suppresses every other candidate.
#[cfg(ctx_semantic_fastembed)]
fn explicit_runtime_source(paths: &SemanticOnnxRuntimePaths) -> Option<&'static str> {
    if paths.ctx_dylib.is_some() {
        Some("ctx_env_dylib")
    } else if paths.ort_dylib.is_some() {
        Some("ort_env_dylib")
    } else if paths.ctx_dir.is_some() {
        Some("ctx_env_dir")
    } else {
        None
    }
}

/// Opens a candidate exactly the way this loader always has, so a verified
/// install and a plain one fail with the same detail.
#[cfg(ctx_semantic_fastembed)]
fn open_semantic_onnxruntime(source: &str, path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    if let Err(error) = preload_windows_onnxruntime(path) {
        return Err(format!("{source} {}: {error:#}", path.display()));
    }
    match ort::init_from(path) {
        Ok(builder) => {
            // Losing the commit race is not a load failure for the CPU runtime:
            // this loader has always reported the library it opened.
            let _ = builder.commit();
            Ok(())
        }
        Err(error) => Err(format_runtime_load_failure(source, path, &error)),
    }
}

/// The verified CPU install the loader must prefer, with the identity to report
/// for it, or `None` when the loader falls through to the plain candidate walk.
///
/// An explicit library or directory selection stays exclusive: the operator
/// named the file to open, so ctx must not quietly prefer another install. A
/// runtime directory with no installer manifest is left to the plain walk as
/// well, because hand-extracted sidecars predate local install and have to keep
/// loading. A directory whose manifest claims ctx's own provenance and then
/// fails validation is tamper evidence, so it fails the load outright instead of
/// being skipped quietly.
#[cfg(ctx_semantic_fastembed)]
fn preferred_verified_cpu_runtime(
    paths: &SemanticOnnxRuntimePaths,
) -> Result<Option<(PathBuf, String)>> {
    if explicit_runtime_source(paths).is_some() {
        return Ok(None);
    }
    for path in verified_runtime_candidates(paths, OnnxRuntimeFlavor::Cpu)? {
        let Some(root) = path.parent().and_then(Path::parent) else {
            continue;
        };
        if runtime_install_claim(root) != RuntimeInstallClaim::CtxProvenance {
            continue;
        }
        let identity = validate_runtime_candidate(&path, OnnxRuntimeFlavor::Cpu)
            .with_context(|| format!("validate the installed CPU runtime {}", root.display()))?;
        return Ok(Some((path, identity)));
    }
    Ok(None)
}

#[cfg(ctx_semantic_fastembed)]
fn load_verified_accelerator_runtime(
    paths: &SemanticModelPaths,
    flavor: OnnxRuntimeFlavor,
) -> Result<LoadedOnnxRuntime> {
    let mut failures = Vec::new();
    for path in verified_runtime_candidates(paths.onnx_runtime(), flavor)? {
        if !path.exists() {
            continue;
        }
        let identity = match validate_runtime_candidate(&path, flavor) {
            Ok(identity) => identity,
            Err(error) => {
                failures.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        #[cfg(target_os = "windows")]
        if let Err(error) = preload_windows_onnxruntime(&path) {
            failures.push(format!("{}: {error:#}", path.display()));
            continue;
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        if flavor == OnnxRuntimeFlavor::Cuda {
            if let Err(error) = cuda_dependencies::preload(&path) {
                failures.push(format!("{}: {error}", path.display()));
                continue;
            }
        }
        match ort::init_from(&path) {
            Ok(builder) => {
                let after_load = validate_runtime_candidate(&path, flavor)
                    .context("revalidate accelerator runtime after dynamic load")?;
                if after_load != identity {
                    return Err(anyhow!(
                        "accelerator runtime identity changed during dynamic load"
                    ));
                }
                if !builder.commit() {
                    return Err(anyhow!(
                        "ONNX Runtime was initialized before the verified accelerator runtime"
                    ));
                }
                return Ok(LoadedOnnxRuntime {
                    path,
                    artifact_identity: identity,
                    flavor,
                });
            }
            Err(error) => failures.push(format!(
                "{}: {}",
                path.display(),
                format_runtime_load_error(&error)
            )),
        }
    }
    let detail = if failures.is_empty() {
        format!(
            "verified {} is not installed for {}; run the signed setup or upgrade provisioning flow",
            flavor.asset_name(),
            flavor.platform_dir()?,
        )
    } else {
        format!(
            "verified {} is unusable: {}",
            flavor.asset_name(),
            failures.join("; ")
        )
    };
    Err(anyhow!(detail))
}

#[cfg(all(any(test, feature = "test-support"), ctx_semantic_fastembed))]
#[allow(dead_code)]
pub(crate) fn load_missing_semantic_onnxruntime_for_test(
    model_cache_dir: &Path,
    missing_dylib: &Path,
) -> Result<PathBuf> {
    load_semantic_onnxruntime(
        model_cache_dir,
        &SemanticOnnxRuntimePaths::new(model_cache_dir.join("semantic-runtime"))
            .with_ctx_dylib(Some(missing_dylib.to_path_buf())),
    )
    .map(|runtime| runtime.path)
}

#[cfg(all(ctx_semantic_fastembed, target_os = "windows"))]
pub(super) fn preload_windows_onnxruntime(path: &Path) -> Result<()> {
    use libloading::os::windows::{
        Library as WindowsLibrary, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    };

    static PRELOADED: std::sync::OnceLock<Mutex<Vec<(PathBuf, libloading::Library)>>> =
        std::sync::OnceLock::new();
    let libraries = PRELOADED.get_or_init(|| Mutex::new(Vec::new()));
    let mut libraries = libraries.lock().unwrap_or_else(|error| error.into_inner());
    if libraries.iter().any(|(loaded_path, _)| loaded_path == path) {
        return Ok(());
    }
    let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;
    let library = unsafe { WindowsLibrary::load_with_flags(path, flags) }
        .with_context(|| format!("load packaged ONNX Runtime {}", path.display()))?;
    libraries.push((path.to_path_buf(), library.into()));
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_onnxruntime_load_candidates(
    model_cache_dir: &Path,
    paths: &SemanticOnnxRuntimePaths,
) -> Vec<SemanticOnnxRuntimeCandidate> {
    let mut candidates = semantic_onnxruntime_candidates(model_cache_dir, paths);
    if let Some(source) = explicit_runtime_source(paths) {
        candidates.retain(|candidate| candidate.source == source);
    }
    candidates
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_onnxruntime_candidates(
    _model_cache_dir: &Path,
    paths: &SemanticOnnxRuntimePaths,
) -> Vec<SemanticOnnxRuntimeCandidate> {
    let mut candidates = Vec::new();
    if let Some(path) = paths.ctx_dylib.as_ref() {
        push_onnxruntime_candidate(&mut candidates, "ctx_env_dylib", path.clone(), true);
    }
    if let Some(path) = paths.ort_dylib.as_ref() {
        push_onnxruntime_candidate(&mut candidates, "ort_env_dylib", path.clone(), true);
    }
    if let Some(path) = paths.ctx_dir.as_ref() {
        push_onnxruntime_candidate(
            &mut candidates,
            "ctx_env_dir",
            path.join(SEMANTIC_ONNXRUNTIME_DYLIB),
            true,
        );
    }
    if let Some(path) = paths.cache_dir.as_ref() {
        push_onnxruntime_cache_candidates(&mut candidates, "ctx_runtime_cache", path);
    }
    if let Some(path) = paths.installed_runtime_dir.as_ref() {
        push_onnxruntime_cache_candidates(&mut candidates, "ctx_installed_runtime", path);
    }
    if let Some(path) = paths.selected_data_root_runtime_dir.as_ref() {
        push_onnxruntime_cache_candidates(&mut candidates, "ctx_selected_data_root_runtime", path);
    }
    push_onnxruntime_cache_candidates(
        &mut candidates,
        "ctx_default_runtime_cache",
        &paths.default_runtime_cache_dir,
    );
    if let Some(path) = paths.executable_dir.as_ref() {
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

#[cfg(all(ctx_semantic_fastembed, target_os = "linux", target_arch = "x86_64"))]
pub(super) fn nvidia_accelerator_present() -> bool {
    nvidia_accelerator_present_in(
        Path::new("/proc"),
        Path::new("/sys"),
        Path::new("/dev"),
        Path::new("/usr/lib/wsl/lib"),
    )
}

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
fn nvidia_accelerator_present_in(
    proc_root: &Path,
    sys_root: &Path,
    dev_root: &Path,
    wsl_driver_root: &Path,
) -> bool {
    proc_root.join("driver/nvidia/version").is_file()
        || sys_root.join("module/nvidia").is_dir()
        || fs::read_dir(sys_root.join("class/drm")).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                fs::read_to_string(entry.path().join("device/vendor"))
                    .is_ok_and(|vendor| vendor.trim().eq_ignore_ascii_case("0x10de"))
            })
        })
        || (dev_root.join("dxg").exists() && wsl_driver_root.join("libnvidia-ml.so.1").is_file())
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn push_onnxruntime_cache_candidates(
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
pub(super) fn push_onnxruntime_candidate(
    candidates: &mut Vec<SemanticOnnxRuntimeCandidate>,
    source: &'static str,
    path: PathBuf,
    try_even_if_missing: bool,
) {
    if path.is_absolute() && !candidates.iter().any(|candidate| candidate.path == path) {
        candidates.push(SemanticOnnxRuntimeCandidate {
            source,
            path,
            try_even_if_missing,
        });
    }
}

#[cfg(all(ctx_semantic_fastembed, target_os = "linux", target_arch = "x86_64"))]
pub(super) fn semantic_onnxruntime_platform_dir() -> &'static str {
    "linux-x64"
}

#[cfg(all(ctx_semantic_fastembed, target_os = "linux", target_arch = "aarch64"))]
pub(super) fn semantic_onnxruntime_platform_dir() -> &'static str {
    "linux-aarch64"
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos", target_arch = "x86_64"))]
pub(super) fn semantic_onnxruntime_platform_dir() -> &'static str {
    "macos-x64"
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos", target_arch = "aarch64"))]
pub(super) fn semantic_onnxruntime_platform_dir() -> &'static str {
    "macos-arm64"
}

#[cfg(all(ctx_semantic_fastembed, target_os = "windows", target_arch = "x86_64"))]
pub(super) fn semantic_onnxruntime_platform_dir() -> &'static str {
    "windows-x64"
}

#[cfg(all(ctx_semantic_fastembed, target_os = "freebsd", target_arch = "x86_64"))]
pub(super) fn semantic_onnxruntime_platform_dir() -> &'static str {
    "freebsd-x64"
}

#[cfg(test)]
#[cfg(ctx_semantic_fastembed)]
mod ort_runtime_tests {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    use std::env;

    use super::*;

    #[test]
    fn runtime_load_failure_preserves_real_native_loader_detail() {
        let temp = tempfile::tempdir().unwrap();
        let missing_dylib = temp.path().join(SEMANTIC_ONNXRUNTIME_DYLIB);
        let error = match ort::init_from(&missing_dylib) {
            Ok(_) => panic!("a missing dynamic library should not load"),
            Err(error) => error,
        };
        let native_cause = match &error {
            ort::LoadDynamicError::Dlopen { error, .. } => std::error::Error::source(error)
                .expect("a dynamic loader failure should retain its native cause")
                .to_string(),
            other => panic!("expected a dynamic loader failure, got {other:?}"),
        };
        let loader_display = error.to_string();
        let formatted =
            format_runtime_load_failure("ctx_installed_runtime", &missing_dylib, &error);
        assert!(formatted.contains(&loader_display), "{formatted}");
        assert!(formatted.contains(&native_cause), "{formatted}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn production_candidate_loader_preserves_preload_error_chain() {
        let temp = tempfile::tempdir().unwrap();
        let missing_dylib = temp.path().join(SEMANTIC_ONNXRUNTIME_DYLIB);
        let preload_error = preload_windows_onnxruntime(&missing_dylib)
            .expect_err("a missing ONNX Runtime library should not preload");
        let preload_chain = format!("{preload_error:#}");
        assert_ne!(preload_chain, preload_error.to_string());

        let production_error = load_semantic_onnxruntime(
            &test_absolute_path("model-cache"),
            &SemanticOnnxRuntimePaths {
                ctx_dylib: Some(missing_dylib.clone()),
                ..SemanticOnnxRuntimePaths::default()
            },
        )
        .expect_err("a missing ONNX Runtime library should not load");
        let production_display = production_error.to_string();
        assert!(production_display.contains("failed to load ONNX Runtime dynamic library"));
        assert!(production_display.contains(&missing_dylib.display().to_string()));
        assert!(
            production_display.contains(&preload_chain),
            "{production_display}"
        );
    }

    #[test]
    fn runtime_load_failure_formats_other_ort_variants() {
        let missing_api = ort::LoadDynamicError::MissingApi {
            path: test_absolute_path(SEMANTIC_ONNXRUNTIME_DYLIB),
        };
        let formatted = format_runtime_load_failure(
            "ctx_installed_runtime",
            Path::new("/tmp/libonnxruntime"),
            &missing_api,
        );
        assert_eq!(
            formatted,
            format!("ctx_installed_runtime /tmp/libonnxruntime: {missing_api}")
        );
    }

    fn test_absolute_path(path: &str) -> PathBuf {
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\ctx-test")
        } else {
            PathBuf::from("/tmp/ctx-test")
        };
        root.join(path)
    }

    #[test]
    fn nvidia_probe_accepts_driver_and_drm_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let proc_root = temp.path().join("proc");
        let sys_root = temp.path().join("sys");
        let dev_root = temp.path().join("dev");
        let wsl_driver_root = temp.path().join("usr/lib/wsl/lib");
        fs::create_dir_all(proc_root.join("driver/nvidia")).unwrap();
        fs::write(proc_root.join("driver/nvidia/version"), b"test").unwrap();
        assert!(nvidia_accelerator_present_in(
            &proc_root,
            &sys_root,
            &dev_root,
            &wsl_driver_root,
        ));

        fs::remove_file(proc_root.join("driver/nvidia/version")).unwrap();
        fs::create_dir_all(sys_root.join("class/drm/card0/device")).unwrap();
        fs::write(sys_root.join("class/drm/card0/device/vendor"), b"0x10de\n").unwrap();
        assert!(nvidia_accelerator_present_in(
            &proc_root,
            &sys_root,
            &dev_root,
            &wsl_driver_root,
        ));
    }

    #[test]
    fn nvidia_probe_requires_wsl_gpu_and_nvidia_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let proc_root = temp.path().join("proc");
        let sys_root = temp.path().join("sys");
        let dev_root = temp.path().join("dev");
        let wsl_driver_root = temp.path().join("usr/lib/wsl/lib");
        fs::create_dir_all(&dev_root).unwrap();
        fs::write(dev_root.join("dxg"), b"test").unwrap();
        assert!(!nvidia_accelerator_present_in(
            &proc_root,
            &sys_root,
            &dev_root,
            &wsl_driver_root,
        ));

        fs::remove_file(dev_root.join("dxg")).unwrap();
        fs::create_dir_all(&wsl_driver_root).unwrap();
        fs::write(wsl_driver_root.join("libnvidia-ml.so.1"), b"test").unwrap();
        assert!(!nvidia_accelerator_present_in(
            &proc_root,
            &sys_root,
            &dev_root,
            &wsl_driver_root,
        ));

        fs::write(dev_root.join("dxg"), b"test").unwrap();
        assert!(nvidia_accelerator_present_in(
            &proc_root,
            &sys_root,
            &dev_root,
            &wsl_driver_root,
        ));
    }

    #[test]
    fn onnxruntime_candidates_prefer_explicit_dylib_env() {
        let env = SemanticOnnxRuntimePaths {
            ctx_dylib: Some(test_absolute_path("custom").join(SEMANTIC_ONNXRUNTIME_DYLIB)),
            ort_dylib: Some(test_absolute_path("ort").join(SEMANTIC_ONNXRUNTIME_DYLIB)),
            ctx_dir: Some(test_absolute_path("ctx-dir")),
            cache_dir: None,
            installed_runtime_dir: None,
            selected_data_root_runtime_dir: None,
            default_runtime_cache_dir: test_absolute_path("default-runtime"),
            executable_dir: None,
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
        let env = SemanticOnnxRuntimePaths {
            ctx_dylib: Some(explicit.clone()),
            ort_dylib: Some(test_absolute_path("ort").join(SEMANTIC_ONNXRUNTIME_DYLIB)),
            ctx_dir: Some(test_absolute_path("ctx-dir")),
            cache_dir: Some(test_absolute_path("cache")),
            installed_runtime_dir: Some(test_absolute_path("runtime")),
            selected_data_root_runtime_dir: Some(test_absolute_path("selected-runtime")),
            default_runtime_cache_dir: test_absolute_path("default-runtime"),
            executable_dir: Some(test_absolute_path("bin")),
        };
        let candidates =
            semantic_onnxruntime_load_candidates(&test_absolute_path("model-cache"), &env);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source, "ctx_env_dylib");
        assert_eq!(candidates[0].path, explicit);
    }

    #[test]
    fn onnxruntime_candidates_include_platform_cache_dir() {
        let env = SemanticOnnxRuntimePaths {
            cache_dir: Some(test_absolute_path("runtime-cache")),
            ..SemanticOnnxRuntimePaths::default()
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
        let paths =
            SemanticOnnxRuntimePaths::new(test_absolute_path("ctx-data").join("semantic-runtime"));
        let candidates = semantic_onnxruntime_candidates(&model_cache, &paths);
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
        let paths = SemanticOnnxRuntimePaths::new(test_absolute_path("default-runtime"))
            .with_selected_data_root_runtime_dir(Some(
                test_absolute_path("custom-data-root").join("runtime"),
            ));
        let candidates = semantic_onnxruntime_candidates(&model_cache, &paths);
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
        let env = SemanticOnnxRuntimePaths {
            installed_runtime_dir: Some(test_absolute_path("ctx-runtime")),
            ..SemanticOnnxRuntimePaths::default()
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
        let env = SemanticOnnxRuntimePaths {
            ctx_dylib: Some(dylib.clone()),
            ort_dylib: Some(dylib.clone()),
            ..SemanticOnnxRuntimePaths::default()
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
        let env = SemanticOnnxRuntimePaths {
            ctx_dylib: Some(PathBuf::from(SEMANTIC_ONNXRUNTIME_DYLIB)),
            ort_dylib: Some(PathBuf::from("runtime").join(SEMANTIC_ONNXRUNTIME_DYLIB)),
            ctx_dir: Some(PathBuf::from("runtime")),
            cache_dir: Some(PathBuf::from("cache")),
            installed_runtime_dir: Some(PathBuf::from("runtime")),
            selected_data_root_runtime_dir: Some(PathBuf::from("selected-runtime")),
            default_runtime_cache_dir: PathBuf::from("default-runtime"),
            executable_dir: Some(PathBuf::from("bin")),
        };

        assert!(semantic_onnxruntime_candidates(Path::new("model-cache"), &env).is_empty());
        assert!(semantic_onnxruntime_load_candidates(Path::new("model-cache"), &env).is_empty());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn onnxruntime_invalid_shared_library_fails_without_deadlock() {
        const CHILD_ENV: &str = "CTX_TEST_INVALID_ONNXRUNTIME_CHILD";
        const CHILD_MARKER_ENV: &str = "CTX_TEST_INVALID_ONNXRUNTIME_CHILD_MARKER";
        const TEST_NAME: &str =
            "model_runtime::onnx::ort_runtime_tests::onnxruntime_invalid_shared_library_fails_without_deadlock";
        let library = [
            "/lib/x86_64-linux-gnu/libm.so.6",
            "/usr/lib/x86_64-linux-gnu/libm.so.6",
            "/lib64/libm.so.6",
            "/usr/lib64/libm.so.6",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .expect("a loadable system library is required for the ORT loader regression test");

        if env::var_os(CHILD_ENV).is_some() {
            let marker = env::var_os(CHILD_MARKER_ENV)
                .map(PathBuf::from)
                .expect("isolated ORT loader regression child marker");
            fs::write(&marker, TEST_NAME).expect("record isolated ORT loader regression child");
            let error = load_semantic_onnxruntime(
                &test_absolute_path("model-cache"),
                &SemanticOnnxRuntimePaths {
                    ctx_dylib: Some(library),
                    ..SemanticOnnxRuntimePaths::default()
                },
            )
            .expect_err("a non-ORT shared library must be rejected");
            assert!(
                format!("{error:#}").contains("OrtGetApiBase"),
                "unexpected ORT loader error: {error:#}"
            );
            return;
        }

        let marker_dir = tempfile::tempdir().expect("create ORT loader child marker directory");
        let marker = marker_dir.path().join("child-ran");
        let mut child =
            std::process::Command::new(env::current_exe().expect("current test binary"))
                .args(["--exact", TEST_NAME, "--nocapture"])
                .env(CHILD_ENV, "1")
                .env(CHILD_MARKER_ENV, &marker)
                .spawn()
                .expect("spawn isolated ORT loader regression test");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().expect("poll ORT loader regression test") {
                assert!(
                    status.success(),
                    "isolated ORT loader regression test failed"
                );
                assert_eq!(
                    fs::read_to_string(&marker)
                        .expect("the intended isolated ORT loader child test must run"),
                    TEST_NAME,
                    "the isolated test selector ran a different child test"
                );
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("ORT loader deadlocked while rejecting a non-ORT shared library");
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn stage_cpu_runtime_files(runtime_root: &Path) -> (PathBuf, Vec<serde_json::Value>) {
        use sha2::Digest as _;

        let root = runtime_root
            .join("onnxruntime")
            .join(SEMANTIC_ONNXRUNTIME_VERSION)
            .join(semantic_onnxruntime_platform_dir());
        fs::create_dir_all(root.join("lib")).unwrap();
        ctx_history_platform::platform_security::restrict_private_directory(&root).unwrap();
        let mut records = Vec::new();
        for relative in verified_runtime::expected_runtime_files(OnnxRuntimeFlavor::Cpu) {
            let bytes = format!("ctx-cpu-runtime::{relative}").into_bytes();
            fs::write(root.join(relative), &bytes).unwrap();
            records.push(serde_json::json!({
                "path": relative,
                "size": bytes.len(),
                "sha256": format!("{:x}", sha2::Sha256::digest(&bytes)),
            }));
        }
        (root, records)
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn verified_cpu_manifest(records: &[serde_json::Value]) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "manager": "ctx-local-operator",
            "metadata_trust": "operator-pinned-digest",
            "runtime": "onnxruntime",
            "platform": semantic_onnxruntime_platform_dir(),
            "version": SEMANTIC_ONNXRUNTIME_VERSION,
            "sha256": "e".repeat(64),
            "artifact_url": "file:///tmp/ctx-onnxruntime-linux-x64.tar.zst",
            "installed_at": "2026-09-08T00:00:00Z",
            "files": records,
        })
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn write_cpu_manifest(root: &Path, manifest: &serde_json::Value) {
        fs::write(
            root.join(RUNTIME_INSTALL_MANIFEST),
            serde_json::to_vec(manifest).unwrap(),
        )
        .unwrap();
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn cpu_runtime_paths(runtime_root: PathBuf) -> SemanticOnnxRuntimePaths {
        SemanticOnnxRuntimePaths {
            cache_dir: Some(runtime_root),
            ..SemanticOnnxRuntimePaths::default()
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn verified_cpu_install_is_preferred_and_carries_its_provenance() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime_root = temporary.path().join("runtime");
        let (root, records) = stage_cpu_runtime_files(&runtime_root);
        write_cpu_manifest(&root, &verified_cpu_manifest(&records));
        let paths = cpu_runtime_paths(runtime_root);

        let (library, identity) = preferred_verified_cpu_runtime(&paths)
            .unwrap()
            .expect("the verified CPU install must be selected");
        assert_eq!(library, root.join("lib").join(SEMANTIC_ONNXRUNTIME_DYLIB));
        assert!(
            identity.starts_with("onnxruntime|platform=linux-x64|version=1.27.0|"),
            "{identity}"
        );
        assert!(
            identity.contains("manager=ctx-local-operator|metadata_trust=operator-pinned-digest"),
            "{identity}"
        );
        assert_ne!(identity, format!("legacy-cpu|path={}", library.display()));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn plain_cpu_layouts_keep_loading_without_a_verified_manifest() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime_root = temporary.path().join("runtime");
        let (root, _) = stage_cpu_runtime_files(&runtime_root);
        let library = root.join("lib").join(SEMANTIC_ONNXRUNTIME_DYLIB);
        let paths = cpu_runtime_paths(runtime_root);
        let model_cache = test_absolute_path("model-cache");

        assert!(preferred_verified_cpu_runtime(&paths).unwrap().is_none());

        // The direct-release installers write a manifest of their own that
        // claims no ctx provenance and carries no per-file records. Those
        // installs must keep loading exactly as they always have.
        write_cpu_manifest(&root, &explicit_metadata_manifest());
        assert!(preferred_verified_cpu_runtime(&paths).unwrap().is_none());

        let candidates = semantic_onnxruntime_load_candidates(&model_cache, &paths);
        let plain = candidates
            .iter()
            .find(|candidate| candidate.path == library)
            .expect("the plain cache layout stays a candidate");
        assert_eq!(plain.source, "ctx_runtime_cache");
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn tampered_verified_cpu_manifest_fails_the_load_instead_of_falling_back() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime_root = temporary.path().join("runtime");
        let (root, mut records) = stage_cpu_runtime_files(&runtime_root);
        records[0]["sha256"] = serde_json::json!("a".repeat(64));
        write_cpu_manifest(&root, &verified_cpu_manifest(&records));
        let paths = cpu_runtime_paths(runtime_root);

        let selection = preferred_verified_cpu_runtime(&paths).unwrap_err();
        let selection = format!("{selection:#}");
        assert!(
            selection.contains("validate the installed CPU runtime"),
            "{selection}"
        );
        assert!(selection.contains("SHA-256 does not match"), "{selection}");

        let load = load_semantic_onnxruntime(&test_absolute_path("model-cache"), &paths)
            .expect_err("tamper evidence must not fall back to the plain layout");
        let load = format!("{load:#}");
        assert!(
            load.contains("validate the installed CPU runtime"),
            "{load}"
        );
        assert!(
            !load.contains("failed to load ONNX Runtime dynamic library"),
            "{load}"
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn explicit_dylib_selection_still_wins_over_a_verified_install() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime_root = temporary.path().join("runtime");
        let (root, records) = stage_cpu_runtime_files(&runtime_root);
        write_cpu_manifest(&root, &verified_cpu_manifest(&records));
        let verified = root.join("lib").join(SEMANTIC_ONNXRUNTIME_DYLIB);
        let explicit = temporary
            .path()
            .join("explicit")
            .join(SEMANTIC_ONNXRUNTIME_DYLIB);
        let paths = SemanticOnnxRuntimePaths {
            ctx_dylib: Some(explicit.clone()),
            ..cpu_runtime_paths(runtime_root)
        };

        // The verified install is present and would otherwise be preferred, so
        // this proves the explicit selection stays exclusive.
        assert!(preferred_verified_cpu_runtime(&paths).unwrap().is_none());
        let candidates =
            semantic_onnxruntime_load_candidates(&test_absolute_path("model-cache"), &paths);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, explicit);
        assert_ne!(candidates[0].path, verified);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn a_verified_cpu_install_is_never_accepted_as_the_cuda_runtime() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime_root = temporary.path().join("runtime");
        let (root, records) = stage_cpu_runtime_files(&runtime_root);
        write_cpu_manifest(&root, &verified_cpu_manifest(&records));
        let library = root.join("lib").join(SEMANTIC_ONNXRUNTIME_DYLIB);
        let paths = cpu_runtime_paths(runtime_root.clone());

        let error = validate_runtime_candidate(&library, OnnxRuntimeFlavor::Cuda)
            .expect_err("a CPU install must not satisfy the CUDA contract")
            .to_string();
        assert!(error.contains("linux-x64-cuda12"), "{error}");
        let expected = runtime_root
            .join("onnxruntime")
            .join(SEMANTIC_ONNXRUNTIME_VERSION)
            .join("linux-x64-cuda12")
            .join("lib")
            .join(SEMANTIC_ONNXRUNTIME_DYLIB);
        let cuda = verified_runtime_candidates(&paths, OnnxRuntimeFlavor::Cuda).unwrap();
        assert_eq!(cuda, [expected]);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn explicit_metadata_manifest() -> serde_json::Value {
        // Exactly what scripts/dev-install-from-metadata.sh and
        // scripts/install.ps1 write next to a direct-release CPU sidecar: a
        // manager ctx does not verify and no per-file records at all.
        serde_json::json!({
            "schema_version": 1,
            "manager": "ctx-explicit-metadata-installer",
            "metadata_trust": "explicit-unsigned",
            "runtime": "onnxruntime",
            "platform": "linux-x64",
            "version": SEMANTIC_ONNXRUNTIME_VERSION,
            "sha256": "f".repeat(64),
            "artifact_url": "https://cli.ctx.rs/ctx-onnxruntime-linux-x64.tar.gz",
            "installed_at": "2026-09-08T00:00:00Z",
        })
    }

    /// A recorded load must name the layout it came from: a verified install
    /// reports its manifest provenance, and everything else keeps the bare path
    /// identity ctx has always reported. The opener is injected because ORT's
    /// dynamic load is process-global and attempted only once per process.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn each_cpu_layout_records_its_own_artifact_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime_root = temporary.path().join("runtime");
        let (root, records) = stage_cpu_runtime_files(&runtime_root);
        let library = root.join("lib").join(SEMANTIC_ONNXRUNTIME_DYLIB);
        let model_cache = test_absolute_path("model-cache");
        let opened = std::cell::RefCell::new(Vec::new());
        let open = |source: &str, path: &Path| -> Result<(), String> {
            opened
                .borrow_mut()
                .push((source.to_owned(), path.to_path_buf()));
            Ok(())
        };

        write_cpu_manifest(&root, &verified_cpu_manifest(&records));
        let paths = cpu_runtime_paths(runtime_root.clone());
        let verified = load_semantic_onnxruntime_with(&model_cache, &paths, &open).unwrap();
        assert_eq!(verified.path, library);
        assert!(
            verified
                .artifact_identity
                .contains("manager=ctx-local-operator|metadata_trust=operator-pinned-digest"),
            "{}",
            verified.artifact_identity
        );
        assert_eq!(
            opened.take(),
            vec![("ctx_verified_runtime".to_owned(), library.clone())]
        );

        // A direct-release install carries an installer manifest ctx does not
        // verify, so it keeps loading with the identity it always reported.
        write_cpu_manifest(&root, &explicit_metadata_manifest());
        let plain = load_semantic_onnxruntime_with(&model_cache, &paths, &open).unwrap();
        assert_eq!(plain.path, library);
        assert_eq!(
            plain.artifact_identity,
            format!("legacy-cpu|path={}", library.display())
        );
        assert_eq!(
            opened.take(),
            vec![("ctx_runtime_cache".to_owned(), library.clone())]
        );

        // An explicit selection stays exclusive even with a verified install
        // in the same runtime root.
        write_cpu_manifest(&root, &verified_cpu_manifest(&records));
        let explicit = temporary.path().join(SEMANTIC_ONNXRUNTIME_DYLIB);
        fs::write(&explicit, b"ctx-cpu-runtime::explicit").unwrap();
        let selected = SemanticOnnxRuntimePaths {
            ctx_dylib: Some(explicit.clone()),
            ..cpu_runtime_paths(runtime_root)
        };
        let chosen = load_semantic_onnxruntime_with(&model_cache, &selected, &open).unwrap();
        assert_eq!(chosen.path, explicit);
        assert_eq!(
            chosen.artifact_identity,
            format!("legacy-cpu|path={}", explicit.display())
        );
        assert_eq!(opened.take(), vec![("ctx_env_dylib".to_owned(), explicit)]);
    }
}
