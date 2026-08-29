use sha2::{Digest, Sha256};

use crate::model_contract::*;

#[test]
fn semantic_model_identity_and_descriptor_bytes_are_frozen() {
    let contract = semantic_model_contract();
    assert_eq!(contract.contract_revision(), 2);
    assert_eq!(contract.contract_version(), 2);
    assert_eq!(contract.model_key(), SEMANTIC_MODEL_KEY);
    assert_eq!(contract.model_id(), "intfloat/multilingual-e5-small");
    assert_eq!(contract.model_revision(), SEMANTIC_MODEL_REVISION);
    assert_eq!(
        contract.tokenizer_fingerprint(),
        "sha256:0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39"
    );
    assert_eq!(
        contract.tokenizer_behavior_fingerprint(),
        "sha256:c61e5e2d53de677ea9023debfa95e4c618b601c63e2de6c43c0c64850560c2c0"
    );
    assert_eq!(contract.dimensions(), 384);
    assert_eq!(contract.max_sequence_length(), 512);
    assert_eq!(contract.pooling(), "attention-mask-mean");
    assert_eq!(contract.normalization(), "l2");
    assert_eq!(contract.query_prefix(), "query: ");
    assert_eq!(contract.document_prefix(), "passage: ");
    assert_eq!(contract.language_scope(), "unicode-global");
    assert_eq!(SEMANTIC_MODEL_KEY, "e5-small-v1:mean-pool:l2:query-passage");
    assert_eq!(
        SEMANTIC_MODEL_REVISION,
        "614241f622f53c4eeff9890bdc4f31cfecc418b3"
    );
    assert_eq!(
        COREML_BUNDLE_CONTRACT.artifact_url,
        "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/stable/1.0.0/ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz"
    );
    assert_eq!(
        SEMANTIC_REQUIRED_MODEL_FILES
            .iter()
            .map(|file| (file.path, file.size, file.sha256))
            .collect::<Vec<_>>(),
        [
            (
                "onnx/model.onnx",
                470_268_510,
                "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665"
            ),
            (
                "tokenizer.json",
                17_082_730,
                "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39"
            ),
            (
                "config.json",
                655,
                "69137736cab8b8903a07fe8afaafdda25aac55415a12a55d1bffa9f581abf959"
            ),
            (
                "special_tokens_map.json",
                167,
                "d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7"
            ),
            (
                "tokenizer_config.json",
                443,
                "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b"
            ),
        ]
    );
    let accelerator_model = SemanticOrtModelVariant::AcceleratorO4Fp16
        .required_files()
        .next()
        .expect("accelerator model contract");
    assert_eq!(
        (
            accelerator_model.path,
            accelerator_model.size,
            accelerator_model.sha256,
        ),
        (
            "onnx/model.onnx",
            235_052_531,
            "4654c156f3e4171abc9c716cdb771bf9116455d15ac1aab364aeeede0e3205b0"
        )
    );
    let descriptor = semantic_model_contract_descriptor();
    let descriptor_sha256 = format!("{:x}", Sha256::digest(descriptor.as_bytes()));
    assert_eq!(descriptor, contract.descriptor());
    assert_eq!(
        semantic_model_contract_fingerprint(),
        contract.fingerprint()
    );
    assert_eq!(
        semantic_model_contract_fingerprint(),
        format!("sha256:{descriptor_sha256}")
    );
    assert_eq!(
        descriptor_sha256,
        "611f11c9b715543137d1b6be8d87497a2b6ef4945d425f3c0b973d2cb0c6036d"
    );

    assert!(std::ptr::eq(contract, semantic_model_contract()));
}

#[test]
fn tokenizer_behavior_identity_covers_every_fastembed_behavior_file_only() {
    assert_eq!(
        SEMANTIC_TOKENIZER_BEHAVIOR_PATHS,
        [
            "tokenizer.json",
            "config.json",
            "special_tokens_map.json",
            "tokenizer_config.json",
        ]
    );
    let baseline = semantic_tokenizer_behavior_fingerprint();
    for path in SEMANTIC_TOKENIZER_BEHAVIOR_PATHS {
        let mut changed = SEMANTIC_REQUIRED_MODEL_FILES.to_vec();
        let file = changed
            .iter_mut()
            .find(|file| file.path == *path)
            .expect("behavior file must be pinned");
        *file = SemanticModelFile::new(
            file.path,
            file.size,
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        );
        assert_ne!(
            semantic_tokenizer_behavior_fingerprint_for(&changed),
            baseline,
            "{path} did not participate in tokenizer behavior identity"
        );
    }

    let mut changed_model_only = SEMANTIC_REQUIRED_MODEL_FILES.to_vec();
    changed_model_only[0] = SemanticModelFile::new(
        "onnx/model.onnx",
        470_268_510,
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    );
    assert_eq!(
        semantic_tokenizer_behavior_fingerprint_for(&changed_model_only),
        baseline,
        "execution artifact identity leaked into tokenizer behavior identity"
    );
    assert_eq!(
        semantic_tokenizer_fingerprint(),
        "sha256:0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
        "the Flat-format tokenizer.json identity must remain unchanged"
    );
}

#[test]
fn exact_builtin_contract_exposes_only_the_frozen_pre_refactor_alias() {
    let contract = semantic_model_contract();
    let descriptor = contract
        .legacy_builtin_descriptor_alias()
        .expect("exact built-in contract must expose its migration alias");
    assert_eq!(
        format!("sha256:{:x}", Sha256::digest(descriptor.as_bytes())),
        "sha256:c812eb325bc5e90e7278b2b8da3933206340c5b5a46fd678be40016e06a89fc3"
    );
    assert!(descriptor.starts_with("ctx-semantic-e5-v2|backend=multilingual-e5|"));
    assert!(descriptor.contains("|backend_variant=cpu:CPUExecutionProvider:"));
    assert!(descriptor.contains("|coreml_manifest_sha256="));

    let non_builtin = contract
        .clone()
        .with_test_tokenizer_behavior_fingerprint("sha256:test-only");
    assert_eq!(non_builtin.legacy_builtin_descriptor_alias(), None);
    let revised_language_scope = contract
        .clone()
        .with_test_language_scope("test-only-language-scope");
    assert_eq!(
        revised_language_scope.legacy_builtin_descriptor_alias(),
        None
    );
}

#[test]
fn provisioning_inventory_keeps_both_pinned_e5_variants_exact() {
    assert_eq!(
        semantic_required_model_file_count(SemanticOrtModelVariant::CpuFp32),
        5
    );
    assert_eq!(
        semantic_required_model_file_count(SemanticOrtModelVariant::AcceleratorO4Fp16),
        5
    );
    assert_eq!(semantic_provisioning_model_path_count(), 7);
    assert!(SemanticOrtModelVariant::CpuFp32
        .required_files()
        .all(|file| {
            semantic_provisioning_model_path_matches(file.path)
                && semantic_required_model_file_matches(
                    SemanticOrtModelVariant::CpuFp32,
                    file.path,
                    file.size,
                    file.sha256,
                )
        }));
    assert!(SemanticOrtModelVariant::AcceleratorO4Fp16
        .required_files()
        .all(|file| {
            semantic_provisioning_model_path_matches(file.path)
                && semantic_required_model_file_matches(
                    SemanticOrtModelVariant::AcceleratorO4Fp16,
                    file.path,
                    file.size,
                    file.sha256,
                )
        }));
    assert_eq!(
        SEMANTIC_PROVISIONING_MODEL_PATHS
            .iter()
            .copied()
            .filter(|path| {
                !SemanticOrtModelVariant::CpuFp32
                    .required_files()
                    .any(|file| file.path == *path)
            })
            .collect::<Vec<_>>(),
        ["LICENSE", "manifest.json"]
    );
}

#[test]
fn semantic_descriptor_excludes_executor_and_publication_identity() {
    let contract_descriptor = semantic_model_contract_descriptor();
    for required in [
        format!("model_key={SEMANTIC_MODEL_KEY}"),
        format!("model_id={SEMANTIC_MODEL_ID}"),
        format!("model_revision={SEMANTIC_MODEL_REVISION}"),
        format!("tokenizer_fingerprint={}", semantic_tokenizer_fingerprint()),
        format!(
            "tokenizer_behavior_fingerprint={}",
            semantic_tokenizer_behavior_fingerprint()
        ),
        format!("dimensions={SEMANTIC_DIMENSIONS}"),
        format!("max_sequence_length={SEMANTIC_MAX_SEQUENCE_LENGTH}"),
        format!("pooling={SEMANTIC_POOLING}"),
        format!("normalization={SEMANTIC_NORMALIZATION}"),
        format!("query_prefix={SEMANTIC_QUERY_PREFIX}"),
        format!("document_prefix={SEMANTIC_PASSAGE_PREFIX}"),
        format!("language_scope={SEMANTIC_LANGUAGE_SCOPE}"),
    ] {
        assert!(
            contract_descriptor.contains(&required),
            "missing vector-space authority: {required}"
        );
    }
    for executor_only in [
        "model_contract=",
        "variant=",
        "file=",
        "backend_variant=",
        "execution_provider",
        "coreml_",
        "artifact_url=",
        "archive_sha256=",
        "runtime=",
    ] {
        assert!(
            !contract_descriptor.contains(executor_only),
            "executor identity leaked into vector contract: {executor_only}"
        );
    }
}

#[test]
fn e5_query_and_passage_policy_applies_each_role_prefix_exactly_once() {
    let contract = semantic_model_contract();
    assert_eq!(
        contract.query_text("find a daemon failure"),
        "query: find a daemon failure"
    );
    assert_eq!(
        contract.query_text("  query: find a daemon failure"),
        "query: find a daemon failure"
    );
    assert_eq!(
        contract.document_text("daemon failed to restart"),
        "passage: daemon failed to restart"
    );
    assert_eq!(
        contract.document_text("  passage: daemon failed to restart"),
        "passage: daemon failed to restart"
    );
    assert_eq!(
        semantic_e5_query_text("find a daemon failure"),
        contract.query_text("find a daemon failure")
    );
    assert_eq!(
        semantic_e5_query_text("  query: find a daemon failure"),
        contract.query_text("  query: find a daemon failure")
    );
    assert_eq!(
        semantic_e5_passage_text("daemon failed to restart"),
        contract.document_text("daemon failed to restart")
    );
    assert_eq!(
        semantic_e5_passage_text("  passage: daemon failed to restart"),
        contract.document_text("  passage: daemon failed to restart")
    );
}

#[test]
fn external_space_validation_and_dimension_aware_work_units_are_bounded() {
    let common = ExternalSemanticSpace::new("acme/multilingual-v2:rev@host+fp=abc_1", 768)
        .expect("common opaque model IDs are header-safe");
    assert_eq!(common.space_id(), "acme/multilingual-v2:rev@host+fp=abc_1");
    assert_eq!(common.dimensions(), 768);
    assert_eq!(common.max_inputs_per_request(), 341);

    assert_eq!(
        ExternalSemanticSpace::new("small", 1)
            .unwrap()
            .max_inputs_per_request(),
        512
    );
    assert_eq!(
        ExternalSemanticSpace::new("maximum", MAX_EXTERNAL_SEMANTIC_DIMENSIONS)
            .unwrap()
            .max_inputs_per_request(),
        64
    );

    for unsafe_id in [
        "",
        "bad space",
        "bad\nheader",
        "unicode-世界",
        "brackets[bad]",
    ] {
        assert!(ExternalSemanticSpace::new(unsafe_id, 384).is_err());
    }
    assert!(
        ExternalSemanticSpace::new("x".repeat(MAX_EXTERNAL_SEMANTIC_SPACE_ID_BYTES + 1), 384,)
            .is_err()
    );
    assert!(ExternalSemanticSpace::new("zero", 0).is_err());
    assert!(ExternalSemanticSpace::new("too-large", MAX_EXTERNAL_SEMANTIC_DIMENSIONS + 1).is_err());
}

#[test]
fn external_contract_keeps_endpoint_out_of_vector_identity_and_preserves_raw_text() {
    let endpoint = "https://embed.example.test/base/";
    let space = ExternalSemanticSpace::new("acme/multilingual-v2", 1_536).unwrap();
    let contract = SemanticModelContract::external_http(endpoint, space.clone());

    assert_eq!(contract.external_space(), Some(&space));
    assert_eq!(contract.external_http_endpoint(), Some(endpoint));
    assert_eq!(contract.model_key(), space.space_id());
    assert_eq!(contract.model_id(), "external-http-v1");
    assert_eq!(contract.model_revision(), space.space_id());
    assert_eq!(contract.tokenizer_fingerprint(), "endpoint-owned-v1");
    assert_eq!(
        contract.tokenizer_behavior_fingerprint(),
        "endpoint-owned-v1"
    );
    assert_eq!(contract.pooling(), "endpoint-owned");
    assert_eq!(contract.normalization(), "l2");
    assert_eq!(contract.dimensions(), 1_536);
    assert!(!contract.descriptor().contains(endpoint));
    assert!(contract.executor_route_identity().starts_with("sha256:"));
    let moved = SemanticModelContract::external_http(
        "https://other.example.test/embeddings/",
        space.clone(),
    );
    assert_eq!(contract.fingerprint(), moved.fingerprint());
    assert_eq!(contract.descriptor(), moved.descriptor());
    assert_ne!(
        contract.executor_route_identity(),
        moved.executor_route_identity()
    );
    assert_eq!(
        contract.query_text("  query-looking text"),
        "  query-looking text"
    );
    assert_eq!(
        contract.document_text("passage: raw document"),
        "passage: raw document"
    );
    assert_eq!(
        contract
            .prepare_documents(vec!["  first".to_owned(), "second".to_owned()])
            .into_texts(),
        ["  first", "second"]
    );

    let changed_endpoint =
        SemanticModelContract::external_http("https://embed.example.test/other/", space.clone());
    let changed_space = SemanticModelContract::external_http(
        endpoint,
        ExternalSemanticSpace::new("acme/multilingual-v3", 1_536).unwrap(),
    );
    let changed_dimensions = SemanticModelContract::external_http(
        endpoint,
        ExternalSemanticSpace::new(space.space_id(), 768).unwrap(),
    );
    assert_eq!(contract.fingerprint(), changed_endpoint.fingerprint());
    assert_ne!(contract.fingerprint(), changed_space.fingerprint());
    assert_ne!(contract.fingerprint(), changed_dimensions.fingerprint());
    assert_eq!(contract.model_id(), changed_endpoint.model_id());
    assert_ne!(contract.model_revision(), changed_space.model_revision());
}

#[test]
fn builtin_contract_uses_the_fixed_executor_route_sentinel() {
    assert_eq!(
        semantic_model_contract().executor_route_identity(),
        BUILTIN_SEMANTIC_EXECUTOR_ROUTE_IDENTITY
    );
}
