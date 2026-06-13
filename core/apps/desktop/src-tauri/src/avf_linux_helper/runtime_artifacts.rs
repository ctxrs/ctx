use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct StagedBootKernelMetadata {
    source_gzip_sha256: String,
}

pub(super) fn default_shared_vm_kernel_cmdline() -> String {
    ensure_required_shared_vm_kernel_cmdline_tokens(format!(
        "console=hvc0 root=LABEL={SHARED_VM_ROOTFS_LABEL} rootwait rw"
    ))
}

pub(super) fn ensure_required_shared_vm_kernel_cmdline_tokens(mut cmdline: String) -> String {
    let required_tokens = REQUIRED_SHARED_VM_KERNEL_CMDLINE_BASE_TOKENS
        .iter()
        .copied()
        .map(str::to_owned)
        .chain(
            SHARED_VM_GUEST_POLICY_MASKED_UNITS
                .iter()
                .map(|unit| format!("systemd.mask={unit}")),
        );
    for token in required_tokens {
        if cmdline.split_whitespace().any(|part| part == token) {
            continue;
        }
        if !cmdline.is_empty() {
            cmdline.push(' ');
        }
        cmdline.push_str(&token);
    }
    cmdline
}

pub(super) fn path_has_gzip_magic(path: &Path) -> Result<bool> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut magic = [0_u8; 2];
    let read = file
        .read(&mut magic)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(read == 2 && magic == [0x1f, 0x8b])
}

fn staged_boot_kernel_metadata_path(staged_kernel_path: &Path) -> PathBuf {
    staged_kernel_path.with_extension("metadata.json")
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn read_staged_boot_kernel_metadata(
    staged_kernel_path: &Path,
) -> Result<Option<StagedBootKernelMetadata>> {
    let metadata_path = staged_boot_kernel_metadata_path(staged_kernel_path);
    if !metadata_path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&metadata_path)
        .with_context(|| format!("reading {}", metadata_path.display()))?;
    let parsed = serde_json::from_str::<StagedBootKernelMetadata>(&raw)
        .with_context(|| format!("parsing {}", metadata_path.display()))?;
    Ok(Some(parsed))
}

fn write_staged_boot_kernel_metadata(
    staged_kernel_path: &Path,
    metadata: &StagedBootKernelMetadata,
) -> Result<()> {
    let metadata_path = staged_boot_kernel_metadata_path(staged_kernel_path);
    let tmp_path = metadata_path.with_extension("metadata.json.tmp");
    let raw = serde_json::to_vec_pretty(metadata)?;
    fs::write(&tmp_path, raw).with_context(|| format!("writing {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &metadata_path).with_context(|| {
        format!(
            "staging boot kernel metadata {} -> {}",
            tmp_path.display(),
            metadata_path.display()
        )
    })?;
    Ok(())
}

fn remove_staged_boot_kernel_metadata(staged_kernel_path: &Path) -> Result<()> {
    let metadata_path = staged_boot_kernel_metadata_path(staged_kernel_path);
    if metadata_path.is_file() {
        fs::remove_file(&metadata_path)
            .with_context(|| format!("removing {}", metadata_path.display()))?;
    }
    Ok(())
}

pub(super) fn materialize_bootable_kernel_image(
    data_root: &Path,
    kernel_path: &Path,
) -> Result<(PathBuf, Option<String>)> {
    if !path_has_gzip_magic(kernel_path)? {
        return Ok((kernel_path.to_path_buf(), None));
    }

    let boot_root = shared_vm_boot_root(data_root);
    fs::create_dir_all(&boot_root).with_context(|| format!("creating {}", boot_root.display()))?;
    let staged_kernel_path = shared_vm_boot_kernel_path(data_root);
    let staged_kernel_tmp_path = staged_kernel_path.with_extension("tmp");
    let source_gzip_sha256 = sha256_file(kernel_path)?;

    if staged_kernel_path.is_file() {
        if read_staged_boot_kernel_metadata(&staged_kernel_path)?
            .is_some_and(|metadata| metadata.source_gzip_sha256 == source_gzip_sha256)
        {
            return Ok((
                staged_kernel_path.clone(),
                Some(format!(
                    "reused staged decompressed kernel image {} for {}",
                    staged_kernel_path.display(),
                    kernel_path.display()
                )),
            ));
        }
    }

    if staged_kernel_path.exists() {
        fs::remove_file(&staged_kernel_path)
            .with_context(|| format!("removing {}", staged_kernel_path.display()))?;
    }
    remove_staged_boot_kernel_metadata(&staged_kernel_path)?;
    if staged_kernel_tmp_path.exists() {
        fs::remove_file(&staged_kernel_tmp_path)
            .with_context(|| format!("removing {}", staged_kernel_tmp_path.display()))?;
    }

    let source_file =
        File::open(kernel_path).with_context(|| format!("opening {}", kernel_path.display()))?;
    let mut decoder = GzDecoder::new(source_file);
    let mut output = File::create(&staged_kernel_tmp_path)
        .with_context(|| format!("creating {}", staged_kernel_tmp_path.display()))?;
    std::io::copy(&mut decoder, &mut output).with_context(|| {
        format!(
            "decompressing gzipped kernel image {} -> {}",
            kernel_path.display(),
            staged_kernel_tmp_path.display()
        )
    })?;
    output
        .flush()
        .with_context(|| format!("flushing {}", staged_kernel_tmp_path.display()))?;
    drop(output);

    fs::rename(&staged_kernel_tmp_path, &staged_kernel_path).with_context(|| {
        format!(
            "staging decompressed kernel image {} -> {}",
            staged_kernel_tmp_path.display(),
            staged_kernel_path.display()
        )
    })?;
    write_staged_boot_kernel_metadata(
        &staged_kernel_path,
        &StagedBootKernelMetadata { source_gzip_sha256 },
    )?;
    Ok((
        staged_kernel_path.clone(),
        Some(format!(
            "decompressed gzipped kernel image {} into {} for AVF Linux boot",
            kernel_path.display(),
            staged_kernel_path.display()
        )),
    ))
}

pub(super) fn clone_or_copy_rootfs_image(
    source_rootfs: &Path,
    staged_rootfs_tmp: &Path,
) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        match fs::copy(source_rootfs, staged_rootfs_tmp) {
            Ok(_) => {
                return Ok(format!(
                    "copied rootfs image {} into helper-managed writable path {}",
                    source_rootfs.display(),
                    staged_rootfs_tmp.display()
                ));
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "copying rootfs image {} -> {}",
                        source_rootfs.display(),
                        staged_rootfs_tmp.display()
                    )
                });
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        fs::copy(source_rootfs, staged_rootfs_tmp).with_context(|| {
            format!(
                "copying rootfs image {} -> {}",
                source_rootfs.display(),
                staged_rootfs_tmp.display()
            )
        })?;
        Ok(format!(
            "copied rootfs image {} into helper-managed writable path {}",
            source_rootfs.display(),
            staged_rootfs_tmp.display()
        ))
    }
}

fn gibibytes(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

fn initialize_sparse_disk_image(
    path: &Path,
    logical_bytes: u64,
    description: &str,
) -> Result<String> {
    let file = File::create(path)
        .with_context(|| format!("creating {} {}", description, path.display()))?;
    file.set_len(logical_bytes).with_context(|| {
        format!(
            "sizing {description} {} to {} bytes",
            path.display(),
            logical_bytes
        )
    })?;
    Ok(format!(
        "initialized sparse {description} {} with {:.2} GiB logical capacity",
        path.display(),
        gibibytes(logical_bytes),
    ))
}

pub(super) fn materialize_writable_rootfs_image(
    data_root: &Path,
    source_rootfs: &Path,
) -> Result<(PathBuf, Option<String>)> {
    let disk_root = shared_vm_disk_root(data_root);
    fs::create_dir_all(&disk_root).with_context(|| format!("creating {}", disk_root.display()))?;
    let staged_rootfs_path = shared_vm_rootfs_path(data_root);
    let staged_rootfs_tmp = staged_rootfs_path.with_extension("tmp");

    if staged_rootfs_path.is_file() {
        return Ok((staged_rootfs_path, None));
    }
    if staged_rootfs_tmp.exists() {
        fs::remove_file(&staged_rootfs_tmp)
            .with_context(|| format!("removing {}", staged_rootfs_tmp.display()))?;
    }

    let source_rootfs_canonical = source_rootfs
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", source_rootfs.display()))?;
    let note = clone_or_copy_rootfs_image(&source_rootfs_canonical, &staged_rootfs_tmp)?;
    fs::rename(&staged_rootfs_tmp, &staged_rootfs_path).with_context(|| {
        format!(
            "staging writable rootfs image {} -> {}",
            staged_rootfs_tmp.display(),
            staged_rootfs_path.display()
        )
    })?;
    Ok((staged_rootfs_path, Some(note)))
}

pub(super) fn materialize_data_disk_image(data_root: &Path) -> Result<(PathBuf, Option<String>)> {
    let disk_root = shared_vm_disk_root(data_root);
    fs::create_dir_all(&disk_root).with_context(|| format!("creating {}", disk_root.display()))?;
    let data_disk_path = shared_vm_data_disk_path(data_root);
    let data_disk_tmp = data_disk_path.with_extension("tmp");

    if data_disk_path.is_file() {
        return Ok((data_disk_path, None));
    }
    if data_disk_tmp.exists() {
        fs::remove_file(&data_disk_tmp)
            .with_context(|| format!("removing {}", data_disk_tmp.display()))?;
    }

    let note = initialize_sparse_disk_image(
        &data_disk_tmp,
        SHARED_VM_INITIAL_DATA_DISK_BYTES,
        "AVF Linux data disk",
    )?;
    fs::rename(&data_disk_tmp, &data_disk_path).with_context(|| {
        format!(
            "staging AVF Linux data disk image {} -> {}",
            data_disk_tmp.display(),
            data_disk_path.display()
        )
    })?;
    Ok((data_disk_path, Some(note)))
}

pub(super) fn load_shared_vm_kernel_cmdline(runtime_root: &Path) -> Result<String> {
    let path = shared_vm_kernel_cmdline_path(runtime_root);
    if !path.is_file() {
        return Ok(default_shared_vm_kernel_cmdline());
    }
    let contents =
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Ok(default_shared_vm_kernel_cmdline());
    }
    Ok(ensure_required_shared_vm_kernel_cmdline_tokens(
        trimmed.to_string(),
    ))
}

pub(super) fn shared_vm_runtime_supports_real_guest_exec(runtime_root: &Path) -> (bool, String) {
    let guest_agent_path = shared_vm_guest_agent_helper_path(runtime_root);
    if guest_agent_path.is_file() {
        return (
            true,
            format!(
                "runtime includes a guest-agent payload at {}; enabling real AVF VM ownership",
                guest_agent_path.display()
            ),
        );
    }
    if std::env::var_os("CTX_AVF_LINUX_FORCE_REAL_VM").is_some() {
        return (
            true,
            format!(
                "forcing real AVF VM ownership without a staged guest-agent payload because CTX_AVF_LINUX_FORCE_REAL_VM is set (expected helper path: {})",
                guest_agent_path.display()
            ),
        );
    }
    (
        false,
        format!(
            "runtime is missing a baked guest-agent payload at {}; keeping the shared VM on the simulated relay path",
            guest_agent_path.display()
        ),
    )
}
