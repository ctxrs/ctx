fn semantic_env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

pub(crate) fn semantic_health_findings(data_root: &Path) -> Vec<String> {
    let mut findings = Vec::new();
    let semantic_lock = semantic_worker_lock_path(data_root);
    if semantic_lock.exists() && semantic_worker_lock_is_stale(&semantic_lock) {
        findings.push(format!(
            "semantic worker lock is stale: {}",
            semantic_lock.display()
        ));
    }
    let daemon_lock = daemon_lock_path(data_root);
    if daemon_lock.exists() && daemon_lock_is_stale(&daemon_lock) {
        findings.push(format!("daemon lock is stale: {}", daemon_lock.display()));
    }
    if let Some(status) = read_semantic_worker_status(data_root) {
        if status.get("status").and_then(Value::as_str) == Some("failed") {
            let error = json_string(&status, "last_error").unwrap_or_else(|| "unknown".to_owned());
            findings.push(format!("semantic worker last failed: {error}"));
        }
    }
    if let Some(status) = read_daemon_status(data_root) {
        if status.get("status").and_then(Value::as_str) == Some("failed") {
            let error = json_string(&status, "last_error").unwrap_or_else(|| "unknown".to_owned());
            findings.push(format!("daemon last failed: {error}"));
        }
    }
    let vector_path = semantic_vector_path(data_root);
    if vector_path.exists() {
        match SemanticVectorStore::open_read_only(&vector_path) {
            Ok(Some(vector_store)) => match vector_store.plaintext_value_count() {
                Ok(0) => {}
                Ok(count) => findings.push(format!(
                    "semantic vector sidecar contains {count} plaintext value(s); run daemon maintenance to scrub it"
                )),
                Err(error) => findings.push(format!(
                    "semantic vector sidecar plaintext check failed: {error:#}"
                )),
            },
            Ok(None) => {}
            Err(error) => findings.push(format!(
                "semantic vector sidecar is unreadable at {}: {error:#}",
                vector_path.display()
            )),
        }
    }
    findings
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

fn json_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|value| value.as_i64())
}

fn json_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
}

fn json_usize(value: &Value, key: &str) -> Option<usize> {
    value
        .get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
}

fn create_private_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("create private directory {}", path.display()))?;
    secure_private_dir_permissions(path)?;
    Ok(())
}

fn private_create_new_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

#[cfg(unix)]
fn secure_private_dir_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("secure private directory {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_private_dir_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_private_file_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("secure private file {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_private_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_semantic_vector_permissions(path: &Path) -> Result<()> {
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        if candidate.exists() {
            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("secure semantic vector file {}", candidate.display()))?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_semantic_vector_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn sqlite_column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sqlite_table_has_columns(conn: &Connection, table: &str, columns: &[&str]) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    let mut existing = std::collections::HashSet::new();
    while let Some(row) = rows.next()? {
        existing.insert(row.get::<_, String>(1)?);
    }
    Ok(columns.iter().all(|column| existing.contains(*column)))
}

fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(exists)
}

fn sqlite_table_sql(conn: &Connection, table: &str) -> Result<Option<String>> {
    let sql = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(sql)
}

fn semantic_query_text(query: &str, terms: &[String]) -> String {
    let mut parts = Vec::new();
    if !query.trim().is_empty() {
        parts.push(query.trim().to_owned());
    }
    parts.extend(
        terms
            .iter()
            .map(|term| term.trim())
            .filter(|term| !term.is_empty())
            .map(str::to_owned),
    );
    parts.join(" ")
}

fn semantic_filters_need_overfetch(filters: &ctx_history_search::SearchFilters) -> bool {
    semantic_filters_require_lexical_fallback(filters)
        || !filters.include_subagents
        || filters.exclude_provider_session.is_some()
}

fn semantic_filters_require_lexical_fallback(filters: &ctx_history_search::SearchFilters) -> bool {
    filters.session.is_some()
        || filters.provider.is_some()
        || filters.history_source.is_some()
        || filters.provider_key.is_some()
        || filters.source_id.is_some()
        || filters.source_format.is_some()
        || filters
            .repo
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
        || filters.since.is_some()
        || filters.primary_only
        || filters.event_type.is_some()
        || filters
            .file
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
}

fn semantic_hybrid_coverage_ready(
    embedded_items: usize,
    searchable_items: usize,
    dirty_items: usize,
) -> bool {
    if searchable_items == 0 {
        return true;
    }
    embedded_items >= searchable_items && dirty_items == 0
}

fn semantic_status_needs_exact_sidecar_stats(
    searchable_items: usize,
    dirty_items: usize,
    stats: SemanticSidecarStats,
) -> bool {
    if searchable_items == 0 || dirty_items > 0 {
        return false;
    }
    stats.embedded_items >= searchable_items
}

fn semantic_hits_for_text_query(
    data_root: &Path,
    store: &Store,
    vector_store: &SemanticVectorStore,
    semantic_text: &str,
    limit: usize,
    event_filter: Option<&[Uuid]>,
) -> Result<(
    Vec<ctx_history_search::SemanticEventHit>,
    SemanticRetrievalDiagnostics,
)> {
    let (query_embedding, query_embed_ms) = daemon_query_embedding(data_root, semantic_text)?
        .ok_or_else(|| anyhow!("daemon semantic query service is not available"))?;
    let semantic_hit_search =
        semantic_hits_for_query(store, vector_store, &query_embedding, limit, event_filter)?;
    let mut diagnostics = semantic_hit_search.diagnostics;
    diagnostics.query_embed_ms = Some(query_embed_ms);
    Ok((semantic_hit_search.hits, diagnostics))
}

fn daemon_query_embedding(data_root: &Path, semantic_text: &str) -> Result<Option<(Vec<f32>, u64)>> {
    let Some(response) = daemon_query_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "embed_query",
            "model_key": SEMANTIC_MODEL_KEY,
            "text": semantic_text,
        })),
        StdDuration::from_secs(30),
        1024 * 1024,
    )? else {
        return Ok(None);
    };
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("daemon query failed");
        return Err(anyhow!("{message}"));
    }
    let model_key = response.get("model_key").and_then(Value::as_str).unwrap_or("");
    if model_key != SEMANTIC_MODEL_KEY {
        return Err(anyhow!("daemon query response model key mismatch"));
    }
    let query_embed_ms = response
        .get("query_embed_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let embedding = response
        .get("embedding")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("daemon query response missing embedding"))?
        .iter()
        .map(|value| {
            value
                .as_f64()
                .map(|value| value as f32)
                .ok_or_else(|| anyhow!("daemon query embedding contains a non-number"))
        })
        .collect::<Result<Vec<_>>>()?;
    if embedding.len() != SEMANTIC_DIMENSIONS {
        return Err(anyhow!(
            "daemon query embedding returned {} dimensions, expected {}",
            embedding.len(),
            SEMANTIC_DIMENSIONS
        ));
    }
    Ok(Some((embedding, query_embed_ms)))
}

#[cfg(ctx_semantic_fastembed)]
struct SemanticEmbedder {
    model: TextEmbedding,
    batch_size: usize,
    policy: SemanticEmbedPolicy,
}

#[cfg(ctx_semantic_fastembed)]
static FASTEMBED_ACQUIRE_ENV_LOCK: Mutex<()> = Mutex::new(());

#[cfg(not(ctx_semantic_fastembed))]
struct SemanticEmbedder;

#[cfg(ctx_semantic_fastembed)]
#[derive(Debug, Clone, Copy)]
struct SemanticMemorySnapshot {
    total_bytes: Option<u64>,
    available_bytes: Option<u64>,
}

#[cfg(ctx_semantic_fastembed)]
#[derive(Debug, Clone)]
struct SemanticEmbedPolicy {
    threads: usize,
    batch_size: usize,
    memory_budget_bytes: u64,
    total_memory_bytes: Option<u64>,
    available_memory_bytes: Option<u64>,
    source: &'static str,
}

#[cfg(ctx_semantic_fastembed)]
impl SemanticEmbedPolicy {
    fn status_json(&self) -> Value {
        compact_json(json!({
            "source": self.source,
            "threads": self.threads,
            "batch_size": self.batch_size,
            "memory_budget_bytes": self.memory_budget_bytes,
            "total_memory_bytes": self.total_memory_bytes,
            "available_memory_bytes": self.available_memory_bytes,
        }))
    }
}

#[cfg(ctx_semantic_fastembed)]
fn new_semantic_embedder(cache_dir: &Path) -> Result<SemanticEmbedder> {
    let snapshot = semantic_model_cache_snapshot_dir(cache_dir).ok_or_else(|| {
        anyhow!(
            "semantic model cache is incomplete at {}",
            cache_dir.display()
        )
    })?;
    let policy = semantic_embed_policy();
    let model_info = TextEmbedding::get_model_info(&EmbeddingModel::AllMiniLML6V2)?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: fs::read(snapshot.join("tokenizer.json"))
            .with_context(|| format!("read semantic tokenizer.json from {}", snapshot.display()))?,
        config_file: fs::read(snapshot.join("config.json"))
            .with_context(|| format!("read semantic config.json from {}", snapshot.display()))?,
        special_tokens_map_file: fs::read(snapshot.join("special_tokens_map.json")).with_context(
            || {
                format!(
                    "read semantic special_tokens_map.json from {}",
                    snapshot.display()
                )
            },
        )?,
        tokenizer_config_file: fs::read(snapshot.join("tokenizer_config.json")).with_context(
            || {
                format!(
                    "read semantic tokenizer_config.json from {}",
                    snapshot.display()
                )
            },
        )?,
    };
    let mut user_model = UserDefinedEmbeddingModel::new(
        fs::read(snapshot.join(&model_info.model_file)).with_context(|| {
            format!(
                "read semantic model file {} from {}",
                model_info.model_file,
                snapshot.display()
            )
        })?,
        tokenizer_files,
    )
    .with_pooling(
        TextEmbedding::get_default_pooling_method(&EmbeddingModel::AllMiniLML6V2)
            .unwrap_or(Pooling::Mean),
    )
    .with_quantization(TextEmbedding::get_quantization_mode(
        &EmbeddingModel::AllMiniLML6V2,
    ));
    user_model.output_key = model_info.output_key.clone();
    let options = InitOptionsUserDefined::new().with_intra_threads(policy.threads);
    let model = TextEmbedding::try_new_from_user_defined(user_model, options)
        .with_context(|| format!("initialize semantic embedding model {SEMANTIC_MODEL_ID}"))?;
    Ok(SemanticEmbedder {
        model,
        batch_size: policy.batch_size,
        policy,
    })
}

#[cfg(ctx_semantic_fastembed)]
fn acquire_semantic_embedder(cache_dir: &Path) -> Result<SemanticEmbedder> {
    let _cache_env = FastembedCacheEnvGuard::new(cache_dir);
    let policy = semantic_embed_policy();
    let options = InitOptions::new(EmbeddingModel::AllMiniLML6V2)
        .with_cache_dir(cache_dir.to_path_buf())
        .with_show_download_progress(false)
        .with_intra_threads(policy.threads);
    let model = TextEmbedding::try_new(options)
        .with_context(|| format!("download and initialize semantic model {SEMANTIC_MODEL_ID}"))?;
    if !semantic_model_cache_available(cache_dir) {
        return Err(anyhow!(
            "semantic model download completed but cache verification failed at {}",
            cache_dir.display()
        ));
    }
    Ok(SemanticEmbedder {
        model,
        batch_size: policy.batch_size,
        policy,
    })
}

#[cfg(ctx_semantic_fastembed)]
struct FastembedCacheEnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    hf_home: Option<std::ffi::OsString>,
}

#[cfg(ctx_semantic_fastembed)]
impl FastembedCacheEnvGuard {
    fn new(cache_dir: &Path) -> Self {
        let lock = FASTEMBED_ACQUIRE_ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let hf_home = env::var_os("HF_HOME");
        env::set_var("HF_HOME", cache_dir);
        Self {
            _lock: lock,
            hf_home,
        }
    }
}

#[cfg(ctx_semantic_fastembed)]
impl Drop for FastembedCacheEnvGuard {
    fn drop(&mut self) {
        match &self.hf_home {
            Some(value) => env::set_var("HF_HOME", value),
            None => env::remove_var("HF_HOME"),
        }
    }
}

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

#[cfg(ctx_semantic_fastembed)]
fn semantic_embed_policy() -> SemanticEmbedPolicy {
    semantic_embed_policy_from_env_and_memory(SemanticMemorySnapshot::current())
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_embed_policy_status_json() -> Value {
    semantic_embed_policy().status_json()
}

#[cfg(not(ctx_semantic_fastembed))]
fn semantic_embed_policy_status_json() -> Value {
    compact_json(json!({
        "source": "unsupported",
    }))
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_embedder_policy_status_json(embedder: &Option<SemanticEmbedder>) -> Value {
    embedder
        .as_ref()
        .map(|embedder| embedder.policy.status_json())
        .unwrap_or_else(semantic_embed_policy_status_json)
}

#[cfg(not(ctx_semantic_fastembed))]
fn semantic_embedder_policy_status_json(_embedder: &Option<SemanticEmbedder>) -> Value {
    semantic_embed_policy_status_json()
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_embed_policy_from_env_and_memory(
    snapshot: SemanticMemorySnapshot,
) -> SemanticEmbedPolicy {
    let mut policy = semantic_adaptive_embed_policy(snapshot);
    let mut source = "adaptive";
    if let Some(threads) = env_usize("CTX_SEMANTIC_THREADS") {
        policy.threads = threads.min(SEMANTIC_EMBED_THREADS_MAX);
        source = "env_override";
    }
    if let Some(batch_size) = env_usize("CTX_SEMANTIC_EMBED_BATCH") {
        policy.batch_size = batch_size.min(SEMANTIC_EMBED_BATCH_MAX);
        source = "env_override";
    }
    policy.source = source;
    policy
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_adaptive_embed_policy(snapshot: SemanticMemorySnapshot) -> SemanticEmbedPolicy {
    let memory_budget_bytes = semantic_adaptive_memory_budget_bytes(snapshot);
    let parallelism = std::thread::available_parallelism()
        .ok()
        .map(|threads| threads.get())
        .unwrap_or(SEMANTIC_EMBED_THREADS_FALLBACK);
    let budget_gib_ceiling = usize::try_from(
        memory_budget_bytes.saturating_add((1024 * 1024 * 1024) - 1) / (1024 * 1024 * 1024),
    )
    .unwrap_or(SEMANTIC_EMBED_THREADS_MAX);
    let threads = budget_gib_ceiling
        .clamp(SEMANTIC_EMBED_THREADS_FALLBACK, SEMANTIC_EMBED_THREADS_MAX)
        .min(parallelism.max(1));
    let raw_batch =
        usize::try_from(memory_budget_bytes / SEMANTIC_EMBED_BATCH_TARGET_BYTES)
            .unwrap_or(SEMANTIC_EMBED_BATCH_ADAPTIVE_MAX);
    let batch_size = (raw_batch / 16 * 16).clamp(
        SEMANTIC_EMBED_BATCH_FALLBACK,
        SEMANTIC_EMBED_BATCH_ADAPTIVE_MAX,
    );
    SemanticEmbedPolicy {
        threads,
        batch_size,
        memory_budget_bytes,
        total_memory_bytes: snapshot.total_bytes,
        available_memory_bytes: snapshot.available_bytes,
        source: "adaptive",
    }
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_adaptive_memory_budget_bytes(snapshot: SemanticMemorySnapshot) -> u64 {
    let by_total = snapshot.total_bytes.map(|bytes| bytes / 5);
    let by_available = snapshot.available_bytes.map(|bytes| bytes / 2);
    let budget = by_total
        .into_iter()
        .chain(by_available)
        .min()
        .unwrap_or(SEMANTIC_MEMORY_BUDGET_FALLBACK_BYTES);
    budget.clamp(
        SEMANTIC_MEMORY_BUDGET_MIN_BYTES,
        SEMANTIC_MEMORY_BUDGET_MAX_BYTES,
    )
}

#[cfg(ctx_semantic_fastembed)]
impl SemanticMemorySnapshot {
    fn current() -> Self {
        semantic_memory_snapshot()
    }
}

#[cfg(all(ctx_semantic_fastembed, target_os = "linux"))]
fn semantic_memory_snapshot() -> SemanticMemorySnapshot {
    let Ok(text) = fs::read_to_string("/proc/meminfo") else {
        return SemanticMemorySnapshot {
            total_bytes: None,
            available_bytes: None,
        };
    };
    let mut total_bytes = None;
    let mut available_bytes = None;
    for line in text.lines() {
        if let Some(bytes) = meminfo_kib_line_bytes(line, "MemTotal:") {
            total_bytes = Some(bytes);
        } else if let Some(bytes) = meminfo_kib_line_bytes(line, "MemAvailable:") {
            available_bytes = Some(bytes);
        }
    }
    SemanticMemorySnapshot {
        total_bytes,
        available_bytes,
    }
}

#[cfg(all(ctx_semantic_fastembed, not(target_os = "linux")))]
fn semantic_memory_snapshot() -> SemanticMemorySnapshot {
    SemanticMemorySnapshot {
        total_bytes: None,
        available_bytes: None,
    }
}

#[cfg(all(ctx_semantic_fastembed, target_os = "linux"))]
fn meminfo_kib_line_bytes(line: &str, key: &str) -> Option<u64> {
    let rest = line.strip_prefix(key)?;
    let kib = rest.split_whitespace().next()?.parse::<u64>().ok()?;
    kib.checked_mul(1024)
}

fn semantic_worker_cache_dir(data_root: &Path) -> PathBuf {
    let env = SemanticCacheEnv::current();
    semantic_worker_cache_dir_from_env(data_root, &env)
}

#[derive(Debug, Clone, Default)]
struct SemanticCacheEnv {
    hf_home: Option<PathBuf>,
    semantic_cache_dir: Option<PathBuf>,
    fastembed_cache_dir: Option<PathBuf>,
    hf_hub_cache: Option<PathBuf>,
    xdg_cache_home: Option<PathBuf>,
    home: Option<PathBuf>,
    current_dir: Option<PathBuf>,
}

impl SemanticCacheEnv {
    fn current() -> Self {
        Self {
            hf_home: env_path("HF_HOME"),
            semantic_cache_dir: env_path("CTX_SEMANTIC_CACHE_DIR"),
            fastembed_cache_dir: env_path("FASTEMBED_CACHE_DIR"),
            hf_hub_cache: env_path("HF_HUB_CACHE"),
            xdg_cache_home: env_path("XDG_CACHE_HOME"),
            home: env_path("HOME"),
            current_dir: env::current_dir().ok(),
        }
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn semantic_worker_cache_dir_from_env(data_root: &Path, env: &SemanticCacheEnv) -> PathBuf {
    if let Some(path) = env.semantic_cache_dir.as_ref() {
        return path.clone();
    }
    if let Some(path) = env.fastembed_cache_dir.as_ref() {
        return path.clone();
    }
    if let Some(path) = env.hf_hub_cache.as_ref() {
        return path.clone();
    }
    if let Some(path) = env.hf_home.as_ref() {
        return path.clone();
    }

    semantic_worker_default_cache_candidates(data_root, env)
        .into_iter()
        .find(|path| semantic_model_cache_available(path))
        .unwrap_or_else(|| data_root.join("semantic-model-cache"))
}

fn semantic_worker_default_cache_candidates(
    data_root: &Path,
    env: &SemanticCacheEnv,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    push_unique_path(&mut candidates, data_root.join("semantic-model-cache"));
    if let Some(current_dir) = env.current_dir.as_ref() {
        push_unique_path(&mut candidates, current_dir.join(".fastembed_cache"));
    }
    if let Some(xdg_cache_home) = env.xdg_cache_home.as_ref() {
        push_unique_path(&mut candidates, xdg_cache_home.join("fastembed"));
        push_unique_path(&mut candidates, xdg_cache_home.join("huggingface").join("hub"));
        push_unique_path(&mut candidates, xdg_cache_home.join("huggingface"));
    }
    if let Some(home) = env.home.as_ref() {
        let cache = home.join(".cache");
        push_unique_path(&mut candidates, home.join(".fastembed_cache"));
        push_unique_path(&mut candidates, cache.join("fastembed"));
        push_unique_path(&mut candidates, cache.join("huggingface").join("hub"));
        push_unique_path(&mut candidates, cache.join("huggingface"));
    }
    candidates
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn semantic_model_cache_available(cache_dir: &Path) -> bool {
    semantic_model_cache_snapshot_dir(cache_dir).is_some()
}

fn semantic_model_cache_snapshot_dir(cache_dir: &Path) -> Option<PathBuf> {
    if !semantic_embedding_supported() {
        return None;
    }
    if cache_dir.as_os_str().is_empty() {
        return None;
    }
    for model_root in semantic_model_cache_roots(cache_dir) {
        if let Some(snapshot) = semantic_model_snapshot_from_root(&model_root) {
            return Some(snapshot);
        }
    }
    None
}

fn semantic_model_cache_roots(cache_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if cache_dir
        .file_name()
        .and_then(|name| name.to_str())
        == Some(SEMANTIC_HF_MODEL_CACHE_DIR)
    {
        push_unique_path(&mut roots, cache_dir.to_path_buf());
    }
    push_unique_path(&mut roots, cache_dir.join(SEMANTIC_HF_MODEL_CACHE_DIR));
    push_unique_path(
        &mut roots,
        cache_dir.join("hub").join(SEMANTIC_HF_MODEL_CACHE_DIR),
    );
    roots
}

fn semantic_model_snapshot_from_root(model_root: &Path) -> Option<PathBuf> {
    let snapshot_ref = fs::read_to_string(model_root.join("refs").join("main")).ok()?;
    let snapshot_ref = snapshot_ref.trim();
    if snapshot_ref.is_empty()
        || snapshot_ref.contains('/')
        || snapshot_ref.contains('\\')
        || snapshot_ref == "."
        || snapshot_ref == ".."
    {
        return None;
    }
    let snapshot = model_root.join("snapshots").join(snapshot_ref);
    if !snapshot.is_dir() {
        return None;
    }
    if SEMANTIC_REQUIRED_MODEL_FILES.iter().all(|file| {
        fs::metadata(snapshot.join(file))
            .map(|metadata| metadata.is_file() && metadata.len() > 0)
            .unwrap_or(false)
    }) {
        Some(snapshot)
    } else {
        None
    }
}

#[cfg(ctx_semantic_fastembed)]
fn semantic_embedding_supported() -> bool {
    true
}

#[cfg(not(ctx_semantic_fastembed))]
fn semantic_embedding_supported() -> bool {
    false
}

fn env_usize(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}
