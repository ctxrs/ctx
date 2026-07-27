use std::fs;

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
    semantic_model_acquisition_integrity_error, stage_semantic_cpu_model_file,
    verify_semantic_cpu_file, SemanticCpuModelIntegrityError, SemanticModelFile,
    SEMANTIC_HF_MODEL_CACHE_DIR, SEMANTIC_MANAGED_MODEL_CACHE_DIR,
};

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
