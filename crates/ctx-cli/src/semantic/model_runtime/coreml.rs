use std::path::Path;

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
use std::{env, fs, path::PathBuf};

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
use anyhow::Context;
use anyhow::{anyhow, Result};

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
use super::{
    cache::{
        commit_compile_destination, create_private_dir_all, discard_compile_destination,
        invalidate_compiled_model_cache, prepare_compile_destination,
    },
    SemanticEmbeddingBackend,
};
use super::{BackendPreference, SemanticDaemonModelAcquisition, SemanticEmbedder};
#[cfg(all(ctx_semantic_fastembed, any(target_os = "macos", test)))]
use super::{SemanticModelAcquisitionBackend, SemanticModelAcquisitionSource};
#[cfg(any(target_os = "macos", test))]
use crate::semantic::model_acquisition::{
    coreml_descriptor_provisioned, model_acquisition_error_kind, CoreMlAcquisitionSource,
    ModelAcquisitionErrorKind,
};
#[cfg(all(ctx_semantic_fastembed, any(target_os = "macos", test)))]
use crate::semantic::resource_policy::semantic_model_load_deferred;
#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
use crate::semantic::{
    health_search::semantic_embed_policy_for,
    model_acquisition::{
        acquire_coreml_bundle_for_daemon, cached_coreml_bundle, AcquiredCoreMlBundle,
    },
    model_bundle::VerifiedModelBundle,
    model_contract::{
        semantic_model_key, SEMANTIC_DIMENSIONS, SEMANTIC_MODEL_ID, SEMANTIC_MODEL_REVISION,
        SEMANTIC_QUERY_PREFIX,
    },
    resource_policy::SemanticSystemResources,
};
#[cfg(any(target_os = "macos", test))]
use crate::semantic::{
    health_search::semantic_model_acquisition_integrity_error,
    model_contract::SEMANTIC_PASSAGE_PREFIX, resource_policy::SemanticComputeClass,
};

#[cfg(all(ctx_semantic_fastembed, not(target_os = "macos")))]
pub(super) fn acquire_coreml_backend(
    _cache_dir: &Path,
    _preference: BackendPreference,
    _fallback: Option<&'static str>,
) -> Result<SemanticEmbedder> {
    Err(anyhow!("Core ML semantic embeddings require macOS"))
}

#[cfg(all(ctx_semantic_fastembed, not(target_os = "macos")))]
pub(super) fn acquire_coreml_model_for_daemon(
    _cache_dir: &Path,
) -> Result<SemanticDaemonModelAcquisition> {
    Err(anyhow!("Core ML semantic embeddings require macOS"))
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) fn acquire_coreml_backend(
    cache_dir: &Path,
    preference: BackendPreference,
    fallback: Option<&'static str>,
) -> Result<SemanticEmbedder> {
    let compute = coreml_compute_units_from_env()?;
    if let Some(deferred) = semantic_model_load_deferred(
        SemanticSystemResources::current().available_memory_bytes,
        compute.compute_class,
    ) {
        return Err(deferred.into());
    }
    let model = CoreMlE5Embedder::acquire(cache_dir, None, compute)?;
    let policy = semantic_embed_policy_for(model.compute_class);
    let acquisition_source = model.acquisition_source;
    Ok(SemanticEmbedder {
        batch_size: model.document.batch_size.min(policy.batch_size).max(1),
        backend: SemanticEmbeddingBackend::CoreMl(model),
        preference,
        acquisition_source,
        acquisition_fallback: fallback,
        model_fingerprint: String::new(),
        backend_fingerprint: String::new(),
        canary_passed: false,
    })
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) fn acquire_coreml_model_for_daemon(
    cache_dir: &Path,
) -> Result<SemanticDaemonModelAcquisition> {
    let compute = coreml_compute_units_from_env()?;
    acquire_coreml_model_for_daemon_with(
        SemanticSystemResources::current().available_memory_bytes,
        compute.compute_class,
        || Ok(acquire_coreml_bundle_for_daemon(cache_dir)?.source),
    )
}

#[cfg(all(ctx_semantic_fastembed, any(target_os = "macos", test)))]
pub(super) fn acquire_coreml_model_for_daemon_with<Acquire>(
    available_memory_bytes: Option<u64>,
    compute_class: SemanticComputeClass,
    acquire: Acquire,
) -> Result<SemanticDaemonModelAcquisition>
where
    Acquire: FnOnce() -> Result<CoreMlAcquisitionSource>,
{
    if let Some(deferred) = semantic_model_load_deferred(available_memory_bytes, compute_class) {
        return Err(deferred.into());
    }
    let source = match acquire()? {
        CoreMlAcquisitionSource::Cache => SemanticModelAcquisitionSource::Cache,
        CoreMlAcquisitionSource::Download => SemanticModelAcquisitionSource::Download,
    };
    Ok(SemanticDaemonModelAcquisition::new(
        SemanticModelAcquisitionBackend::CoreMl,
        source,
    ))
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn coreml_fallback_reason(error: &anyhow::Error) -> &'static str {
    match model_acquisition_error_kind(error) {
        Some(ModelAcquisitionErrorKind::Unavailable) if !coreml_descriptor_provisioned() => {
            "descriptor_unprovisioned"
        }
        Some(ModelAcquisitionErrorKind::Unavailable) => "coreml_unavailable",
        Some(ModelAcquisitionErrorKind::Integrity) => "integrity_failure",
        None => "coreml_load_error",
    }
}

#[cfg(all(ctx_semantic_fastembed, any(target_os = "macos", test)))]
pub(super) fn acquire_auto_coreml_backend_with<T, CoreMl, Cpu>(
    acquire_and_authorize_coreml: CoreMl,
    acquire_cpu: Cpu,
) -> Result<T>
where
    CoreMl: FnOnce() -> Result<T>,
    Cpu: FnOnce(&'static str) -> Result<T>,
{
    match acquire_and_authorize_coreml() {
        Ok(backend) => Ok(backend),
        Err(error) if semantic_model_acquisition_integrity_error(&error) => Err(error),
        Err(error) => acquire_cpu(coreml_fallback_reason(&error)),
    }
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn recover_coreml_after_inference_with<T, CoreMl, Cpu>(
    preference: BackendPreference,
    reacquire_coreml: CoreMl,
    acquire_cpu: Cpu,
) -> Result<T>
where
    CoreMl: FnOnce() -> Result<T>,
    Cpu: FnOnce() -> Result<T>,
{
    if preference == BackendPreference::Auto {
        acquire_cpu()
    } else {
        reacquire_coreml()
    }
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) struct CoreMlRoleModel {
    model: coreml_native::Model,
    batch_size: usize,
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) struct CoreMlE5Embedder {
    document: CoreMlRoleModel,
    query: Option<CoreMlRoleModel>,
    tokenizer: tokenizers::Tokenizer,
    sequence_length: usize,
    acquisition_source: &'static str,
    compute_class: SemanticComputeClass,
    compute_mode: &'static str,
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
impl CoreMlE5Embedder {
    pub(super) fn compute_class(&self) -> SemanticComputeClass {
        self.compute_class
    }

    pub(super) fn compute_mode(&self) -> &'static str {
        self.compute_mode
    }

    pub(super) fn acquire(
        cache_dir: &Path,
        acquired: Option<AcquiredCoreMlBundle>,
        compute: CoreMlComputeConfig,
    ) -> Result<Self> {
        let acquired = match acquired {
            Some(acquired) => acquired,
            None => AcquiredCoreMlBundle {
                bundle: cached_coreml_bundle(cache_dir)?.ok_or_else(|| {
                    anyhow!("verified Core ML model bundle is not available in the local cache")
                })?,
                source: CoreMlAcquisitionSource::Cache,
            },
        };
        let bundle = acquired.bundle;
        validate_coreml_bundle_identity(&bundle)?;
        let query_path = bundle.query_model_path();
        let document_model = load_coreml_role_model(
            &bundle.document_model_path(),
            cache_dir,
            &bundle.manifest_sha256,
            "document",
            compute.units,
            bundle.manifest.tensor_contract.document_batch_size as usize,
            bundle.manifest.tensor_contract.max_sequence_length as usize,
        )?;
        let query_batch_size = bundle.manifest.tensor_contract.query_batch_size;
        let query_model = query_path
            .map(|path| {
                let expected_batch_size = query_batch_size.ok_or_else(|| {
                    anyhow!("Core ML query model has no signed query batch contract")
                })? as usize;
                load_coreml_role_model(
                    &path,
                    cache_dir,
                    &bundle.manifest_sha256,
                    "query",
                    compute.units,
                    expected_batch_size,
                    bundle.manifest.tensor_contract.max_sequence_length as usize,
                )
            })
            .transpose()?;
        let sequence_length = bundle.manifest.tensor_contract.max_sequence_length as usize;
        let tokenizer = load_coreml_tokenizer(&bundle.tokenizer_path(), sequence_length)?;
        Ok(Self {
            document: document_model,
            query: query_model,
            tokenizer,
            sequence_length,
            acquisition_source: match acquired.source {
                CoreMlAcquisitionSource::Cache => "cache",
                CoreMlAcquisitionSource::Download => "download",
            },
            compute_class: compute.compute_class,
            compute_mode: compute.mode,
        })
    }

    pub(super) fn embed_query(&self, query: String) -> Result<Vec<f32>> {
        let model = self.query.as_ref().unwrap_or(&self.document);
        let mut embeddings = self.embed_role(model, vec![query], SEMANTIC_QUERY_PREFIX)?;
        embeddings
            .pop()
            .ok_or_else(|| anyhow!("native Core ML query embedding was empty"))
    }

    pub(super) fn embed_documents(&self, documents: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.embed_role(&self.document, documents, SEMANTIC_PASSAGE_PREFIX)
    }

    pub(super) fn embed_role(
        &self,
        role_model: &CoreMlRoleModel,
        texts: Vec<String>,
        padding_text: &str,
    ) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let original_len = texts.len();
        let texts = pad_role_batch(texts, role_model.batch_size, padding_text)?;
        let mut embeddings = Vec::with_capacity(texts.len());
        for batch in texts.chunks(role_model.batch_size) {
            embeddings.extend(self.embed_batch(&role_model.model, batch)?);
        }
        embeddings.truncate(original_len);
        Ok(embeddings)
    }

    pub(super) fn embed_batch(
        &self,
        model: &coreml_native::Model,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>> {
        use coreml_native::{AsMultiArray, BorrowedTensor};

        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|error| anyhow!("tokenize native Core ML batch: {error}"))?;
        let batch_size = encodings.len();
        let element_count = batch_size.saturating_mul(self.sequence_length);
        let mut input_ids = Vec::with_capacity(element_count);
        let mut attention_mask = Vec::with_capacity(element_count);
        let mut token_type_ids = Vec::with_capacity(element_count);
        for encoding in encodings {
            if encoding.len() != self.sequence_length {
                return Err(anyhow!(
                    "native Core ML tokenizer returned sequence length {}, expected {}",
                    encoding.len(),
                    self.sequence_length
                ));
            }
            input_ids.extend(encoding.get_ids().iter().map(|value| *value as i32));
            attention_mask.extend(
                encoding
                    .get_attention_mask()
                    .iter()
                    .map(|value| *value as i32),
            );
            token_type_ids.extend(encoding.get_type_ids().iter().map(|value| *value as i32));
        }
        let shape = [batch_size, self.sequence_length];
        let input_ids = BorrowedTensor::from_i32(&input_ids, &shape)
            .map_err(|error| anyhow!("create Core ML input_ids: {error}"))?;
        let attention_mask = BorrowedTensor::from_i32(&attention_mask, &shape)
            .map_err(|error| anyhow!("create Core ML attention_mask: {error}"))?;
        let token_type_ids = BorrowedTensor::from_i32(&token_type_ids, &shape)
            .map_err(|error| anyhow!("create Core ML token_type_ids: {error}"))?;
        let inputs: [(&str, &dyn AsMultiArray); 3] = [
            ("input_ids", &input_ids),
            ("attention_mask", &attention_mask),
            ("token_type_ids", &token_type_ids),
        ];
        let prediction = model
            .predict(&inputs)
            .map_err(|error| anyhow!("run native Core ML embedding: {error}"))?;
        let (values, output_shape) = prediction
            .get_f32("sentence_embeddings")
            .map_err(|error| anyhow!("read native Core ML embedding: {error}"))?;
        if output_shape != [batch_size, SEMANTIC_DIMENSIONS] {
            return Err(anyhow!(
                "native Core ML output shape is {output_shape:?}, expected [{batch_size}, {SEMANTIC_DIMENSIONS}]"
            ));
        }
        Ok(values
            .chunks_exact(SEMANTIC_DIMENSIONS)
            .map(<[f32]>::to_vec)
            .collect())
    }
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) fn validate_coreml_bundle_identity(bundle: &VerifiedModelBundle) -> Result<()> {
    if bundle.manifest.model.embedding_space_id != semantic_model_key() {
        return Err(anyhow!(
            "Core ML bundle embedding space {:?} does not match required {:?}",
            bundle.manifest.model.embedding_space_id,
            semantic_model_key()
        ));
    }
    if bundle.manifest.model.id != SEMANTIC_MODEL_ID
        || bundle.manifest.model.source_revision != SEMANTIC_MODEL_REVISION
    {
        return Err(anyhow!(
            "Core ML bundle does not match the required {SEMANTIC_MODEL_ID} revision"
        ));
    }
    Ok(())
}

#[cfg(any(all(ctx_semantic_fastembed, target_os = "macos"), test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CoreMlComputeMode {
    All,
    CpuAndNeuralEngine,
    CpuAndGpu,
    CpuOnly,
}

#[cfg(any(all(ctx_semantic_fastembed, target_os = "macos"), test))]
impl CoreMlComputeMode {
    pub(super) fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "ane" | "cpu-ane" => Ok(Self::CpuAndNeuralEngine),
            "gpu" | "cpu-gpu" => Ok(Self::CpuAndGpu),
            "cpu" => Ok(Self::CpuOnly),
            value => Err(anyhow!(
                "unsupported CTX_SEMANTIC_COREML_NATIVE_COMPUTE mode {value:?}"
            )),
        }
    }

    pub(super) fn compute_class(self) -> SemanticComputeClass {
        match self {
            Self::CpuOnly => SemanticComputeClass::Cpu,
            Self::All | Self::CpuAndNeuralEngine | Self::CpuAndGpu => {
                SemanticComputeClass::Accelerator
            }
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::CpuAndNeuralEngine => "cpu_and_neural_engine",
            Self::CpuAndGpu => "cpu_and_gpu",
            Self::CpuOnly => "cpu_only",
        }
    }
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
#[derive(Clone, Copy)]
pub(super) struct CoreMlComputeConfig {
    units: coreml_native::ComputeUnits,
    compute_class: SemanticComputeClass,
    mode: &'static str,
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) fn coreml_compute_units_from_env() -> Result<CoreMlComputeConfig> {
    use coreml_native::ComputeUnits;

    let mode = CoreMlComputeMode::parse(
        &env::var("CTX_SEMANTIC_COREML_NATIVE_COMPUTE").unwrap_or_else(|_| "all".to_owned()),
    )?;
    let units = match mode {
        CoreMlComputeMode::All => ComputeUnits::All,
        CoreMlComputeMode::CpuAndNeuralEngine => ComputeUnits::CpuAndNeuralEngine,
        CoreMlComputeMode::CpuAndGpu => ComputeUnits::CpuAndGpu,
        CoreMlComputeMode::CpuOnly => ComputeUnits::CpuOnly,
    };
    Ok(CoreMlComputeConfig {
        units,
        compute_class: mode.compute_class(),
        mode: mode.as_str(),
    })
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) fn load_coreml_role_model(
    source: &Path,
    cache_dir: &Path,
    manifest_sha256: &str,
    role: &str,
    compute_units: coreml_native::ComputeUnits,
    expected_batch_size: usize,
    expected_sequence_length: usize,
) -> Result<CoreMlRoleModel> {
    let (load_path, reused_cache) =
        compiled_coreml_model_path(source, cache_dir, manifest_sha256, role)?;
    match load_and_validate_coreml_role_model(
        &load_path,
        role,
        compute_units,
        expected_batch_size,
        expected_sequence_length,
    ) {
        Ok(model) => Ok(model),
        Err(first_error) if reused_cache => {
            invalidate_compiled_model_cache(&load_path)
                .with_context(|| format!("invalidate corrupt Core ML {role} compiled cache"))?;
            let (rebuilt_path, reused_after_invalidation) =
                compiled_coreml_model_path(source, cache_dir, manifest_sha256, role)?;
            if reused_after_invalidation {
                return Err(anyhow!(
                    "Core ML {role} compiled cache remained present after invalidation"
                ));
            }
            load_and_validate_coreml_role_model(
                &rebuilt_path,
                role,
                compute_units,
                expected_batch_size,
                expected_sequence_length,
            )
            .with_context(|| {
                format!(
                    "rebuild Core ML {role} compiled cache after load failure; first failure: {first_error:#}"
                )
            })
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) fn load_and_validate_coreml_role_model(
    path: &Path,
    role: &str,
    compute_units: coreml_native::ComputeUnits,
    expected_batch_size: usize,
    expected_sequence_length: usize,
) -> Result<CoreMlRoleModel> {
    let model = coreml_native::Model::load(path, compute_units)
        .map_err(|error| anyhow!("load native Core ML {role} model: {error}"))?;
    let batch_size = validate_coreml_model_contract(
        &model,
        role,
        expected_batch_size,
        expected_sequence_length,
    )?;
    Ok(CoreMlRoleModel { model, batch_size })
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) fn compiled_coreml_model_path(
    source: &Path,
    cache_dir: &Path,
    manifest_sha256: &str,
    role: &str,
) -> Result<(PathBuf, bool)> {
    const COMPILER_IDENTITY: &str = "coreml-native-0.2.0:MLModel.compileModelAtURL";

    if source.extension().and_then(|value| value.to_str()) != Some("mlpackage") {
        return Err(anyhow!(
            "verified Core ML {role} artifact must be an mlpackage"
        ));
    }
    create_private_dir_all(cache_dir)?;
    let destination =
        prepare_compile_destination(cache_dir, manifest_sha256, role, COMPILER_IDENTITY)?;
    if destination.final_path.is_dir() {
        discard_compile_destination(&destination)?;
        return Ok((destination.final_path, true));
    }

    let temporary = coreml_native::compile_model(source)
        .map_err(|error| anyhow!("compile native Core ML {role} model: {error}"))?;
    let result = (|| -> Result<()> {
        copy_directory_contents(&temporary, &destination.staging_path)?;
        commit_compile_destination(&destination)?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&temporary);
    if let Err(error) = result {
        let _ = discard_compile_destination(&destination);
        return Err(error);
    }
    Ok((destination.final_path, false))
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) fn copy_directory_contents(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)
        .with_context(|| format!("read compiled Core ML directory {}", source.display()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            fs::create_dir(&destination_path)?;
            copy_directory_contents(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path)?;
        } else {
            return Err(anyhow!(
                "compiled Core ML model contains unsupported entry {}",
                source_path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) fn validate_coreml_model_contract(
    model: &coreml_native::Model,
    role: &str,
    expected_batch_size: usize,
    expected_sequence_length: usize,
) -> Result<usize> {
    use coreml_native::{DataType, FeatureType};

    let inputs = model.inputs();
    let mut batch_size = None;
    for name in ["input_ids", "attention_mask", "token_type_ids"] {
        let input = inputs
            .iter()
            .find(|input| input.name() == name)
            .ok_or_else(|| anyhow!("Core ML {role} model is missing input {name}"))?;
        if input.feature_type() != &FeatureType::MultiArray
            || input.data_type() != Some(DataType::Int32)
        {
            return Err(anyhow!(
                "Core ML {role} input {name} has an incompatible type"
            ));
        }
        let shape = input
            .shape()
            .ok_or_else(|| anyhow!("Core ML {role} input {name} has no fixed shape"))?;
        if shape.len() != 2
            || shape[0] != expected_batch_size
            || shape[1] != expected_sequence_length
        {
            return Err(anyhow!(
                "Core ML {role} input {name} shape {shape:?} is incompatible with signed contract [{expected_batch_size}, {expected_sequence_length}]"
            ));
        }
        if batch_size
            .replace(shape[0])
            .is_some_and(|batch| batch != shape[0])
        {
            return Err(anyhow!(
                "Core ML {role} inputs do not share one fixed batch size"
            ));
        }
    }
    if inputs.len() != 3 {
        return Err(anyhow!(
            "Core ML {role} model must expose exactly three inputs"
        ));
    }
    let batch_size = batch_size.ok_or_else(|| anyhow!("Core ML {role} batch size is missing"))?;
    let outputs = model.outputs();
    let output = outputs
        .iter()
        .find(|output| output.name() == "sentence_embeddings")
        .ok_or_else(|| anyhow!("Core ML {role} model is missing sentence_embeddings output"))?;
    if outputs.len() != 1
        || output.feature_type() != &FeatureType::MultiArray
        || output.data_type() != Some(DataType::Float32)
        || output.shape() != Some([batch_size, SEMANTIC_DIMENSIONS].as_slice())
    {
        return Err(anyhow!(
            "Core ML {role} sentence_embeddings output contract is incompatible"
        ));
    }
    Ok(batch_size)
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
pub(super) fn load_coreml_tokenizer(
    path: &Path,
    sequence_length: usize,
) -> Result<tokenizers::Tokenizer> {
    use tokenizers::{PaddingParams, PaddingStrategy, TruncationParams};

    const E5_PAD_TOKEN: &str = "<pad>";
    const E5_PAD_ID: u32 = 1;

    let mut tokenizer = tokenizers::Tokenizer::from_file(path)
        .map_err(|error| anyhow!("load Core ML tokenizer {}: {error}", path.display()))?;
    let pad_id = tokenizer
        .token_to_id(E5_PAD_TOKEN)
        .ok_or_else(|| anyhow!("Core ML tokenizer does not define {E5_PAD_TOKEN}"))?;
    if pad_id != E5_PAD_ID {
        return Err(anyhow!(
            "Core ML tokenizer {E5_PAD_TOKEN} id {pad_id} does not match pinned id {E5_PAD_ID}"
        ));
    }
    tokenizer.with_padding(Some(PaddingParams {
        strategy: PaddingStrategy::Fixed(sequence_length),
        pad_id,
        pad_token: E5_PAD_TOKEN.to_owned(),
        ..Default::default()
    }));
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length: sequence_length,
            ..Default::default()
        }))
        .map_err(|error| anyhow!("configure Core ML tokenizer truncation: {error}"))?;
    Ok(tokenizer)
}

#[cfg(any(all(ctx_semantic_fastembed, target_os = "macos"), test))]
pub(super) fn pad_role_batch(
    mut texts: Vec<String>,
    exact_batch_size: usize,
    padding_text: &str,
) -> Result<Vec<String>> {
    if exact_batch_size == 0 {
        return Err(anyhow!("semantic fixed batch must be positive"));
    }
    let remainder = texts.len() % exact_batch_size;
    if remainder != 0 {
        texts.extend(std::iter::repeat_n(
            padding_text.to_owned(),
            exact_batch_size - remainder,
        ));
    }
    Ok(texts)
}

#[cfg(all(test, ctx_semantic_fastembed))]
pub(in crate::semantic) fn pad_texts_to_exact_batch(
    texts: Vec<String>,
    exact_batch_size: usize,
) -> Result<Vec<String>> {
    pad_role_batch(texts, exact_batch_size, SEMANTIC_PASSAGE_PREFIX)
}

#[cfg(all(test, ctx_semantic_fastembed))]
pub(in crate::semantic) fn semantic_fixed_shape_from_values(
    batch_size: Option<&str>,
    sequence_length: Option<&str>,
) -> Result<Option<(usize, usize)>> {
    match (batch_size, sequence_length) {
        (None, None) => Ok(None),
        (Some(batch_size), Some(sequence_length)) => {
            let batch_size = batch_size
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|value| *value > 0);
            let sequence_length = sequence_length
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|value| *value > 0);
            match (batch_size, sequence_length) {
                (Some(batch_size), Some(sequence_length)) => {
                    Ok(Some((batch_size, sequence_length)))
                }
                _ => Err(anyhow!("fixed shape values must be positive integers")),
            }
        }
        _ => Err(anyhow!("fixed shape values must be provided together")),
    }
}
