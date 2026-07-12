const SEMANTIC_BACKEND_PREFERENCE_ENV: &str = "CTX_INTERNAL_SEMANTIC_BACKEND";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SemanticModelAccess {
    ForegroundCacheOnly,
    DaemonNetwork,
}

impl SemanticModelAccess {
    fn network_allowed(self) -> bool {
        self == Self::DaemonNetwork
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendPreference {
    Auto,
    Cpu,
    CoreMl,
}

impl BackendPreference {
    fn from_env() -> Result<Self> {
        Self::parse(env::var(SEMANTIC_BACKEND_PREFERENCE_ENV).ok().as_deref())
    }

    fn parse(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("auto") => Ok(Self::Auto),
            Some("cpu") => Ok(Self::Cpu),
            Some("coreml") => Ok(Self::CoreMl),
            Some(value) => Err(anyhow!(
                "unsupported {SEMANTIC_BACKEND_PREFERENCE_ENV} value {value:?}; expected auto, cpu, or coreml"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::CoreMl => "coreml",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SemanticEmbeddingRuntimeInfo {
    preference: BackendPreference,
    backend: &'static str,
    compute_class: SemanticComputeClass,
    compute_mode: Option<&'static str>,
    acquisition_source: &'static str,
    acquisition_fallback: Option<&'static str>,
}

impl SemanticEmbeddingRuntimeInfo {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "preference": self.preference.as_str(),
            "backend": self.backend,
            "compute_class": self.compute_class.as_str(),
            "compute_mode": self.compute_mode,
            "model_id": SEMANTIC_MODEL_ID,
            "model_key": semantic_model_key(),
            "dimensions": SEMANTIC_DIMENSIONS,
            "acquisition_source": self.acquisition_source,
            "acquisition_fallback": self.acquisition_fallback,
        }))
    }
}

#[cfg(ctx_semantic_fastembed)]
enum SemanticEmbeddingBackend {
    Cpu(fastembed::TextEmbedding),
    #[cfg(target_os = "macos")]
    CoreMl(CoreMlE5Embedder),
}

#[cfg(ctx_semantic_fastembed)]
impl SemanticEmbeddingBackend {
    fn embed_query(&mut self, query: String) -> Result<Vec<f32>> {
        let query = semantic_e5_query_text_value(&query);
        let raw = match self {
            Self::Cpu(model) => model
                .embed(vec![query], Some(1))
                .with_context(|| format!("embed query with semantic model {SEMANTIC_MODEL_ID}"))?,
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => vec![model.embed_query(query)?],
        };
        let mut embeddings = normalize_and_validate_embeddings(raw, 1)?;
        embeddings
            .pop()
            .ok_or_else(|| anyhow!("semantic query embedding was empty"))
    }

    fn embed_documents(
        &mut self,
        documents: Vec<String>,
        batch_size: usize,
    ) -> Result<Vec<Vec<f32>>> {
        let expected = documents.len();
        if expected == 0 {
            return Ok(Vec::new());
        }
        let documents = documents
            .into_iter()
            .map(|text| semantic_e5_passage_text(&text))
            .collect::<Vec<_>>();
        let raw = match self {
            Self::Cpu(model) => model.embed(documents, Some(batch_size)).with_context(|| {
                format!("embed documents with semantic model {SEMANTIC_MODEL_ID}")
            })?,
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => model.embed_documents(documents)?,
        };
        normalize_and_validate_embeddings(raw, expected)
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Cpu(_) => "cpu",
            #[cfg(target_os = "macos")]
            Self::CoreMl(_) => "coreml",
        }
    }

    fn compute_class(&self) -> SemanticComputeClass {
        match self {
            Self::Cpu(_) => SemanticComputeClass::Cpu,
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => model.compute_class,
        }
    }

    fn compute_mode(&self) -> Option<&'static str> {
        match self {
            Self::Cpu(_) => None,
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => Some(model.compute_mode),
        }
    }
}

#[cfg(ctx_semantic_fastembed)]
struct SemanticEmbedder {
    backend: SemanticEmbeddingBackend,
    batch_size: usize,
    policy: SemanticEmbedPolicy,
    preference: BackendPreference,
    acquisition_source: &'static str,
    acquisition_fallback: Option<&'static str>,
}

#[cfg(ctx_semantic_fastembed)]
impl SemanticEmbedder {
    fn embed_query(&mut self, query: String) -> Result<Vec<f32>> {
        self.backend.embed_query(query)
    }

    fn embed_documents(&mut self, documents: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.backend.embed_documents(documents, self.batch_size)
    }

    fn runtime_info(&self) -> SemanticEmbeddingRuntimeInfo {
        SemanticEmbeddingRuntimeInfo {
            preference: self.preference,
            backend: self.backend.name(),
            compute_class: self.backend.compute_class(),
            compute_mode: self.backend.compute_mode(),
            acquisition_source: self.acquisition_source,
            acquisition_fallback: self.acquisition_fallback,
        }
    }

    fn quiet_policy(&self) -> SemanticQuietPolicy {
        semantic_quiet_policy(
            SemanticSystemResources::current(),
            self.backend.compute_class(),
        )
    }
}

#[cfg(ctx_semantic_fastembed)]
fn normalize_and_validate_embeddings(
    mut embeddings: Vec<Vec<f32>>,
    expected_count: usize,
) -> Result<Vec<Vec<f32>>> {
    if embeddings.len() != expected_count {
        return Err(anyhow!(
            "semantic model returned {} embeddings, expected {expected_count}",
            embeddings.len()
        ));
    }
    for embedding in &mut embeddings {
        if embedding.len() != SEMANTIC_DIMENSIONS {
            return Err(anyhow!(
                "semantic model returned {} dimensions, expected {}",
                embedding.len(),
                SEMANTIC_DIMENSIONS
            ));
        }
        if embedding.iter().any(|value| !value.is_finite()) {
            return Err(anyhow!(
                "semantic model returned a non-finite embedding value"
            ));
        }
        let norm = embedding
            .iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>()
            .sqrt();
        if !norm.is_finite() || norm <= f64::EPSILON {
            return Err(anyhow!("semantic model returned a zero-norm embedding"));
        }
        for value in embedding {
            *value = (f64::from(*value) / norm) as f32;
        }
    }
    Ok(embeddings)
}

#[cfg(ctx_semantic_fastembed)]
fn new_semantic_embedder(cache_dir: &Path) -> Result<SemanticEmbedder> {
    acquire_semantic_embedder_with_mode(cache_dir, SemanticModelAccess::ForegroundCacheOnly)
}

#[cfg(ctx_semantic_fastembed)]
fn acquire_semantic_embedder(cache_dir: &Path) -> Result<SemanticEmbedder> {
    acquire_semantic_embedder_with_mode(cache_dir, SemanticModelAccess::DaemonNetwork)
}

#[cfg(ctx_semantic_fastembed)]
fn acquire_semantic_embedder_with_mode(
    cache_dir: &Path,
    access: SemanticModelAccess,
) -> Result<SemanticEmbedder> {
    let preference = BackendPreference::from_env()?;
    match preference {
        BackendPreference::Cpu => acquire_cpu_backend(
            cache_dir,
            semantic_embed_policy_for(SemanticComputeClass::Cpu),
            preference,
            access.network_allowed(),
        ),
        BackendPreference::CoreMl => {
            acquire_coreml_backend(cache_dir, preference, None, access.network_allowed())
        }
        BackendPreference::Auto => {
            #[cfg(target_os = "macos")]
            match acquire_coreml_backend(cache_dir, preference, None, access.network_allowed()) {
                Ok(embedder) => return Ok(embedder),
                Err(error) if model_acquisition_integrity_error(&error) => return Err(error),
                Err(error) => {
                    let fallback = coreml_fallback_reason(&error);
                    return acquire_cpu_backend(
                        cache_dir,
                        semantic_embed_policy_for(SemanticComputeClass::Cpu),
                        preference,
                        access.network_allowed(),
                    )
                    .map(|mut embedder| {
                        embedder.acquisition_fallback = Some(fallback);
                        embedder
                    });
                }
            }
            #[cfg(not(target_os = "macos"))]
            acquire_cpu_backend(
                cache_dir,
                semantic_embed_policy_for(SemanticComputeClass::Cpu),
                preference,
                access.network_allowed(),
            )
        }
    }
}

#[cfg(ctx_semantic_fastembed)]
fn reacquire_semantic_embedder(
    cache_dir: &Path,
    runtime: &SemanticEmbeddingRuntimeInfo,
) -> Result<SemanticEmbedder> {
    match runtime.backend {
        "cpu" => acquire_cpu_backend(
            cache_dir,
            semantic_embed_policy_for(SemanticComputeClass::Cpu),
            runtime.preference,
            false,
        )
        .map(|mut embedder| {
            embedder.acquisition_fallback = runtime.acquisition_fallback;
            embedder
        }),
        "coreml" => acquire_coreml_backend(
            cache_dir,
            runtime.preference,
            runtime.acquisition_fallback,
            false,
        ),
        backend => Err(anyhow!(
            "cannot reacquire unsupported semantic backend {backend:?}"
        )),
    }
}

#[cfg(ctx_semantic_fastembed)]
fn acquire_cpu_backend(
    cache_dir: &Path,
    policy: SemanticEmbedPolicy,
    preference: BackendPreference,
    allow_download: bool,
) -> Result<SemanticEmbedder> {
    if let Some(deferred) = semantic_cpu_model_load_deferred(policy.available_memory_bytes) {
        return Err(deferred.into());
    }
    if env::var_os("CTX_SEMANTIC_MODEL_ONNX").is_some() {
        return Err(anyhow!(
            "CTX_SEMANTIC_MODEL_ONNX is no longer accepted; CPU embeddings use the verified {SEMANTIC_MODEL_ID} cache"
        ));
    }
    let (snapshot, downloaded) = match semantic_cpu_cache_snapshot(cache_dir) {
        Ok(snapshot) => (snapshot, false),
        Err(error) if allow_download && semantic_cpu_cache_repairable(&error) => (
            replace_cpu_model_cache_from_pinned_revision(cache_dir)?,
            true,
        ),
        Err(error) => return Err(error),
    };
    let model = load_cached_cpu_model(&snapshot, cache_dir, &policy)?;
    Ok(SemanticEmbedder {
        backend: SemanticEmbeddingBackend::Cpu(model),
        batch_size: policy.batch_size,
        policy,
        preference,
        acquisition_source: if downloaded { "download" } else { "cache" },
        acquisition_fallback: None,
    })
}

#[cfg(ctx_semantic_fastembed)]
fn load_cached_cpu_model(
    snapshot: &Path,
    cache_dir: &Path,
    policy: &SemanticEmbedPolicy,
) -> Result<fastembed::TextEmbedding> {
    use fastembed::{
        EmbeddingModel, InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles,
        UserDefinedEmbeddingModel,
    };

    let _runtime = ensure_semantic_onnxruntime_loaded(cache_dir)?;
    let model_info = TextEmbedding::get_model_info(&EmbeddingModel::MultilingualE5Small)?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: read_semantic_model_file(snapshot, "tokenizer.json")?,
        config_file: read_semantic_model_file(snapshot, "config.json")?,
        special_tokens_map_file: read_semantic_model_file(snapshot, "special_tokens_map.json")?,
        tokenizer_config_file: read_semantic_model_file(snapshot, "tokenizer_config.json")?,
    };
    let model_path = snapshot.join(&model_info.model_file);
    let mut user_model = UserDefinedEmbeddingModel::new(
        fs::read(&model_path)
            .with_context(|| format!("read semantic model file {}", model_path.display()))?,
        tokenizer_files,
    )
    .with_pooling(
        TextEmbedding::get_default_pooling_method(&EmbeddingModel::MultilingualE5Small)
            .unwrap_or(Pooling::Mean),
    )
    .with_quantization(TextEmbedding::get_quantization_mode(
        &EmbeddingModel::MultilingualE5Small,
    ));
    user_model.output_key = model_info.output_key.clone();
    TextEmbedding::try_new_from_user_defined(
        user_model,
        InitOptionsUserDefined::new().with_intra_threads(policy.threads),
    )
    .with_context(|| format!("initialize semantic embedding model {SEMANTIC_MODEL_ID}"))
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_cpu_cache_snapshot(cache_dir: &Path) -> Result<PathBuf> {
    let mut repairable_error = None;
    for model_root in semantic_model_cache_roots(cache_dir) {
        let snapshot = model_root.join("snapshots").join(SEMANTIC_MODEL_REVISION);
        match fs::metadata(&snapshot) {
            Ok(metadata) if metadata.is_dir() => match verify_semantic_cpu_snapshot(&snapshot) {
                Ok(()) => return Ok(snapshot),
                Err(error) if semantic_cpu_cache_repairable(&error) => {
                    repairable_error.get_or_insert(error);
                }
                Err(error) => return Err(error),
            },
            Ok(_) => {
                repairable_error.get_or_insert_with(|| {
                    SemanticCpuModelIntegrityError(format!(
                        "semantic CPU model snapshot {} is not a directory",
                        snapshot.display()
                    ))
                    .into()
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect semantic model cache {}", snapshot.display())
                });
            }
        }
    }
    Err(repairable_error.unwrap_or_else(|| {
        SemanticCpuModelCacheMissing(format!(
            "semantic model cache is incomplete at {}",
            cache_dir.display()
        ))
        .into()
    }))
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_cpu_cache_repairable(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<SemanticCpuModelCacheMissing>()
        .is_some()
        || error
            .downcast_ref::<SemanticCpuModelIntegrityError>()
            .is_some()
}

#[cfg(ctx_semantic_fastembed)]
fn verify_semantic_cpu_snapshot(snapshot: &Path) -> Result<()> {
    for expected in SEMANTIC_REQUIRED_MODEL_FILES {
        verify_semantic_cpu_file(&snapshot.join(expected.path), *expected)?;
    }
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
fn verify_semantic_cpu_file(path: &Path, expected: SemanticModelFile) -> Result<()> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SemanticCpuModelCacheMissing(format!(
                "semantic CPU model file {} is missing",
                path.display()
            ))
            .into());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect semantic CPU model file {}", path.display()));
        }
    };
    if !metadata.is_file() || metadata.len() != expected.size {
        return Err(SemanticCpuModelIntegrityError(format!(
            "semantic CPU model file {} has size {}, expected {}",
            path.display(),
            metadata.len(),
            expected.size
        ))
        .into());
    }
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SemanticCpuModelCacheMissing(format!(
                "semantic CPU model file {} disappeared during verification",
                path.display()
            ))
            .into());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("open semantic CPU model file {}", path.display()));
        }
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("read semantic CPU model file {}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected.sha256 {
        return Err(SemanticCpuModelIntegrityError(format!(
            "semantic CPU model file {} has SHA-256 {actual}, expected {}",
            path.display(),
            expected.sha256
        ))
        .into());
    }
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
fn replace_cpu_model_cache_from_pinned_revision(cache_dir: &Path) -> Result<PathBuf> {
    use hf_hub::{api::sync::ApiBuilder, Repo, RepoType};

    let managed_root = cache_dir.join(SEMANTIC_MANAGED_MODEL_CACHE_DIR);
    fs::create_dir_all(&managed_root)
        .with_context(|| format!("create semantic model cache {}", managed_root.display()))?;
    let _lock = lock_semantic_model_acquisition(&managed_root)?;

    match semantic_cpu_cache_snapshot(cache_dir) {
        Ok(snapshot) => return Ok(snapshot),
        Err(error) if semantic_cpu_cache_repairable(&error) => {}
        Err(error) => return Err(error),
    }

    let download_cache = managed_root.join("download-cache");
    let model_root = managed_root.join(SEMANTIC_HF_MODEL_CACHE_DIR);
    let mut verified_staging_root = None;
    for attempt in 0..2 {
        if attempt > 0 && download_cache.exists() {
            fs::remove_dir_all(&download_cache).with_context(|| {
                format!(
                    "discard corrupt ctx-managed model download cache {}",
                    download_cache.display()
                )
            })?;
        }
        fs::create_dir_all(&download_cache).with_context(|| {
            format!(
                "create semantic model download cache {}",
                download_cache.display()
            )
        })?;
        let api = ApiBuilder::new()
            .with_cache_dir(download_cache.clone())
            .with_progress(false)
            .build()
            .context("initialize pinned semantic model downloader")?;
        let repo = api.repo(Repo::with_revision(
            SEMANTIC_MODEL_ID.to_owned(),
            RepoType::Model,
            SEMANTIC_MODEL_REVISION.to_owned(),
        ));
        let staging_root = managed_root.join(format!(
            ".{SEMANTIC_HF_MODEL_CACHE_DIR}.staging-{}",
            Uuid::new_v4().simple()
        ));
        let staging_snapshot = staging_root.join("snapshots").join(SEMANTIC_MODEL_REVISION);
        let staged = (|| -> Result<()> {
            for expected in SEMANTIC_REQUIRED_MODEL_FILES {
                let downloaded = repo.download(expected.path).with_context(|| {
                    format!(
                        "download {SEMANTIC_MODEL_ID}@{SEMANTIC_MODEL_REVISION}/{}",
                        expected.path
                    )
                })?;
                let destination = staging_snapshot.join(expected.path);
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).with_context(|| {
                        format!(
                            "create semantic model staging directory {}",
                            parent.display()
                        )
                    })?;
                }
                fs::copy(&downloaded, &destination).with_context(|| {
                    format!(
                        "stage semantic model file {} at {}",
                        downloaded.display(),
                        destination.display()
                    )
                })?;
            }
            verify_semantic_cpu_snapshot(&staging_snapshot).with_context(|| {
                format!(
                    "downloaded semantic CPU model failed verification in {}",
                    staging_snapshot.display()
                )
            })
        })();
        match staged {
            Ok(()) => {
                verified_staging_root = Some(staging_root);
                break;
            }
            Err(error) if attempt == 0 && semantic_cpu_cache_repairable(&error) => {
                let _ = fs::remove_dir_all(&staging_root);
            }
            Err(error) => {
                let _ = fs::remove_dir_all(&staging_root);
                return Err(error);
            }
        }
    }
    let staging_root = verified_staging_root.ok_or_else(|| {
        anyhow!("semantic CPU model download did not produce a verified snapshot")
    })?;

    if let Err(error) = publish_semantic_cpu_model_root(&staging_root, &model_root) {
        let _ = fs::remove_dir_all(&staging_root);
        return Err(error);
    }
    Ok(model_root.join("snapshots").join(SEMANTIC_MODEL_REVISION))
}

#[cfg(ctx_semantic_fastembed)]
fn lock_semantic_model_acquisition(managed_root: &Path) -> Result<fs::File> {
    use fs2::FileExt;

    fs::create_dir_all(managed_root)
        .with_context(|| format!("create semantic model cache {}", managed_root.display()))?;
    let lock_path = managed_root.join("acquisition.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| {
            format!(
                "open semantic model acquisition lock {}",
                lock_path.display()
            )
        })?;
    lock.lock_exclusive()
        .with_context(|| format!("lock semantic model acquisition {}", lock_path.display()))?;
    Ok(lock)
}

#[cfg(ctx_semantic_fastembed)]
fn publish_semantic_cpu_model_root(staging_root: &Path, model_root: &Path) -> Result<()> {
    let managed_root = model_root
        .parent()
        .ok_or_else(|| anyhow!("semantic model root has no parent"))?;
    let backup_root = managed_root.join(format!(
        ".{SEMANTIC_HF_MODEL_CACHE_DIR}.backup-{}",
        Uuid::new_v4().simple()
    ));
    let had_previous = model_root.exists();
    if had_previous {
        fs::rename(model_root, &backup_root).with_context(|| {
            format!(
                "preserve previous semantic model cache {}",
                model_root.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(staging_root, model_root) {
        let restore = if had_previous {
            fs::rename(&backup_root, model_root).err()
        } else {
            None
        };
        return Err(anyhow!(match restore {
            Some(restore) => format!(
                "publish semantic model cache {}: {error}; restore previous cache: {restore}",
                model_root.display()
            ),
            None => format!(
                "publish semantic model cache {}: {error}",
                model_root.display()
            ),
        }));
    }
    if had_previous {
        // Publication is already committed. A cleanup failure must not turn a
        // valid model into a retry loop; a later acquisition may remove it.
        let _ = fs::remove_dir_all(&backup_root);
    }
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
fn read_semantic_model_file(snapshot: &Path, relative: &str) -> Result<Vec<u8>> {
    let path = snapshot.join(relative);
    fs::read(&path).with_context(|| format!("read semantic model file {}", path.display()))
}

#[cfg(all(ctx_semantic_fastembed, not(target_os = "macos")))]
fn acquire_coreml_backend(
    _cache_dir: &Path,
    _preference: BackendPreference,
    _fallback: Option<&'static str>,
    _allow_download: bool,
) -> Result<SemanticEmbedder> {
    Err(anyhow!("Core ML semantic embeddings require macOS"))
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
fn acquire_coreml_backend(
    cache_dir: &Path,
    preference: BackendPreference,
    fallback: Option<&'static str>,
    allow_download: bool,
) -> Result<SemanticEmbedder> {
    let compute = coreml_compute_units_from_env()?;
    if let Some(deferred) = semantic_model_load_deferred(
        SemanticSystemResources::current().available_memory_bytes,
        compute.compute_class,
    ) {
        return Err(deferred.into());
    }
    let acquired = if allow_download {
        Some(acquire_coreml_bundle_for_daemon(cache_dir)?)
    } else {
        None
    };
    let model = CoreMlE5Embedder::acquire(cache_dir, acquired, compute)?;
    let policy = semantic_embed_policy_for(model.compute_class);
    let acquisition_source = model.acquisition_source;
    Ok(SemanticEmbedder {
        batch_size: model.document.batch_size.min(policy.batch_size).max(1),
        backend: SemanticEmbeddingBackend::CoreMl(model),
        policy,
        preference,
        acquisition_source,
        acquisition_fallback: fallback,
    })
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
fn coreml_fallback_reason(error: &anyhow::Error) -> &'static str {
    match model_acquisition_error_kind(error) {
        Some(ModelAcquisitionErrorKind::Unavailable) if !coreml_descriptor_provisioned() => {
            "descriptor_unprovisioned"
        }
        Some(ModelAcquisitionErrorKind::Unavailable) => "coreml_unavailable",
        Some(ModelAcquisitionErrorKind::Integrity) => "integrity_failure",
        None => "coreml_load_error",
    }
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
struct CoreMlRoleModel {
    model: coreml_native::Model,
    batch_size: usize,
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
struct CoreMlE5Embedder {
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
    fn acquire(
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

    fn embed_query(&self, query: String) -> Result<Vec<f32>> {
        let model = self.query.as_ref().unwrap_or(&self.document);
        let mut embeddings = self.embed_role(model, vec![query], SEMANTIC_QUERY_PREFIX)?;
        embeddings
            .pop()
            .ok_or_else(|| anyhow!("native Core ML query embedding was empty"))
    }

    fn embed_documents(&self, documents: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.embed_role(&self.document, documents, SEMANTIC_PASSAGE_PREFIX)
    }

    fn embed_role(
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

    fn embed_batch(&self, model: &coreml_native::Model, texts: &[String]) -> Result<Vec<Vec<f32>>> {
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
fn validate_coreml_bundle_identity(bundle: &VerifiedModelBundle) -> Result<()> {
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
enum CoreMlComputeMode {
    All,
    CpuAndNeuralEngine,
    CpuAndGpu,
    CpuOnly,
}

#[cfg(any(all(ctx_semantic_fastembed, target_os = "macos"), test))]
impl CoreMlComputeMode {
    fn parse(value: &str) -> Result<Self> {
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

    fn compute_class(self) -> SemanticComputeClass {
        match self {
            Self::CpuOnly => SemanticComputeClass::Cpu,
            Self::All | Self::CpuAndNeuralEngine | Self::CpuAndGpu => {
                SemanticComputeClass::Accelerator
            }
        }
    }

    fn as_str(self) -> &'static str {
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
struct CoreMlComputeConfig {
    units: coreml_native::ComputeUnits,
    compute_class: SemanticComputeClass,
    mode: &'static str,
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
fn coreml_compute_units_from_env() -> Result<CoreMlComputeConfig> {
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
fn load_coreml_role_model(
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
fn load_and_validate_coreml_role_model(
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
fn compiled_coreml_model_path(
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
fn copy_directory_contents(source: &Path, destination: &Path) -> Result<()> {
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
fn validate_coreml_model_contract(
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
fn load_coreml_tokenizer(path: &Path, sequence_length: usize) -> Result<tokenizers::Tokenizer> {
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
fn pad_role_batch(
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
fn pad_texts_to_exact_batch(texts: Vec<String>, exact_batch_size: usize) -> Result<Vec<String>> {
    pad_role_batch(texts, exact_batch_size, SEMANTIC_PASSAGE_PREFIX)
}

#[cfg(all(test, ctx_semantic_fastembed))]
fn semantic_fixed_shape_from_values(
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

#[cfg(not(ctx_semantic_fastembed))]
struct SemanticEmbedder;

#[cfg(not(ctx_semantic_fastembed))]
fn new_semantic_embedder(_cache_dir: &Path) -> Result<SemanticEmbedder> {
    Err(anyhow!(
        "semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"
    ))
}

#[cfg(not(ctx_semantic_fastembed))]
fn acquire_semantic_embedder(_cache_dir: &Path) -> Result<SemanticEmbedder> {
    Err(anyhow!(
        "semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"
    ))
}

#[cfg(all(test, ctx_semantic_fastembed))]
mod embedding_backend_tests {
    use super::*;

    #[test]
    fn backend_preference_is_strict() {
        assert_eq!(
            BackendPreference::parse(None).unwrap(),
            BackendPreference::Auto
        );
        assert_eq!(
            BackendPreference::parse(Some("cpu")).unwrap(),
            BackendPreference::Cpu
        );
        assert_eq!(
            BackendPreference::parse(Some("coreml")).unwrap(),
            BackendPreference::CoreMl
        );
        assert!(BackendPreference::parse(Some("gpu")).is_err());
        assert!(BackendPreference::parse(Some("CPU")).is_err());
    }

    #[test]
    fn foreground_model_access_is_cache_only() {
        assert!(!SemanticModelAccess::ForegroundCacheOnly.network_allowed());
        assert!(SemanticModelAccess::DaemonNetwork.network_allowed());
    }

    #[test]
    fn coreml_cpu_only_uses_cpu_quiet_policy_class() {
        let cpu_only = CoreMlComputeMode::parse("cpu").unwrap();
        assert_eq!(cpu_only.compute_class(), SemanticComputeClass::Cpu);
        assert_eq!(cpu_only.as_str(), "cpu_only");
        let all = CoreMlComputeMode::parse("all").unwrap();
        assert_eq!(all.compute_class(), SemanticComputeClass::Accelerator);

        let available = 5 * 512 * 1024 * 1024;
        assert!(semantic_model_load_deferred(Some(available), cpu_only.compute_class()).is_none());
        assert!(semantic_model_load_deferred(Some(available), all.compute_class()).is_some());
    }

    #[test]
    fn normalization_is_central_and_strict() {
        let mut vector = vec![0.0; SEMANTIC_DIMENSIONS];
        vector[0] = 3.0;
        vector[1] = 4.0;
        let normalized = normalize_and_validate_embeddings(vec![vector], 1).unwrap();
        assert!((normalized[0][0] - 0.6).abs() < 1e-6);
        assert!((normalized[0][1] - 0.8).abs() < 1e-6);

        assert!(normalize_and_validate_embeddings(Vec::new(), 1).is_err());
        assert!(normalize_and_validate_embeddings(vec![vec![1.0]], 1).is_err());
        assert!(
            normalize_and_validate_embeddings(vec![vec![0.0; SEMANTIC_DIMENSIONS]], 1).is_err()
        );
        let mut non_finite = vec![1.0; SEMANTIC_DIMENSIONS];
        non_finite[0] = f32::NAN;
        assert!(normalize_and_validate_embeddings(vec![non_finite], 1).is_err());
    }

    #[test]
    fn runtime_info_keeps_space_identity_backend_independent() {
        let cpu = SemanticEmbeddingRuntimeInfo {
            preference: BackendPreference::Auto,
            backend: "cpu",
            compute_class: SemanticComputeClass::Cpu,
            compute_mode: None,
            acquisition_source: "cache",
            acquisition_fallback: None,
        };
        let coreml = SemanticEmbeddingRuntimeInfo {
            backend: "coreml",
            ..cpu.clone()
        };
        assert_eq!(cpu.to_json()["model_key"], coreml.to_json()["model_key"]);
        assert_ne!(cpu.to_json()["backend"], coreml.to_json()["backend"]);
    }
}
