use super::*;
use crate::semantic::{
    model_bundle::{
        completion_marker_matches, completion_marker_path, content_addressed_bundle_path,
        lock_signed_bundle_cache, prepare_signed_bundle_cache,
        set_signed_bundle_cache_lock_contended_hook, write_completion_marker_atomic,
    },
    model_contract::{CoreMlBundleContract, COREML_BUNDLE_CONTRACT},
};
use sha2::{Digest, Sha256};

const A_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B_HASH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[test]
fn production_descriptor_is_hash_pinned_and_cache_probe_is_offline() {
    assert!(coreml_descriptor_provisioned());
    assert_eq!(COREML_BUNDLE_CONTRACT.document_batch_size, 16);
    assert_eq!(COREML_BUNDLE_CONTRACT.query_batch_size, Some(1));
    let temp = tempfile::tempdir().unwrap();
    assert!(cached_coreml_bundle(temp.path()).unwrap().is_none());
}

#[test]
fn cache_only_probe_never_reads_artifact_url() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp
        .path()
        .join("ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz");
    let artifact_url = format!("file://{}", missing.display());
    let descriptor = test_descriptor(&artifact_url, A_HASH, B_HASH);
    assert!(cached_coreml_bundle_for(temp.path(), &descriptor)
        .unwrap()
        .is_none());
    assert!(!missing.exists());
}

#[test]
fn archive_hash_mismatch_is_an_integrity_failure() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp
        .path()
        .join("ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz");
    fs::write(&archive, b"not an archive").unwrap();
    let artifact_url = format!("file://{}", archive.display());
    let descriptor = test_descriptor(&artifact_url, A_HASH, B_HASH);
    let error = acquire_coreml_bundle_for(temp.path(), &descriptor).unwrap_err();
    assert!(model_acquisition_integrity_error(&error));
    assert!(format!("{error:#}").contains("SHA-256"));
}

#[test]
fn archive_paths_and_entry_types_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    for (path, entry_type) in [
        (
            "ctx-multilingual-e5-small-coreml-fp16-1.0.0/../escape",
            tar::EntryType::Regular,
        ),
        (
            "ctx-multilingual-e5-small-coreml-fp16-1.0.0/link",
            tar::EntryType::Symlink,
        ),
        (
            "ctx-multilingual-e5-small-coreml-fp16-1.0.0/device",
            tar::EntryType::Char,
        ),
        (
            "ctx-multilingual-e5-small-coreml-fp16-1.0.0/hardlink",
            tar::EntryType::Link,
        ),
        (
            "ctx-multilingual-e5-small-coreml-fp16-1.0.0/fifo",
            tar::EntryType::Fifo,
        ),
        (
            "ctx-multilingual-e5-small-coreml-fp16-1.0.0/unknown",
            tar::EntryType::new(b'Z'),
        ),
    ] {
        let archive = temp
            .path()
            .join(format!("{}.tar.xz", path.rsplit('/').next().unwrap()));
        write_test_archive(&archive, &[(path, entry_type, b"x")]);
        let output = temp.path().join(format!("out-{}", entry_type.as_byte()));
        fs::create_dir(&output).unwrap();
        let descriptor = test_descriptor("file:///unused", A_HASH, B_HASH);
        let error = extract_archive(&archive, &output, &descriptor).unwrap_err();
        assert!(model_acquisition_integrity_error(&error));
    }
}

#[test]
fn archive_duplicate_paths_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("duplicate.tar.xz");
    write_test_archive(
        &archive,
        &[
            (
                "ctx-multilingual-e5-small-coreml-fp16-1.0.0/file",
                tar::EntryType::Regular,
                b"a",
            ),
            (
                "ctx-multilingual-e5-small-coreml-fp16-1.0.0/file",
                tar::EntryType::Regular,
                b"b",
            ),
        ],
    );
    let output = temp.path().join("output");
    fs::create_dir(&output).unwrap();
    let descriptor = test_descriptor("file:///unused", A_HASH, B_HASH);
    let error = extract_archive(&archive, &output, &descriptor).unwrap_err();
    assert!(format!("{error:#}").contains("duplicate"));
}

#[test]
fn macos_versions_compare_numerically() {
    assert!(version_at_least("14.7.5", "13.0").unwrap());
    assert!(version_at_least("13.0", "13.0").unwrap());
    assert!(!version_at_least("12.6.9", "13.0").unwrap());
    assert!(version_at_least("13.0.1", "13.0").unwrap());
}

#[test]
fn verified_archive_installs_content_addressed_and_then_uses_cache_only() {
    let temp = tempfile::tempdir().unwrap();
    let (archive_path, archive_sha256, manifest_sha256) = create_test_bundle_archive(temp.path());
    let artifact_url = format!("file://{}", archive_path.display());
    let descriptor = test_descriptor(&artifact_url, &archive_sha256, &manifest_sha256);
    let cache = temp.path().join("cache");

    let acquired = acquire_coreml_bundle_for(&cache, &descriptor).unwrap();
    assert_eq!(acquired.source, CoreMlAcquisitionSource::Download);
    assert_eq!(acquired.bundle.manifest_sha256, manifest_sha256);
    let installed = content_addressed_bundle_path(&cache, descriptor.manifest_sha256).unwrap();
    assert!(completion_marker_matches(&installed, &manifest_sha256).unwrap());

    fs::remove_file(&archive_path).unwrap();
    let cached = acquire_coreml_bundle_for(&cache, &descriptor).unwrap();
    assert_eq!(cached.source, CoreMlAcquisitionSource::Cache);
    assert_eq!(cached.bundle.manifest_sha256, manifest_sha256);
}

#[test]
fn acquisition_lock_keeps_repair_away_from_active_publication() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let manifest_sha256 = create_test_bundle(&source);
    let missing_archive = temp.path().join(COREML_BUNDLE_CONTRACT.artifact_name);
    let artifact_url = format!("file://{}", missing_archive.display());
    let descriptor = test_descriptor(&artifact_url, A_HASH, &manifest_sha256);
    let cache = temp.path().join("cache");
    let artifacts = prepare_signed_bundle_cache(&cache).unwrap();

    let publisher_lock = lock_signed_bundle_cache(&artifacts).unwrap();
    let installed = content_addressed_bundle_path(&cache, descriptor.manifest_sha256).unwrap();
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    fs::rename(&source, &installed).unwrap();
    let marker = completion_marker_path(&installed).unwrap();
    assert!(installed.is_dir());
    assert!(!marker.exists());

    let (contended_tx, contended_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let second = scope.spawn(|| {
            set_signed_bundle_cache_lock_contended_hook(move || {
                contended_tx.send(()).unwrap();
            });
            acquire_coreml_bundle_for(&cache, &descriptor)
        });

        if let Err(error) = contended_rx.recv_timeout(Duration::from_secs(5)) {
            write_completion_marker_atomic(&installed).unwrap();
            drop(publisher_lock);
            let second_result = second.join();
            panic!(
                "second acquirer did not report lock contention: {error}; result: {second_result:?}"
            );
        }
        assert!(installed.is_dir());
        assert!(!marker.exists());

        write_completion_marker_atomic(&installed).unwrap();
        drop(publisher_lock);

        let acquired = second.join().unwrap().unwrap();
        assert_eq!(acquired.source, CoreMlAcquisitionSource::Cache);
        assert!(completion_marker_matches(&installed, &manifest_sha256).unwrap());
    });
}

#[test]
fn daemon_acquisition_repairs_bundle_published_without_marker() {
    let temp = tempfile::tempdir().unwrap();
    let (archive_path, archive_sha256, manifest_sha256) = create_test_bundle_archive(temp.path());
    let artifact_url = format!("file://{}", archive_path.display());
    let descriptor = test_descriptor(&artifact_url, &archive_sha256, &manifest_sha256);
    let cache = temp.path().join("cache");
    acquire_coreml_bundle_for(&cache, &descriptor).unwrap();

    let installed = content_addressed_bundle_path(&cache, descriptor.manifest_sha256).unwrap();
    let marker = completion_marker_path(&installed).unwrap();
    fs::remove_file(marker).unwrap();
    fs::write(installed.join("interrupted-publication"), b"stale").unwrap();

    let repaired = acquire_coreml_bundle_for(&cache, &descriptor).unwrap();
    assert_eq!(repaired.source, CoreMlAcquisitionSource::Download);
    assert!(!installed.join("interrupted-publication").exists());
    assert!(completion_marker_matches(&installed, &manifest_sha256).unwrap());
}

#[test]
fn daemon_acquisition_repairs_marker_published_without_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let (archive_path, archive_sha256, manifest_sha256) = create_test_bundle_archive(temp.path());
    let artifact_url = format!("file://{}", archive_path.display());
    let descriptor = test_descriptor(&artifact_url, &archive_sha256, &manifest_sha256);
    let cache = temp.path().join("cache");
    acquire_coreml_bundle_for(&cache, &descriptor).unwrap();

    let installed = content_addressed_bundle_path(&cache, descriptor.manifest_sha256).unwrap();
    let marker = completion_marker_path(&installed).unwrap();
    fs::remove_dir_all(&installed).unwrap();
    assert!(marker.is_file());

    let repaired = acquire_coreml_bundle_for(&cache, &descriptor).unwrap();
    assert_eq!(repaired.source, CoreMlAcquisitionSource::Download);
    assert!(completion_marker_matches(&installed, &manifest_sha256).unwrap());
}

#[cfg(unix)]
#[test]
fn daemon_acquisition_refuses_to_repair_symlinked_incomplete_entries() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let missing_archive = temp.path().join("missing.tar.xz");
    let artifact_url = format!("file://{}", missing_archive.display());
    let descriptor = test_descriptor(&artifact_url, A_HASH, B_HASH);

    let directory_cache = temp.path().join("directory-cache");
    let directory_path =
        content_addressed_bundle_path(&directory_cache, descriptor.manifest_sha256).unwrap();
    fs::create_dir_all(directory_path.parent().unwrap()).unwrap();
    let directory_target = temp.path().join("directory-target");
    fs::create_dir(&directory_target).unwrap();
    fs::write(directory_target.join("keep"), b"keep").unwrap();
    symlink(&directory_target, &directory_path).unwrap();
    let error = acquire_coreml_bundle_for(&directory_cache, &descriptor).unwrap_err();
    assert!(model_acquisition_integrity_error(&error));
    assert_eq!(fs::read(directory_target.join("keep")).unwrap(), b"keep");

    let marker_cache = temp.path().join("marker-cache");
    let bundle_path =
        content_addressed_bundle_path(&marker_cache, descriptor.manifest_sha256).unwrap();
    fs::create_dir_all(bundle_path.parent().unwrap()).unwrap();
    let marker = completion_marker_path(&bundle_path).unwrap();
    let marker_target = temp.path().join("marker-target");
    fs::write(&marker_target, b"keep").unwrap();
    symlink(&marker_target, &marker).unwrap();
    let error = acquire_coreml_bundle_for(&marker_cache, &descriptor).unwrap_err();
    assert!(model_acquisition_integrity_error(&error));
    assert_eq!(fs::read(marker_target).unwrap(), b"keep");
}

#[cfg(unix)]
#[test]
fn daemon_acquisition_refuses_to_repair_through_symlinked_parent() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let missing_archive = temp.path().join("missing.tar.xz");
    let artifact_url = format!("file://{}", missing_archive.display());
    let descriptor = test_descriptor(&artifact_url, A_HASH, B_HASH);
    let cache = temp.path().join("cache");
    let installed = content_addressed_bundle_path(&cache, descriptor.manifest_sha256).unwrap();
    let digest_parent = installed.parent().unwrap();
    fs::create_dir_all(digest_parent.parent().unwrap()).unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, digest_parent).unwrap();
    let outside_bundle = outside.join(installed.file_name().unwrap());
    fs::create_dir(&outside_bundle).unwrap();
    fs::write(outside_bundle.join("keep"), b"keep").unwrap();

    let error = acquire_coreml_bundle_for(&cache, &descriptor).unwrap_err();
    assert!(model_acquisition_integrity_error(&error));
    assert_eq!(fs::read(outside_bundle.join("keep")).unwrap(), b"keep");
}

#[test]
fn daemon_acquisition_does_not_repair_completed_integrity_failures() {
    let temp = tempfile::tempdir().unwrap();
    let (archive_path, archive_sha256, manifest_sha256) = create_test_bundle_archive(temp.path());
    let artifact_url = format!("file://{}", archive_path.display());
    let descriptor = test_descriptor(&artifact_url, &archive_sha256, &manifest_sha256);

    let marker_cache = temp.path().join("marker-cache");
    acquire_coreml_bundle_for(&marker_cache, &descriptor).unwrap();
    let marker_bundle =
        content_addressed_bundle_path(&marker_cache, descriptor.manifest_sha256).unwrap();
    let marker = completion_marker_path(&marker_bundle).unwrap();
    fs::write(
        &marker,
        format!(r#"{{"schema_version":1,"manifest_sha256":"{B_HASH}"}}"#),
    )
    .unwrap();
    let error = acquire_coreml_bundle_for(&marker_cache, &descriptor).unwrap_err();
    assert!(model_acquisition_integrity_error(&error));
    assert!(marker_bundle.is_dir());

    let content_cache = temp.path().join("content-cache");
    acquire_coreml_bundle_for(&content_cache, &descriptor).unwrap();
    let content_bundle =
        content_addressed_bundle_path(&content_cache, descriptor.manifest_sha256).unwrap();
    let model = content_bundle.join("document.mlpackage/Data/model.bin");
    fs::write(&model, b"tampered").unwrap();
    let error = acquire_coreml_bundle_for(&content_cache, &descriptor).unwrap_err();
    assert!(model_acquisition_integrity_error(&error));
    assert_eq!(fs::read(model).unwrap(), b"tampered");
    assert!(completion_marker_matches(&content_bundle, &manifest_sha256).unwrap());
}

fn test_descriptor<'a>(
    artifact_url: &'a str,
    archive_sha256: &'a str,
    manifest_sha256: &'a str,
) -> CoreMlBundleContract<'a> {
    CoreMlBundleContract {
        artifact_url,
        artifact_name: "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
        archive_sha256,
        manifest_sha256,
        query_batch_size: None,
        ..COREML_BUNDLE_CONTRACT
    }
}

fn write_test_archive(path: &Path, entries: &[(&str, tar::EntryType, &[u8])]) {
    let file = fs::File::create(path).unwrap();
    let encoder = xz2::write::XzEncoder::new(file, 1);
    let mut archive = tar::Builder::new(encoder);
    for (path, entry_type, body) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(*entry_type);
        header.set_mode(0o600);
        header.set_size(body.len() as u64);
        let name = path.as_bytes();
        assert!(name.len() < 100);
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..name.len()].copy_from_slice(name);
        header.set_cksum();
        archive.append(&header, *body).unwrap();
    }
    let encoder = archive.into_inner().unwrap();
    encoder.finish().unwrap();
}

fn create_test_bundle_archive(root: &Path) -> (PathBuf, String, String) {
    let root_name = "ctx-multilingual-e5-small-coreml-fp16-1.0.0";
    let source = root.join(root_name);
    fs::create_dir(&source).unwrap();
    let manifest_sha256 = create_test_bundle(&source);
    let archive_path = root.join(format!("{root_name}.tar.xz"));
    write_bundle_archive(&archive_path, &source, root_name);
    let archive_sha256 = sha256_path(&archive_path);
    (archive_path, archive_sha256, manifest_sha256)
}

fn create_test_bundle(root: &Path) -> String {
    let payloads = [
        ("LICENSES/MODEL_LICENSE.txt", b"license\n".as_slice()),
        ("PROVENANCE.json", b"{}".as_slice()),
        ("THIRD_PARTY_NOTICES.md", b"notices\n".as_slice()),
        ("document.mlpackage/Data/model.bin", b"model".as_slice()),
        ("tokenizer.json", b"{}".as_slice()),
    ];
    let mut files = Vec::new();
    for (relative, body) in payloads {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        files.push(json!({
            "path": relative,
            "size_bytes": body.len(),
            "sha256": format!("{:x}", Sha256::digest(body)),
        }));
    }
    files.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    let manifest = json!({
        "schema_version": 1,
        "bundle_id": "ctx.multilingual-e5-small.coreml.fp16",
        "bundle_version": "1.0.0",
        "model": {
            "id": "intfloat/multilingual-e5-small",
            "source_revision": "614241f622f53c4eeff9890bdc4f31cfecc418b3",
            "embedding_space_id": "e5-small-v1:mean-pool:l2:query-passage",
            "precision": "fp16",
        },
        "tensor_contract": {
            "inputs": [
                {"name": "input_ids", "dtype": "int32", "shape": [16, 512]},
                {"name": "attention_mask", "dtype": "int32", "shape": [16, 512]},
                {"name": "token_type_ids", "dtype": "int32", "shape": [16, 512]},
            ],
            "output": {"name": "sentence_embeddings", "dtype": "float32", "shape": [16, 384]},
            "document_batch_size": 16,
            "max_sequence_length": 512,
            "embedding_dimensions": 384,
            "document_prefix": "passage: ",
            "query_prefix": "query: ",
            "pooling": "attention_mask_mean",
            "normalization": "l2",
        },
        "artifacts": {
            "tokenizer": "tokenizer.json",
            "document_model": "document.mlpackage",
        },
        "files": files,
    });
    let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    bytes.push(b'\n');
    fs::write(root.join("manifest.json"), &bytes).unwrap();
    format!("{:x}", Sha256::digest(bytes))
}

fn write_bundle_archive(path: &Path, root: &Path, root_name: &str) {
    let file = fs::File::create(path).unwrap();
    let encoder = xz2::write::XzEncoder::new(file, 1);
    let mut archive = tar::Builder::new(encoder);
    archive.append_dir(root_name, root).unwrap();
    let mut paths = Vec::new();
    collect_paths(root, root, &mut paths);
    paths.sort();
    for relative in paths {
        let source = root.join(&relative);
        let archive_name = Path::new(root_name).join(&relative);
        if source.is_dir() {
            archive.append_dir(archive_name, source).unwrap();
        } else {
            archive.append_path_with_name(source, archive_name).unwrap();
        }
    }
    let encoder = archive.into_inner().unwrap();
    encoder.finish().unwrap();
}

fn collect_paths(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        paths.push(path.strip_prefix(root).unwrap().to_path_buf());
        if path.is_dir() {
            collect_paths(root, &path, paths);
        }
    }
}

fn sha256_path(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}
