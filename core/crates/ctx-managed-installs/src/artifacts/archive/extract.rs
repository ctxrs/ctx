use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

use super::safe_paths::{
    create_archive_dir, create_archive_file, create_archive_hardlink, create_archive_symlink,
    ensure_archive_root, normalize_archive_entry_path_allowing_empty, safe_archive_dest,
};

pub(crate) fn extract_zip_to_dir(zip_path: &Path, out_dir: &Path) -> Result<()> {
    let root = ensure_archive_root(out_dir)?;
    let file =
        std::fs::File::open(zip_path).with_context(|| format!("open {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("parsing zip")?;
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).context("zip entry")?;
        let entry_name = f.name().to_string();
        let dest = safe_archive_dest(out_dir, Path::new(&entry_name), "zip entry path")?;
        let mode = f.unix_mode();
        let file_type = mode.unwrap_or(0) & 0o170000;
        if f.is_dir() || file_type == 0o040000 {
            create_archive_dir(&root, out_dir, &dest)?;
            continue;
        }

        if file_type == 0o120000 {
            let mut target = String::new();
            f.read_to_string(&mut target)
                .context("read zip symlink target")?;
            create_archive_symlink(&root, out_dir, &dest, Path::new(&target))?;
            continue;
        }

        if file_type != 0 && file_type != 0o100000 {
            anyhow::bail!("unsupported zip entry type for {}", entry_name);
        }
        create_archive_file(&root, out_dir, &dest, &mut f, mode)?;
    }
    Ok(())
}

pub(crate) fn extract_tar_gz_to_dir(tar_gz_path: &Path, out_dir: &Path) -> Result<()> {
    let tar_gz = std::fs::File::open(tar_gz_path)
        .with_context(|| format!("open {}", tar_gz_path.display()))?;
    let dec = flate2::read::GzDecoder::new(tar_gz);
    extract_tar_stream_to_dir(dec, out_dir, "tar.gz")
}

pub(crate) fn extract_tar_bz2_to_dir(tar_bz2_path: &Path, out_dir: &Path) -> Result<()> {
    let tar_bz2 = std::fs::File::open(tar_bz2_path)
        .with_context(|| format!("open {}", tar_bz2_path.display()))?;
    let dec = bzip2::read::BzDecoder::new(tar_bz2);
    extract_tar_stream_to_dir(dec, out_dir, "tar.bz2")
}

fn extract_tar_stream_to_dir<R: Read>(reader: R, out_dir: &Path, label: &str) -> Result<()> {
    let root = ensure_archive_root(out_dir)?;
    let mut archive = tar::Archive::new(reader);
    for entry in archive
        .entries()
        .with_context(|| format!("read {label} entries"))?
    {
        let mut entry = entry.with_context(|| format!("read {label} entry"))?;
        let entry_type = entry.header().entry_type();
        if is_tar_metadata_entry(&entry_type) {
            continue;
        }
        let raw_path = entry
            .path()
            .with_context(|| format!("read {label} entry path"))?
            .into_owned();
        let normalized_path =
            normalize_archive_entry_path_allowing_empty(&raw_path, "tar entry path")?;
        if normalized_path.as_os_str().is_empty() {
            if entry_type.is_dir() {
                continue;
            }
            anyhow::bail!("tar entry path is empty");
        }
        let dest = out_dir.join(normalized_path);

        if entry_type.is_dir() {
            create_archive_dir(&root, out_dir, &dest)?;
            continue;
        }
        if entry_type.is_file() {
            let mode = entry.header().mode().ok();
            create_archive_file(&root, out_dir, &dest, &mut entry, mode)?;
            continue;
        }
        if entry_type.is_symlink() {
            let target = entry
                .link_name()
                .with_context(|| format!("read {label} symlink target"))?
                .ok_or_else(|| {
                    anyhow::anyhow!("tar symlink missing target: {}", raw_path.display())
                })?
                .into_owned();
            create_archive_symlink(&root, out_dir, &dest, &target)?;
            continue;
        }
        if entry_type.is_hard_link() {
            let target = entry
                .link_name()
                .with_context(|| format!("read {label} hardlink target"))?
                .ok_or_else(|| {
                    anyhow::anyhow!("tar hardlink missing target: {}", raw_path.display())
                })?
                .into_owned();
            create_archive_hardlink(&root, out_dir, &dest, &target)?;
            continue;
        }

        anyhow::bail!(
            "unsupported tar entry type {} for {}",
            entry_type.as_byte(),
            raw_path.display()
        );
    }
    Ok(())
}

fn is_tar_metadata_entry(entry_type: &tar::EntryType) -> bool {
    entry_type.is_pax_global_extensions()
        || entry_type.is_pax_local_extensions()
        || entry_type.is_gnu_longname()
        || entry_type.is_gnu_longlink()
}
