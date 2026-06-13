#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
use ctx_bundled_assets as bundled_assets;
#[cfg(test)]
use url::Url;

#[cfg(test)]
fn managed_artifact_extension(uri: &str) -> &'static str {
    let path = Url::parse(uri)
        .ok()
        .map(|parsed| parsed.path().to_string())
        .unwrap_or_else(|| uri.to_string());
    let path_lc = path.to_ascii_lowercase();
    if path_lc.ends_with(".tar.gz") {
        "tar.gz"
    } else if path_lc.ends_with(".zst") {
        "zst"
    } else if path_lc.ends_with(".tgz") {
        "tgz"
    } else if path_lc.ends_with(".tar") {
        "tar"
    } else {
        "zip"
    }
}

#[cfg(test)]
pub(super) fn managed_sandbox_cli_archive_path(
    data_root: &Path,
    source: &bundled_assets::ManagedRuntimeSource,
) -> PathBuf {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let ext = managed_artifact_extension(&source.uri);
    data_root
        .join("managed")
        .join("downloads")
        .join("sandbox-cli")
        .join(os)
        .join(arch)
        .join(format!(
            "sha256-{}.{}",
            source.sha256.trim().to_ascii_lowercase(),
            ext
        ))
}
