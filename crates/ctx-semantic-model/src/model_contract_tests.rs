use sha2::{Digest, Sha256};

use crate::model_contract::*;

#[test]
fn semantic_model_identity_and_descriptor_bytes_are_frozen() {
    let contract = semantic_model_contract();
    assert_eq!(contract.contract_revision(), 2);
    assert_eq!(contract.model_id(), "intfloat/multilingual-e5-small");
    assert_eq!(contract.dimensions(), 384);
    assert_eq!(contract.normalization(), "l2");
    assert_eq!(SEMANTIC_MODEL_KEY, "e5-small-v1:mean-pool:l2:query-passage");
    assert_eq!(
        SEMANTIC_MODEL_REVISION,
        "614241f622f53c4eeff9890bdc4f31cfecc418b3"
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
    assert_eq!(
        descriptor_sha256,
        "cb3a09cf8923c26a87f1d52caee9941ddf3726e0e5adbc4fb897eb0c347d7ebc"
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
fn semantic_generation_descriptor_covers_every_runtime_and_artifact_authority() {
    let descriptor = semantic_model_contract_descriptor();
    for required in [
        format!("max_sequence_length={SEMANTIC_MAX_SEQUENCE_LENGTH}"),
        format!("pooling={SEMANTIC_POOLING}"),
        format!("query_prefix={SEMANTIC_QUERY_PREFIX}"),
        format!("passage_prefix={SEMANTIC_PASSAGE_PREFIX}"),
        format!(
            "coreml_manifest_sha256={}",
            COREML_BUNDLE_CONTRACT.manifest_sha256
        ),
        format!(
            "coreml_inputs={}:{}",
            COREML_BUNDLE_CONTRACT.inputs[0].0, COREML_BUNDLE_CONTRACT.inputs[0].1
        ),
        format!(
            "coreml_output={}:{}",
            COREML_BUNDLE_CONTRACT.output_name, COREML_BUNDLE_CONTRACT.output_dtype
        ),
    ] {
        assert!(
            descriptor.contains(&required),
            "missing descriptor authority: {required}"
        );
    }
    for variant in [
        SemanticOrtModelVariant::CpuFp32,
        SemanticOrtModelVariant::AcceleratorO4Fp16,
    ] {
        for file in variant.required_files() {
            assert!(descriptor.contains(&format!(
                "variant={}|file={}:{}:{}",
                variant.as_str(),
                file.path,
                file.size,
                file.sha256
            )));
        }
    }
    for backend in [
        SemanticBackendKind::Cpu,
        SemanticBackendKind::CoreMl,
        SemanticBackendKind::OrtCuda,
        SemanticBackendKind::WindowsMl,
    ] {
        assert!(descriptor.contains(&format!(
            "backend_variant={}:{}:{}",
            backend.as_str(),
            backend.execution_provider(),
            backend.contract_id()
        )));
    }
}

#[test]
fn e5_query_and_passage_policy_applies_each_role_prefix_exactly_once() {
    assert_eq!(
        semantic_e5_query_text("find a daemon failure"),
        "query: find a daemon failure"
    );
    assert_eq!(
        semantic_e5_query_text("  query: find a daemon failure"),
        "query: find a daemon failure"
    );
    assert_eq!(
        semantic_e5_passage_text("daemon failed to restart"),
        "passage: daemon failed to restart"
    );
    assert_eq!(
        semantic_e5_passage_text("  passage: daemon failed to restart"),
        "passage: daemon failed to restart"
    );
}
