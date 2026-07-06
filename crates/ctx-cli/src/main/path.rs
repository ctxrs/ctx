#[allow(unused_imports)]
use super::*;

pub(crate) fn discovered_plugin_sources_json(data_root: &Path) -> Result<Vec<Value>> {
    let plugin_discovery = discover_history_source_plugins_with_diagnostics(data_root, &[])?;
    let mut values = plugin_sources_json(&plugin_discovery.sources);
    values.extend(plugin_manifest_failures_json(&plugin_discovery.failures));
    Ok(values)
}

pub(crate) fn low_disk_space_warning(db_path: &Path, planned_total_bytes: u64) -> Option<String> {
    let parent = db_path.parent().unwrap_or_else(|| Path::new("."));
    let available = available_space_bytes(parent)?;
    let recommended = (planned_total_bytes / 4).clamp(1 << 30, 20 * (1 << 30));
    if available < recommended {
        Some(format!(
            "low disk space: {} available near {}, {} recommended before indexing {}",
            format_bytes(available),
            parent.display(),
            format_bytes(recommended),
            format_bytes(planned_total_bytes)
        ))
    } else {
        None
    }
}

#[cfg(unix)]
pub(crate) fn available_space_bytes(path: &Path) -> Option<u64> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    fn statvfs_field_to_u64<T>(value: T) -> Option<u64>
    where
        T: TryInto<u64>,
    {
        value.try_into().ok()
    }

    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    let available_blocks = statvfs_field_to_u64(stat.f_bavail)?;
    let fragment_size = statvfs_field_to_u64(stat.f_frsize)?;
    Some(available_blocks.saturating_mul(fragment_size))
}

#[cfg(not(unix))]
pub(crate) fn available_space_bytes(_path: &Path) -> Option<u64> {
    None
}

pub(crate) fn write_output(body: String, out: Option<PathBuf>) -> Result<()> {
    if let Some(out) = out {
        if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(&out, body).with_context(|| format!("write {}", out.display()))?;
    } else {
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

pub(crate) fn source_json_for(store: &Store, source_id: Option<Uuid>) -> Option<Value> {
    let source = source_id.and_then(|source_id| store.get_capture_source(source_id).ok())?;
    let path = source.descriptor.raw_source_path.clone();
    Some(compact_json(json!({
        "source_id": source.id,
        "provider": source.descriptor.provider,
        "provider_session_id": source.descriptor.external_session_id,
        "path": path,
        "exists": source_path_exists(path.as_deref()),
        "cwd": source.descriptor.cwd,
        "started_at": source.started_at,
        "ended_at": source.ended_at,
        "source_format": source_format(&source.sync.metadata),
        "cursor": source_cursor(&source.sync.metadata),
    })))
}

pub(crate) fn source_path_for(store: &Store, source_id: Option<Uuid>) -> Option<String> {
    source_id
        .and_then(|source_id| store.get_capture_source(source_id).ok())
        .and_then(|source| source.descriptor.raw_source_path)
}

pub(crate) fn source_path_exists(source_path: Option<&str>) -> Option<bool> {
    source_path.map(|path| Path::new(path).exists())
}

pub(crate) fn manifest_arg_matches_source(arg: &Path, manifest_path: &Path) -> bool {
    if arg.is_file() {
        return same_pathish(arg, manifest_path);
    }
    if arg.is_dir() {
        return manifest_path.starts_with(arg);
    }
    same_pathish(arg, manifest_path)
}

pub(crate) fn same_pathish(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

pub(crate) fn sha256_file_prefix_hex(path: &Path, byte_count: u64) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut remaining = byte_count;
    let mut buffer = [0_u8; 8192];
    while remaining > 0 {
        let to_read = buffer.len().min(remaining as usize);
        let read = file.read(&mut buffer[..to_read])?;
        if read == 0 {
            return Err(anyhow!(
                "file ended before checkpoint byte offset {byte_count}: {}",
                path.display()
            ));
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn explicit_path_source(provider: CaptureProvider, path: PathBuf) -> SourceInfo {
    source_for_path(provider, path)
}

pub(crate) fn raw_retention_json(retention: ProviderRawRetention) -> &'static str {
    match retention {
        ProviderRawRetention::None => "none",
        ProviderRawRetention::PathReference => "path_reference",
        ProviderRawRetention::MetadataOnly => "metadata_only",
        ProviderRawRetention::LocalBlob => "local_blob",
        ProviderRawRetention::Withheld => "withheld",
    }
}
