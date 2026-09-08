//! Operator-provisioned semantic runtime installation.
//!
//! ctx normally provisions the ONNX Runtime sidecar through the hosted
//! installer, which carries signed release metadata. Direct-release installs
//! have no signed runtime metadata at all, so this module lets an operator
//! install a sidecar archive whose digest they pinned by hand. The digest is
//! verified before a single byte is extracted, the staged tree must equal the
//! in-binary file contract exactly, and the published install is handed back to
//! the loader's own validator before the install is reported as successful.
//! Nothing here relaxes per-file verification: the manifest written is the same
//! shape the loader deserializes, and every file it declares is measured from
//! the staged bytes.
//!
//! Which runtime an archive carries is read from the archive itself: every
//! flavor's member set is unique, so the contents identify it. That happens
//! only after the pinned digest settles, and never from the file name or from
//! the operator-supplied `.asset.json` sidecar, neither of which the digest
//! covers.

use std::path::{Path, PathBuf};

/// Runtimes ctx can install from a local archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticRuntimeBackend {
    Cpu,
    Cuda,
    WindowsMl,
}

impl SemanticRuntimeBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::WindowsMl => "windowsml",
        }
    }
}

/// An operator's request to install a sidecar archive they already hold.
#[derive(Debug, Clone, Copy)]
pub struct SemanticRuntimeInstall<'a> {
    pub archive: &'a Path,
    pub expected_archive_sha256: &'a str,
    pub runtime_root: &'a Path,
    /// Which runtime the archive must contain. Every sidecar names its own
    /// files, so `None` lets the archive decide and an operator never has to
    /// know which flavor they downloaded. `Some` is an assertion, not a
    /// selection: it fails a mismatch instead of installing the other runtime.
    pub backend: Option<SemanticRuntimeBackend>,
    pub replace_existing: bool,
}

/// What ctx installed, described exactly as the loader sees it afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticRuntimeReport {
    pub backend: SemanticRuntimeBackend,
    pub platform: String,
    pub version: String,
    pub root: PathBuf,
    pub library: PathBuf,
    pub archive_sha256: String,
    pub manager: &'static str,
    pub metadata_trust: &'static str,
    pub files: usize,
    pub identity: String,
}

/// The accelerator runtime this host could execute on, if any.
///
/// This is the same host detection the automatic backend selection uses, so
/// guidance never advertises an accelerator the loader would not pick. Core ML
/// maps to no backend at all: it is an execution provider of the CPU runtime
/// supplied by the OS, not an installable sidecar.
pub fn detected_accelerator_backend() -> Option<SemanticRuntimeBackend> {
    match crate::model_runtime::semantic_native_accelerator_target()? {
        crate::model_runtime::SemanticNativeAcceleratorTarget::Cuda => {
            Some(SemanticRuntimeBackend::Cuda)
        }
        crate::model_runtime::SemanticNativeAcceleratorTarget::WindowsMl => {
            Some(SemanticRuntimeBackend::WindowsMl)
        }
        crate::model_runtime::SemanticNativeAcceleratorTarget::CoreMl => None,
    }
}

/// Backends this build can install from a local archive, in display order.
///
/// The CPU runtime leads because it is the one every semantic platform needs;
/// CUDA is an explicit opt-in. Windows publishes both of its sidecars as zips
/// and this crate deliberately carries no zip reader, so a Windows build has to
/// provision through the hosted installer.
#[cfg(all(ctx_semantic_fastembed, target_os = "linux", target_arch = "x86_64"))]
pub fn supported_local_runtime_backends() -> &'static [SemanticRuntimeBackend] {
    &[SemanticRuntimeBackend::Cpu, SemanticRuntimeBackend::Cuda]
}

#[cfg(all(
    ctx_semantic_fastembed,
    not(target_os = "windows"),
    not(all(target_os = "linux", target_arch = "x86_64"))
))]
pub fn supported_local_runtime_backends() -> &'static [SemanticRuntimeBackend] {
    &[SemanticRuntimeBackend::Cpu]
}

#[cfg(any(
    not(ctx_semantic_fastembed),
    all(ctx_semantic_fastembed, target_os = "windows")
))]
pub fn supported_local_runtime_backends() -> &'static [SemanticRuntimeBackend] {
    &[]
}

#[cfg(not(ctx_semantic_fastembed))]
pub fn install_operator_runtime(
    request: &SemanticRuntimeInstall<'_>,
) -> anyhow::Result<SemanticRuntimeReport> {
    Err(unsupported_runtime_error(request.backend))
}

#[cfg(not(ctx_semantic_fastembed))]
pub fn identify_runtime_archive(_archive: &Path) -> anyhow::Result<SemanticRuntimeBackend> {
    Err(unsupported_runtime_error(None))
}

#[cfg(not(ctx_semantic_fastembed))]
pub fn installed_runtime_report(
    _runtime_root: &Path,
    backend: SemanticRuntimeBackend,
) -> anyhow::Result<Option<SemanticRuntimeReport>> {
    Err(unsupported_runtime_error(Some(backend)))
}

#[cfg(not(ctx_semantic_fastembed))]
fn unsupported_runtime_error(backend: Option<SemanticRuntimeBackend>) -> anyhow::Error {
    anyhow::anyhow!(
        "this ctx build has no semantic runtime, so the {} runtime cannot be installed on {} {}",
        backend.map_or("semantic", SemanticRuntimeBackend::as_str),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

#[cfg(ctx_semantic_fastembed)]
pub use enabled::{identify_runtime_archive, install_operator_runtime, installed_runtime_report};

#[cfg(ctx_semantic_fastembed)]
mod enabled;
