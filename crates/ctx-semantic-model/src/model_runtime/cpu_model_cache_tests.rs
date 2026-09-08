use std::{fs, path::Path};

use anyhow::Result;

#[cfg(unix)]
use super::{
    cleanup_semantic_cpu_download_cache, open_verified_semantic_cpu_model_blob,
    stage_opened_semantic_cpu_model_blob_with_link,
};
use super::{
    lock_semantic_model_acquisition,
    maybe_cleanup_semantic_cpu_download_cache_after_cached_acquisition,
    prepare_semantic_cpu_download_cache, publish_semantic_cpu_model_root,
    replace_ort_model_cache_from_pinned_revision, semantic_cpu_cache_snapshot,
    semantic_model_acquisition_integrity_error, semantic_ort_cache_snapshot,
    semantic_ort_published_model_root, stage_semantic_cpu_model_file, verify_semantic_cpu_file,
    verify_semantic_ort_snapshot, SemanticCpuModelCacheMissing, SemanticCpuModelIntegrityError,
    SemanticModelFile, SemanticOrtModelVariant, SEMANTIC_HF_MODEL_CACHE_DIR,
    SEMANTIC_MANAGED_MODEL_CACHE_DIR, SEMANTIC_MODEL_REVISION,
};
use crate::cache_paths::SEMANTIC_ACCELERATOR_MODEL_CACHE_DIR;

#[test]
fn cpu_model_file_verification_binds_size_and_sha256() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("model.bin");
    fs::write(&path, b"test")?;
    let expected = SemanticModelFile::new(
        "model.bin",
        4,
        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
    );
    verify_semantic_cpu_file(&path, expected)?;

    let size_error = verify_semantic_cpu_file(
        &path,
        SemanticModelFile::new("model.bin", 5, expected.sha256),
    )
    .unwrap_err();
    assert!(size_error
        .downcast_ref::<SemanticCpuModelIntegrityError>()
        .is_some());
    let hash_error = verify_semantic_cpu_file(
        &path,
        SemanticModelFile::new(
            "model.bin",
            4,
            "0000000000000000000000000000000000000000000000000000000000000000",
        ),
    )
    .unwrap_err();
    assert!(semantic_model_acquisition_integrity_error(&hash_error));
    Ok(())
}

#[test]
fn cpu_model_publication_failure_restores_old_root_and_preserves_download_cache() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let managed = temp.path().join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    let model_root = managed.join(SEMANTIC_HF_MODEL_CACHE_DIR);
    let shared = temp.path().join("shared-hf-cache");
    let partial_download = managed.join("download-cache/partial");
    fs::create_dir_all(&model_root)?;
    fs::create_dir_all(&shared)?;
    fs::create_dir_all(partial_download.parent().expect("partial parent"))?;
    fs::write(model_root.join("old"), b"old")?;
    fs::write(shared.join("keep"), b"shared")?;
    fs::write(&partial_download, b"partial")?;

    let missing_staging = managed.join("missing-staging");
    let lock = lock_semantic_model_acquisition(&managed)?;
    assert!(publish_semantic_cpu_model_root(&missing_staging, &model_root, &lock).is_err());
    assert_eq!(fs::read(model_root.join("old"))?, b"old");
    assert_eq!(fs::read(shared.join("keep"))?, b"shared");
    assert_eq!(fs::read(&partial_download)?, b"partial");

    Ok(())
}

#[test]
fn cpu_model_publication_removes_download_cache_after_commit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let managed = temp.path().join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    let model_root = managed.join(SEMANTIC_HF_MODEL_CACHE_DIR);
    let download_cache = managed.join("download-cache");
    let staging = managed.join("staging");
    fs::create_dir_all(&download_cache)?;
    fs::create_dir_all(&staging)?;
    fs::write(download_cache.join("downloaded"), b"duplicate")?;
    fs::write(staging.join("new"), b"new")?;

    let lock = lock_semantic_model_acquisition(&managed)?;
    publish_semantic_cpu_model_root(&staging, &model_root, &lock)?;
    assert_eq!(fs::read(model_root.join("new"))?, b"new");
    assert!(!download_cache.exists());
    Ok(())
}

#[test]
fn daemon_cached_cpu_model_acquisition_retries_stale_download_cache_cleanup() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let managed = temp.path().join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    let download_cache = managed.join("download-cache");
    fs::create_dir_all(&download_cache)?;
    fs::write(download_cache.join("stale"), b"stale")?;

    maybe_cleanup_semantic_cpu_download_cache_after_cached_acquisition(temp.path(), true);

    assert!(!download_cache.exists());
    assert!(managed.join("acquisition.lock").is_file());
    Ok(())
}

#[test]
fn foreground_cached_cpu_model_acquisition_does_not_mutate_cache() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let managed = temp.path().join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    let download_cache = managed.join("download-cache");
    fs::create_dir_all(&download_cache)?;
    fs::write(download_cache.join("stale"), b"stale")?;

    maybe_cleanup_semantic_cpu_download_cache_after_cached_acquisition(temp.path(), false);

    assert_eq!(fs::read(download_cache.join("stale"))?, b"stale");
    assert!(!managed.join("acquisition.lock").exists());
    Ok(())
}

#[test]
fn daemon_cpu_acquisition_fails_before_network_on_invalid_managed_root() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let managed = temp.path().join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    fs::write(&managed, b"preserve invalid managed root")?;

    let error = replace_ort_model_cache_from_pinned_revision(
        temp.path(),
        SemanticOrtModelVariant::CpuFp32,
    )
    .unwrap_err();

    assert!(format!("{error:#}").contains("create semantic model cache"));
    assert_eq!(fs::read(&managed)?, b"preserve invalid managed root");
    assert!(
        !managed.join("download-cache").exists(),
        "deterministic local failure must happen before downloader initialization"
    );
    assert!(!managed.join(SEMANTIC_HF_MODEL_CACHE_DIR).exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn cpu_model_staging_hard_links_verified_download_blob() -> Result<()> {
    use std::os::unix::fs::{symlink, MetadataExt};

    let temp = tempfile::tempdir()?;
    let download_cache = temp.path().join("download-cache");
    let blob = download_cache.join("repo/blobs/model");
    let pointer = download_cache.join("repo/snapshots/revision/model.onnx");
    let destination = temp.path().join("staging/model.onnx");
    fs::create_dir_all(blob.parent().expect("blob parent"))?;
    fs::create_dir_all(pointer.parent().expect("pointer parent"))?;
    fs::create_dir_all(destination.parent().expect("destination parent"))?;
    fs::write(&blob, b"test")?;
    symlink("../../blobs/model", &pointer)?;

    stage_semantic_cpu_model_file(&pointer, &download_cache, &destination)?;
    verify_semantic_cpu_file(
        &destination,
        SemanticModelFile::new(
            "model.onnx",
            4,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        ),
    )?;

    let blob_metadata = fs::metadata(&blob)?;
    let staged_metadata = fs::metadata(&destination)?;
    assert_eq!(
        (blob_metadata.dev(), blob_metadata.ino()),
        (staged_metadata.dev(), staged_metadata.ino())
    );
    cleanup_semantic_cpu_download_cache(&download_cache)?;
    assert_eq!(fs::read(&destination)?, b"test");
    Ok(())
}

#[cfg(unix)]
#[test]
fn cpu_model_staging_copy_fallback_uses_preopened_managed_blob() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let download_cache = temp.path().join("download-cache");
    let blob = download_cache.join("repo/blobs/model");
    let displaced = temp.path().join("displaced-model");
    let destination = temp.path().join("staging/model.onnx");
    fs::create_dir_all(blob.parent().expect("blob parent"))?;
    fs::create_dir_all(destination.parent().expect("destination parent"))?;
    fs::write(&blob, b"test")?;

    let (source, mut source_file) = open_verified_semantic_cpu_model_blob(&blob, &download_cache)?;
    stage_opened_semantic_cpu_model_blob_with_link(
        &source,
        &mut source_file,
        &destination,
        |source, _destination| {
            fs::rename(source, &displaced)?;
            fs::write(source, b"unsafe replacement")?;
            Err(std::io::Error::other("forced hard-link failure"))
        },
    )?;

    verify_semantic_cpu_file(
        &destination,
        SemanticModelFile::new(
            "model.onnx",
            4,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        ),
    )?;
    assert_eq!(fs::read(&destination)?, b"test");
    assert_eq!(fs::read(&blob)?, b"unsafe replacement");
    Ok(())
}

#[cfg(windows)]
#[test]
fn cpu_model_staging_copies_download_blob_on_windows() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let download_cache = temp.path().join("download-cache");
    let blob = download_cache.join("repo/blobs/model");
    let destination = temp.path().join("staging/model.onnx");
    fs::create_dir_all(blob.parent().expect("blob parent"))?;
    fs::create_dir_all(destination.parent().expect("destination parent"))?;
    fs::write(&blob, b"test")?;

    stage_semantic_cpu_model_file(&blob, &download_cache, &destination)?;
    fs::write(&blob, b"changed")?;

    assert_eq!(fs::read(&destination)?, b"test");
    Ok(())
}

#[cfg(unix)]
#[test]
fn cpu_model_staging_rejects_download_symlink_outside_managed_cache() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let download_cache = temp.path().join("download-cache");
    let outside = temp.path().join("outside-model");
    let pointer = download_cache.join("repo/snapshots/revision/model.onnx");
    let destination = temp.path().join("staging/model.onnx");
    fs::create_dir_all(pointer.parent().expect("pointer parent"))?;
    fs::create_dir_all(destination.parent().expect("destination parent"))?;
    fs::write(&outside, b"test")?;
    symlink(&outside, &pointer)?;

    assert!(stage_semantic_cpu_model_file(&pointer, &download_cache, &destination).is_err());
    assert_eq!(fs::read(&outside)?, b"test");
    assert!(!destination.exists());
    Ok(())
}

#[test]
fn cpu_model_publication_ignores_unexpected_download_cache_shape() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let managed = temp.path().join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    let model_root = managed.join(SEMANTIC_HF_MODEL_CACHE_DIR);
    let download_cache = managed.join("download-cache");
    let staging = managed.join("staging");
    fs::create_dir_all(&staging)?;
    fs::write(&download_cache, b"preserve")?;
    fs::write(staging.join("new"), b"new")?;

    let lock = lock_semantic_model_acquisition(&managed)?;
    publish_semantic_cpu_model_root(&staging, &model_root, &lock)?;

    assert_eq!(fs::read(model_root.join("new"))?, b"new");
    assert_eq!(fs::read(&download_cache)?, b"preserve");
    assert!(prepare_semantic_cpu_download_cache(&download_cache).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn cpu_model_publication_never_follows_download_cache_symlink() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let managed = temp.path().join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    let model_root = managed.join(SEMANTIC_HF_MODEL_CACHE_DIR);
    let download_cache = managed.join("download-cache");
    let staging = managed.join("staging");
    fs::create_dir_all(&model_root)?;
    fs::create_dir_all(&staging)?;
    fs::write(model_root.join("old"), b"old")?;
    fs::write(staging.join("new"), b"new")?;
    symlink(&model_root, &download_cache)?;

    let lock = lock_semantic_model_acquisition(&managed)?;
    publish_semantic_cpu_model_root(&staging, &model_root, &lock)?;

    assert_eq!(fs::read(model_root.join("new"))?, b"new");
    assert!(fs::symlink_metadata(&download_cache)?
        .file_type()
        .is_symlink());
    assert!(prepare_semantic_cpu_download_cache(&download_cache).is_err());
    Ok(())
}

#[test]
fn cpu_model_acquisition_lock_serializes_publishers() -> Result<()> {
    use fs2::FileExt;

    let temp = tempfile::tempdir()?;
    let managed = temp.path().join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    let first = lock_semantic_model_acquisition(&managed)?;
    let second_path = managed.join("acquisition.lock");
    let second = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&second_path)?;
    assert!(second.try_lock_exclusive().is_err());
    drop(first);
    second.lock_exclusive()?;
    FileExt::unlock(&second)?;
    Ok(())
}

/// Materialize `variant`'s pinned files at their contract paths with the pinned
/// sizes but zero content, so size checks pass and digest checks must fail.
fn write_sized_variant_snapshot(model_root: &Path, variant: SemanticOrtModelVariant) -> Result<()> {
    let snapshot = model_root.join("snapshots").join(SEMANTIC_MODEL_REVISION);
    for file in variant.required_files() {
        let path = snapshot.join(file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::File::create(&path)?.set_len(file.size)?;
    }
    Ok(())
}

#[test]
fn accelerator_variant_downloads_upstream_o4_graph_into_the_contract_path() -> Result<()> {
    let accelerator = SemanticOrtModelVariant::AcceleratorO4Fp16;
    assert_eq!(
        accelerator.pinned_source_path("onnx/model.onnx")?,
        "onnx/model_O4.onnx"
    );
    for file in accelerator.required_files().skip(1) {
        assert_eq!(accelerator.pinned_source_path(file.path)?, file.path);
    }
    for file in SemanticOrtModelVariant::CpuFp32.required_files() {
        assert_eq!(
            SemanticOrtModelVariant::CpuFp32.pinned_source_path(file.path)?,
            file.path
        );
    }
    assert!(accelerator.pinned_source_path("onnx/model_O4.onnx").is_err());
    assert!(accelerator.pinned_source_path("onnx/model_fp16.onnx").is_err());
    Ok(())
}

#[test]
fn accelerator_snapshot_verification_rejects_truncated_and_mismatched_models() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let variant = SemanticOrtModelVariant::AcceleratorO4Fp16;
    let snapshot = temp.path().join("snapshot");
    let model = snapshot.join("onnx/model.onnx");
    fs::create_dir_all(model.parent().expect("model parent"))?;
    fs::write(&model, b"truncated")?;

    let truncated = verify_semantic_ort_snapshot(&snapshot, variant).unwrap_err();
    assert!(semantic_model_acquisition_integrity_error(&truncated));
    assert!(format!("{truncated:#}").contains("onnx/model.onnx"));

    write_sized_variant_snapshot(temp.path().join("sized-root").as_path(), variant)?;
    let sized = temp
        .path()
        .join("sized-root")
        .join("snapshots")
        .join(SEMANTIC_MODEL_REVISION);
    let mismatched = verify_semantic_ort_snapshot(&sized, variant).unwrap_err();
    assert!(semantic_model_acquisition_integrity_error(&mismatched));
    assert!(format!("{mismatched:#}").contains("SHA-256"));
    Ok(())
}

#[test]
fn accelerator_and_cpu_snapshots_resolve_from_separate_roots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cache = temp.path().join("model-cache");
    let accelerator = SemanticOrtModelVariant::AcceleratorO4Fp16;
    let accelerator_root = semantic_ort_published_model_root(&cache, accelerator);
    write_sized_variant_snapshot(&accelerator_root, accelerator)?;

    // The accelerator lookup reaches its own root: the only way to reject these
    // files is to have hashed them there.
    let accelerator_error = semantic_ort_cache_snapshot(&cache, accelerator).unwrap_err();
    assert!(semantic_model_acquisition_integrity_error(
        &accelerator_error
    ));
    assert!(format!("{accelerator_error:#}").contains(SEMANTIC_ACCELERATOR_MODEL_CACHE_DIR));

    // The CPU lookup never sees the accelerator root, so it reports a missing
    // cache rather than adopting accelerator bytes.
    let cpu_error = semantic_cpu_cache_snapshot(&cache).unwrap_err();
    assert!(!semantic_model_acquisition_integrity_error(&cpu_error));
    assert!(cpu_error
        .downcast_ref::<SemanticCpuModelCacheMissing>()
        .is_some());
    Ok(())
}

#[test]
fn publishing_one_variant_preserves_the_other_variants_snapshot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cache = temp.path().join("model-cache");
    let managed = cache.join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    let cpu_root = semantic_ort_published_model_root(&cache, SemanticOrtModelVariant::CpuFp32);
    let accelerator_root =
        semantic_ort_published_model_root(&cache, SemanticOrtModelVariant::AcceleratorO4Fp16);
    assert_eq!(cpu_root, managed.join(SEMANTIC_HF_MODEL_CACHE_DIR));
    assert_ne!(cpu_root, accelerator_root);

    fs::create_dir_all(&cpu_root)?;
    fs::write(cpu_root.join("cpu-marker"), b"cpu")?;
    let accelerator_staging = managed.join("accelerator-staging");
    fs::create_dir_all(&accelerator_staging)?;
    fs::write(accelerator_staging.join("accelerator-marker"), b"accelerator")?;

    let lock = lock_semantic_model_acquisition(&managed)?;
    publish_semantic_cpu_model_root(&accelerator_staging, &accelerator_root, &lock)?;
    assert_eq!(fs::read(cpu_root.join("cpu-marker"))?, b"cpu");
    assert_eq!(
        fs::read(accelerator_root.join("accelerator-marker"))?,
        b"accelerator"
    );

    let cpu_staging = managed.join("cpu-staging");
    fs::create_dir_all(&cpu_staging)?;
    fs::write(cpu_staging.join("cpu-marker"), b"republished")?;
    publish_semantic_cpu_model_root(&cpu_staging, &cpu_root, &lock)?;
    assert_eq!(fs::read(cpu_root.join("cpu-marker"))?, b"republished");
    assert_eq!(
        fs::read(accelerator_root.join("accelerator-marker"))?,
        b"accelerator"
    );
    Ok(())
}
