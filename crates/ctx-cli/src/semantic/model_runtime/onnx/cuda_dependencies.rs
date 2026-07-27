#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
use std::path::{Path, PathBuf};
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::sync::Mutex;

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
use anyhow::{anyhow, Result};

pub(super) const FILES: &[&str] = &[
    "lib/libcudart.so.12",
    "lib/libcublasLt.so.12",
    "lib/libcublas.so.12",
    "lib/libcurand.so.10",
    "lib/libcufft.so.11",
    "lib/libnvrtc.so.12",
    "lib/libcudnn.so.9",
    "lib/libcudnn_graph.so.9",
    "lib/libcudnn_ops.so.9",
];
pub(super) const DOCUMENTS: &[&str] = &["NVIDIA-CUDA-LICENSE.txt", "NVIDIA-CUDNN-LICENSE.txt"];

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(super) fn preload(runtime_path: &Path) -> Result<()> {
    use libloading::os::unix::{Library as UnixLibrary, RTLD_GLOBAL, RTLD_NOW};

    static PRELOADED: std::sync::OnceLock<Mutex<Vec<(PathBuf, libloading::Library)>>> =
        std::sync::OnceLock::new();
    let libraries = PRELOADED.get_or_init(|| Mutex::new(Vec::new()));
    let mut libraries = libraries.lock().unwrap_or_else(|error| error.into_inner());
    for path in paths(runtime_path)? {
        if libraries
            .iter()
            .any(|(loaded_path, _)| loaded_path == &path)
        {
            continue;
        }
        let library =
            unsafe { UnixLibrary::open(Some(&path), RTLD_NOW | RTLD_GLOBAL) }.map_err(|error| {
                anyhow!("load packaged CUDA dependency {}: {error}", path.display())
            })?;
        libraries.push((path, library.into()));
    }
    Ok(())
}

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
pub(super) fn paths(runtime_path: &Path) -> Result<Vec<PathBuf>> {
    let directory = runtime_path
        .parent()
        .ok_or_else(|| anyhow!("ONNX Runtime library has no parent directory"))?;
    FILES
        .iter()
        .map(|relative| {
            Path::new(relative)
                .file_name()
                .map(|name| directory.join(name))
                .ok_or_else(|| anyhow!("CUDA dependency path has no file name"))
        })
        .collect()
}
