use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SemanticBackendPreference {
    #[default]
    Auto,
    Cpu,
    CoreMl,
    Cuda,
    WindowsMl,
}

impl SemanticBackendPreference {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::CoreMl => "coreml",
            Self::Cuda => "cuda",
            Self::WindowsMl => "windowsml",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SemanticCoreMlComputeMode {
    #[default]
    All,
    CpuAndNeuralEngine,
    CpuAndGpu,
    CpuOnly,
}

#[derive(Clone, Debug)]
pub struct SemanticOnnxRuntimePaths {
    pub(crate) ctx_dylib: Option<PathBuf>,
    pub(crate) ort_dylib: Option<PathBuf>,
    pub(crate) ctx_dir: Option<PathBuf>,
    pub(crate) cache_dir: Option<PathBuf>,
    pub(crate) installed_runtime_dir: Option<PathBuf>,
    pub(crate) selected_data_root_runtime_dir: Option<PathBuf>,
    pub(crate) default_runtime_cache_dir: PathBuf,
    pub(crate) executable_dir: Option<PathBuf>,
}

impl SemanticOnnxRuntimePaths {
    pub fn new(default_runtime_cache_dir: PathBuf) -> Self {
        Self {
            ctx_dylib: None,
            ort_dylib: None,
            ctx_dir: None,
            cache_dir: None,
            installed_runtime_dir: None,
            selected_data_root_runtime_dir: None,
            default_runtime_cache_dir,
            executable_dir: None,
        }
    }

    pub fn with_ctx_dylib(mut self, path: Option<PathBuf>) -> Self {
        self.ctx_dylib = path;
        self
    }

    pub fn with_ort_dylib(mut self, path: Option<PathBuf>) -> Self {
        self.ort_dylib = path;
        self
    }

    pub fn with_ctx_dir(mut self, path: Option<PathBuf>) -> Self {
        self.ctx_dir = path;
        self
    }

    pub fn with_cache_dir(mut self, path: Option<PathBuf>) -> Self {
        self.cache_dir = path;
        self
    }

    pub fn with_installed_runtime_dir(mut self, path: Option<PathBuf>) -> Self {
        self.installed_runtime_dir = path;
        self
    }

    pub fn with_selected_data_root_runtime_dir(mut self, path: Option<PathBuf>) -> Self {
        self.selected_data_root_runtime_dir = path;
        self
    }

    pub fn with_executable_dir(mut self, path: Option<PathBuf>) -> Self {
        self.executable_dir = path;
        self
    }
}

#[cfg(test)]
impl Default for SemanticOnnxRuntimePaths {
    fn default() -> Self {
        Self::new(PathBuf::new())
    }
}

#[derive(Clone, Debug)]
pub struct SemanticModelPaths {
    model_cache_dir: PathBuf,
    onnx_runtime: SemanticOnnxRuntimePaths,
}

impl SemanticModelPaths {
    pub fn new(model_cache_dir: PathBuf, onnx_runtime: SemanticOnnxRuntimePaths) -> Self {
        Self {
            model_cache_dir,
            onnx_runtime,
        }
    }

    pub fn model_cache_dir(&self) -> &Path {
        &self.model_cache_dir
    }

    pub(crate) fn onnx_runtime(&self) -> &SemanticOnnxRuntimePaths {
        &self.onnx_runtime
    }
}

#[derive(Clone, Debug)]
pub struct SemanticModelConfig {
    paths: SemanticModelPaths,
    backend_preference: SemanticBackendPreference,
    backend_preference_error: Option<String>,
    coreml_compute_mode: SemanticCoreMlComputeMode,
    coreml_compute_mode_error: Option<String>,
    thread_override: Option<usize>,
    batch_size_override: Option<usize>,
    deprecated_model_onnx_present: bool,
}

impl SemanticModelConfig {
    pub fn new(paths: SemanticModelPaths) -> Self {
        Self {
            paths,
            backend_preference: SemanticBackendPreference::Auto,
            backend_preference_error: None,
            coreml_compute_mode: SemanticCoreMlComputeMode::All,
            coreml_compute_mode_error: None,
            thread_override: None,
            batch_size_override: None,
            deprecated_model_onnx_present: false,
        }
    }

    pub fn with_backend_preference(mut self, preference: SemanticBackendPreference) -> Self {
        self.backend_preference = preference;
        self.backend_preference_error = None;
        self
    }

    pub fn with_backend_preference_error(mut self, error: String) -> Self {
        self.backend_preference_error = Some(error);
        self
    }

    pub fn with_coreml_compute_mode(mut self, mode: SemanticCoreMlComputeMode) -> Self {
        self.coreml_compute_mode = mode;
        self.coreml_compute_mode_error = None;
        self
    }

    pub fn with_coreml_compute_mode_error(mut self, error: String) -> Self {
        self.coreml_compute_mode_error = Some(error);
        self
    }

    pub fn with_thread_override(mut self, threads: Option<usize>) -> Self {
        self.thread_override = threads;
        self
    }

    pub fn with_batch_size_override(mut self, batch_size: Option<usize>) -> Self {
        self.batch_size_override = batch_size;
        self
    }

    pub fn with_deprecated_model_onnx_present(mut self, present: bool) -> Self {
        self.deprecated_model_onnx_present = present;
        self
    }

    pub fn paths(&self) -> &SemanticModelPaths {
        &self.paths
    }

    pub(crate) fn backend_preference(&self) -> Result<SemanticBackendPreference> {
        self.backend_preference_error
            .as_ref()
            .map_or(Ok(self.backend_preference), |error| {
                Err(anyhow!(error.clone()))
            })
    }

    #[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
    pub(crate) fn coreml_compute_mode(&self) -> Result<SemanticCoreMlComputeMode> {
        self.coreml_compute_mode_error
            .as_ref()
            .map_or(Ok(self.coreml_compute_mode), |error| {
                Err(anyhow!(error.clone()))
            })
    }

    pub(crate) const fn thread_override(&self) -> Option<usize> {
        self.thread_override
    }

    pub(crate) const fn batch_size_override(&self) -> Option<usize> {
        self.batch_size_override
    }

    pub(crate) const fn deprecated_model_onnx_present(&self) -> bool {
        self.deprecated_model_onnx_present
    }
}
