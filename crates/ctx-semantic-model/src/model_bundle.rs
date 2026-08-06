mod manifest;
pub(crate) use manifest::{
    validate_bundle_contract, validate_relative_path, VerifiedModelBundle, MAX_BUNDLE_BYTES,
    MAX_BUNDLE_DIRECTORIES, MAX_BUNDLE_FILES, MAX_FILE_BYTES,
};
#[cfg(test)]
pub(crate) use manifest::{
    BundleArtifacts, BundleFile, ModelBundleManifest, ModelIdentity, TensorContract, TensorSpec,
    MANIFEST_FILE, MAX_MANIFEST_BYTES,
};

mod secure_fs;

mod verify;
pub(crate) use verify::verify_model_bundle;

mod cache;
pub(crate) use cache::{
    cached_signed_bundle, create_signed_bundle_staging_directory,
    create_signed_bundle_staging_file, lock_signed_bundle_cache, prepare_signed_bundle_cache,
    publish_signed_bundle, remove_signed_bundle_staging_directory,
    repair_interrupted_signed_bundle_publication, signed_bundle_cache_error_kind,
    signed_bundle_cache_status, SignedBundleCacheErrorKind, SignedBundleCacheStatus,
};
#[cfg(test)]
pub(crate) use cache::{
    completion_marker_matches, completion_marker_path, content_addressed_bundle_path,
    set_signed_bundle_cache_lock_contended_hook, write_completion_marker_atomic,
};

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::{
        completion_marker_matches, completion_marker_path, content_addressed_bundle_path,
        manifest::{sha256_bytes, validate_sha256},
        prepare_signed_bundle_cache, publish_signed_bundle, verify_model_bundle,
        write_completion_marker_atomic, BundleArtifacts, BundleFile, ModelBundleManifest,
        ModelIdentity, TensorContract, TensorSpec, MANIFEST_FILE, MAX_MANIFEST_BYTES,
    };
    use crate::model_contract::COREML_BUNDLE_CONTRACT;

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn valid_manifest(root: &Path) -> ModelBundleManifest {
        let expected = &COREML_BUNDLE_CONTRACT;
        let payloads = [
            (expected.tokenizer_artifact, b"{}".as_slice()),
            ("document.mlpackage/Data/model.bin", b"model".as_slice()),
            ("PROVENANCE.json", b"{}".as_slice()),
            ("THIRD_PARTY_NOTICES.md", b"notices\n".as_slice()),
            ("LICENSES/MODEL_LICENSE.txt", b"license\n".as_slice()),
        ];
        let mut files: Vec<_> = payloads
            .into_iter()
            .map(|(path, body)| {
                write(&root.join(path), body);
                BundleFile {
                    path: path.to_owned(),
                    size_bytes: body.len() as u64,
                    sha256: sha256_bytes(body),
                }
            })
            .collect();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        ModelBundleManifest {
            schema_version: expected.schema_version,
            bundle_id: expected.bundle_id.to_owned(),
            bundle_version: expected.bundle_version.to_owned(),
            model: ModelIdentity {
                id: expected.model_id.to_owned(),
                source_revision: expected.source_revision.to_owned(),
                embedding_space_id: expected.embedding_space_id.to_owned(),
                precision: expected.precision.to_owned(),
            },
            tensor_contract: TensorContract {
                inputs: expected
                    .inputs
                    .into_iter()
                    .map(|(name, dtype)| TensorSpec {
                        name: name.to_owned(),
                        dtype: dtype.to_owned(),
                        shape: vec![expected.document_batch_size, expected.max_sequence_length],
                    })
                    .collect(),
                output: TensorSpec {
                    name: expected.output_name.to_owned(),
                    dtype: expected.output_dtype.to_owned(),
                    shape: vec![expected.document_batch_size, expected.embedding_dimensions],
                },
                document_batch_size: expected.document_batch_size,
                query_batch_size: None,
                max_sequence_length: expected.max_sequence_length,
                embedding_dimensions: expected.embedding_dimensions,
                document_prefix: expected.document_prefix.to_owned(),
                query_prefix: expected.query_prefix.to_owned(),
                pooling: expected.pooling.to_owned(),
                normalization: expected.normalization.to_owned(),
            },
            artifacts: BundleArtifacts {
                tokenizer: expected.tokenizer_artifact.to_owned(),
                document_model: expected.document_model_artifact.to_owned(),
                query_model: None,
            },
            files,
        }
    }

    fn create_valid_bundle(root: &Path) -> ModelBundleManifest {
        let manifest = valid_manifest(root);
        write_manifest(root, &manifest);
        manifest
    }

    fn create_publishable_bundle(root: &Path) -> String {
        let mut manifest = valid_manifest(root);
        add_query_model(root, &mut manifest);
        write_manifest(root, &manifest);
        verify_model_bundle(root).unwrap().manifest_sha256
    }

    fn add_query_model(root: &Path, manifest: &mut ModelBundleManifest) {
        let path = "query.mlpackage/Data/model.bin";
        let body = b"query model";
        write(&root.join(path), body);
        manifest.files.push(BundleFile {
            path: path.to_owned(),
            size_bytes: body.len() as u64,
            sha256: sha256_bytes(body),
        });
        manifest
            .files
            .sort_by(|left, right| left.path.cmp(&right.path));
        manifest.artifacts.query_model =
            Some(COREML_BUNDLE_CONTRACT.query_model_artifact.to_owned());
        manifest.tensor_contract.query_batch_size = COREML_BUNDLE_CONTRACT.query_batch_size;
    }

    fn write_manifest(root: &Path, manifest: &ModelBundleManifest) {
        let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        bytes.push(b'\n');
        fs::write(root.join(MANIFEST_FILE), bytes).unwrap();
    }

    #[test]
    fn verifies_complete_bundle_and_reports_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        create_valid_bundle(temp.path());
        let verified = verify_model_bundle(temp.path()).unwrap();
        assert_eq!(verified.manifest.model.id, COREML_BUNDLE_CONTRACT.model_id);
        assert_eq!(
            verified.tokenizer_path(),
            temp.path().join("tokenizer.json")
        );
        assert_eq!(
            verified.document_model_path(),
            temp.path().join("document.mlpackage")
        );
        assert_eq!(verified.query_model_path(), None);
        validate_sha256(&verified.manifest_sha256, "test hash").unwrap();
    }

    #[test]
    fn verifies_distinct_document_and_query_batch_contracts() {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = valid_manifest(temp.path());
        add_query_model(temp.path(), &mut manifest);
        write_manifest(temp.path(), &manifest);

        let verified = verify_model_bundle(temp.path()).unwrap();
        assert_eq!(
            verified.manifest.tensor_contract.document_batch_size,
            COREML_BUNDLE_CONTRACT.document_batch_size
        );
        assert_eq!(
            verified.manifest.tensor_contract.query_batch_size,
            COREML_BUNDLE_CONTRACT.query_batch_size
        );
        assert_eq!(
            verified.query_model_path(),
            Some(temp.path().join("query.mlpackage"))
        );
    }

    #[test]
    fn rejects_hash_mismatch_and_unlisted_payload() {
        let temp = tempfile::tempdir().unwrap();
        create_valid_bundle(temp.path());
        fs::write(temp.path().join("tokenizer.json"), b"changed").unwrap();
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("size mismatch"));

        let temp = tempfile::tempdir().unwrap();
        create_valid_bundle(temp.path());
        fs::write(temp.path().join("unexpected"), b"x").unwrap();
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("file set"));
    }

    #[test]
    fn rejects_traversal_unknown_fields_and_oversized_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = create_valid_bundle(temp.path());
        manifest.files[0].path = "../tokenizer.json".to_owned();
        fs::write(
            temp.path().join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("relative path"));

        let temp = tempfile::tempdir().unwrap();
        create_valid_bundle(temp.path());
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(temp.path().join(MANIFEST_FILE)).unwrap()).unwrap();
        value["unknown"] = serde_json::json!(true);
        fs::write(
            temp.path().join(MANIFEST_FILE),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(verify_model_bundle(temp.path()).is_err());

        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(MANIFEST_FILE),
            vec![b' '; MAX_MANIFEST_BYTES as usize + 1],
        )
        .unwrap();
        let error = verify_model_bundle(temp.path()).unwrap_err();
        assert!(format!("{error:#}").contains("size limit"));
    }

    #[test]
    fn rejects_unpinned_model_source_revision() {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = create_valid_bundle(temp.path());
        manifest.model.source_revision = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned();
        fs::write(
            temp.path().join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("source revision"));
    }

    #[test]
    fn rejects_nonproduction_document_tensor_shape() {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = create_valid_bundle(temp.path());
        manifest.tensor_contract.document_batch_size = 512;
        for input in &mut manifest.tensor_contract.inputs {
            input.shape[0] = 512;
        }
        manifest.tensor_contract.output.shape[0] = 512;
        fs::write(
            temp.path().join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("document tensor contract must use fixed batch 16"));
    }

    #[test]
    fn rejects_swapped_role_batches_and_query_contract_mismatches() {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = valid_manifest(temp.path());
        add_query_model(temp.path(), &mut manifest);
        manifest.tensor_contract.document_batch_size = 1;
        manifest.tensor_contract.query_batch_size = Some(16);
        for input in &mut manifest.tensor_contract.inputs {
            input.shape[0] = 1;
        }
        manifest.tensor_contract.output.shape[0] = 1;
        write_manifest(temp.path(), &manifest);
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("document tensor contract must use fixed batch 16"));

        let temp = tempfile::tempdir().unwrap();
        let mut manifest = valid_manifest(temp.path());
        add_query_model(temp.path(), &mut manifest);
        manifest.tensor_contract.query_batch_size = None;
        write_manifest(temp.path(), &manifest);
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("query tensor contract must use fixed batch 1"));

        let temp = tempfile::tempdir().unwrap();
        let mut manifest = valid_manifest(temp.path());
        manifest.tensor_contract.query_batch_size = Some(1);
        write_manifest(temp.path(), &manifest);
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("query batch size requires a query model artifact"));
    }

    #[test]
    fn rejects_legacy_batch_size_field() {
        let temp = tempfile::tempdir().unwrap();
        create_valid_bundle(temp.path());
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(temp.path().join(MANIFEST_FILE)).unwrap()).unwrap();
        value["tensor_contract"]["batch_size"] = serde_json::json!(16);
        value["tensor_contract"]
            .as_object_mut()
            .unwrap()
            .remove("document_batch_size");
        fs::write(
            temp.path().join(MANIFEST_FILE),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(verify_model_bundle(temp.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_in_bundle_and_manifest() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        create_valid_bundle(temp.path());
        fs::remove_file(temp.path().join("tokenizer.json")).unwrap();
        symlink("PROVENANCE.json", temp.path().join("tokenizer.json")).unwrap();
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("symlink"));

        let outside = tempfile::NamedTempFile::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join(MANIFEST_FILE)).unwrap();
        assert!(verify_model_bundle(root.path()).is_err());
    }

    #[test]
    fn builds_content_addressed_path_and_atomic_marker() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        create_valid_bundle(&source);
        let hash = verify_model_bundle(&source).unwrap().manifest_sha256;
        let bundle = content_addressed_bundle_path(temp.path(), &hash).unwrap();
        fs::create_dir_all(bundle.parent().unwrap()).unwrap();
        fs::rename(&source, &bundle).unwrap();
        assert!(bundle.ends_with(format!("{}/{hash}", &hash[..2])));
        assert!(!completion_marker_matches(&bundle, &hash).unwrap());
        write_completion_marker_atomic(&bundle).unwrap();
        assert!(completion_marker_matches(&bundle, &hash).unwrap());
        assert!(content_addressed_bundle_path(temp.path(), &"A".repeat(64)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn signed_bundle_publication_rejects_symlinked_cache_parent() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let cache_root = temp.path().join("cache");
        prepare_signed_bundle_cache(&cache_root).unwrap();
        let staging = temp.path().join("staging");
        fs::create_dir(&staging).unwrap();
        let hash = create_publishable_bundle(&staging);
        let contract = crate::model_contract::CoreMlBundleContract {
            manifest_sha256: &hash,
            ..COREML_BUNDLE_CONTRACT
        };
        let digest_root = cache_root.join("semantic-model-bundles/sha256");
        fs::create_dir_all(&digest_root).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, digest_root.join(&hash[..2])).unwrap();

        assert!(publish_signed_bundle(&cache_root, &staging, &contract).is_err());
        assert!(staging.is_dir());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn signed_bundle_publication_rolls_back_when_marker_cannot_commit() {
        let temp = tempfile::tempdir().unwrap();
        let cache_root = temp.path().join("cache");
        prepare_signed_bundle_cache(&cache_root).unwrap();
        let staging = temp.path().join("staging");
        fs::create_dir(&staging).unwrap();
        let hash = create_publishable_bundle(&staging);
        let contract = crate::model_contract::CoreMlBundleContract {
            manifest_sha256: &hash,
            ..COREML_BUNDLE_CONTRACT
        };
        let final_path = content_addressed_bundle_path(&cache_root, &hash).unwrap();
        fs::create_dir_all(final_path.parent().unwrap()).unwrap();
        let marker = completion_marker_path(&final_path).unwrap();
        fs::create_dir(&marker).unwrap();

        assert!(publish_signed_bundle(&cache_root, &staging, &contract).is_err());
        assert!(!final_path.exists());
        assert!(!staging.exists());
        assert!(marker.is_dir());
    }

    #[test]
    fn canonical_contract_drives_manifest_validation() {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = valid_manifest(temp.path());
        add_query_model(temp.path(), &mut manifest);
        write_manifest(temp.path(), &manifest);
        let verified = verify_model_bundle(temp.path()).unwrap();
        let contract = crate::model_contract::CoreMlBundleContract {
            manifest_sha256: &verified.manifest_sha256,
            ..COREML_BUNDLE_CONTRACT
        };
        super::validate_bundle_contract(&verified, &contract).unwrap();

        manifest.model.id.push_str("-drift");
        write_manifest(temp.path(), &manifest);
        assert!(verify_model_bundle(temp.path()).is_err());
    }
}
