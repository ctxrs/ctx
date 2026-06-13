#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use url::Url;

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

fn extract_zip_to_dir(zip_path: &Path, out_dir: &Path) -> Result<()> {
    let file =
        std::fs::File::open(zip_path).with_context(|| format!("open {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("parsing zip archive")?;
    for idx in 0..archive.len() {
        let mut entry = archive.by_index(idx).context("zip entry")?;
        if entry.is_dir() {
            continue;
        }
        let Some(enclosed) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
            continue;
        };
        let dest = out_dir.join(enclosed);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut out =
            std::fs::File::create(&dest).with_context(|| format!("create {}", dest.display()))?;
        std::io::copy(&mut entry, &mut out).context("extract zip entry")?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode));
        }
    }
    Ok(())
}

fn extract_zstd_to_dir(archive_path: &Path, source_uri: &str, out_dir: &Path) -> Result<()> {
    let source_path = Url::parse(source_uri)
        .ok()
        .map(|parsed| parsed.path().to_string())
        .unwrap_or_else(|| source_uri.to_string());
    let file_name = Path::new(&source_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("unable to derive zstd output filename from {source_uri}")
        })?;
    let output_name = file_name
        .strip_suffix(".zst")
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("zstd archive path must end with a concrete filename: {source_uri}")
        })?;
    let dest = out_dir.join(output_name);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let input = std::fs::File::open(archive_path)
        .with_context(|| format!("open {}", archive_path.display()))?;
    let mut decoder = zstd::stream::read::Decoder::new(input).context("parsing zstd archive")?;
    let mut output =
        std::fs::File::create(&dest).with_context(|| format!("create {}", dest.display()))?;
    std::io::copy(&mut decoder, &mut output).context("extract zstd archive")?;
    Ok(())
}

pub fn extract_archive_to_dir(archive_path: &Path, source_uri: &str, out_dir: &Path) -> Result<()> {
    let kind = managed_artifact_extension(source_uri);
    match kind {
        "zip" => extract_zip_to_dir(archive_path, out_dir),
        "zst" => extract_zstd_to_dir(archive_path, source_uri, out_dir),
        "tar.gz" | "tgz" => {
            let archive_file = std::fs::File::open(archive_path)
                .with_context(|| format!("open {}", archive_path.display()))?;
            let decoder = flate2::read::GzDecoder::new(archive_file);
            let mut archive = tar::Archive::new(decoder);
            archive.unpack(out_dir).context("extract tar.gz archive")
        }
        "tar" => {
            let archive_file = std::fs::File::open(archive_path)
                .with_context(|| format!("open {}", archive_path.display()))?;
            let mut archive = tar::Archive::new(archive_file);
            archive.unpack(out_dir).context("extract tar archive")
        }
        _ => anyhow::bail!("unsupported sandbox CLI archive type for {source_uri}"),
    }
}

pub fn resolve_single_extracted_root(extract_dir: &Path) -> Result<PathBuf> {
    let mut dirs = Vec::new();
    let mut has_files = false;
    for entry in std::fs::read_dir(extract_dir)
        .with_context(|| format!("read_dir {}", extract_dir.display()))?
    {
        let entry = entry.with_context(|| format!("read_dir entry {}", extract_dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        } else {
            has_files = true;
        }
    }
    if has_files || dirs.len() != 1 {
        return Ok(extract_dir.to_path_buf());
    }
    Ok(dirs.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_artifact_extension_recognizes_zstd_archives() {
        assert_eq!(
            managed_artifact_extension("https://example.test/rootfs.raw.zst"),
            "zst"
        );
    }

    #[test]
    fn extract_archive_to_dir_writes_single_file_zstd_payload() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive_path = temp.path().join("rootfs.raw.zst");
        let extracted_dir = temp.path().join("extract");
        std::fs::create_dir_all(&extracted_dir).expect("create extract dir");
        let compressed =
            zstd::stream::encode_all(&b"rootfs payload"[..], 1).expect("encode zstd payload");
        std::fs::write(&archive_path, compressed).expect("write zstd archive");

        extract_archive_to_dir(
            &archive_path,
            "https://example.test/runtime/rootfs.raw.zst",
            &extracted_dir,
        )
        .expect("extract zstd archive");

        let extracted = extracted_dir.join("rootfs.raw");
        assert_eq!(
            std::fs::read(&extracted).expect("read extracted rootfs"),
            b"rootfs payload"
        );
    }
}
