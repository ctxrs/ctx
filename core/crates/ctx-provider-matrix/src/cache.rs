use super::*;
use anyhow::Context;
use std::sync::atomic::{AtomicU64, Ordering};

static MATRIX_CACHE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn matrix_cache_path(data_root: &Path) -> PathBuf {
    data_root.join("providers").join(MATRIX_CACHE_FILENAME)
}

fn explicit_bundle_matrix_path_from_env() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("CTX_BUNDLE_MATRIX_JSON") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    None
}

fn bundled_matrix_path_from_env() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("CTX_BUNDLE_DIR") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join(MATRIX_CACHE_FILENAME));
        }
    }
    None
}

fn parse_matrix_from_path(path: &Path) -> anyhow::Result<ProviderMatrix> {
    let txt =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: ProviderMatrix =
        serde_json::from_str(&txt).with_context(|| format!("parsing {}", path.display()))?;
    if parsed.version != MATRIX_SCHEMA_VERSION {
        anyhow::bail!(
            "provider matrix schema mismatch in {}: expected {}, got {}",
            path.display(),
            MATRIX_SCHEMA_VERSION,
            parsed.version
        );
    }
    Ok(parsed)
}

fn load_matrix_from_path(path: &Path) -> Option<ProviderMatrix> {
    parse_matrix_from_path(path).ok()
}

pub fn load_explicit_matrix_from_env() -> anyhow::Result<Option<ProviderMatrix>> {
    let Some(path) = explicit_bundle_matrix_path_from_env() else {
        return Ok(None);
    };
    parse_matrix_from_path(&path).map(Some)
}

pub fn load_bundled_matrix_from_env() -> Option<ProviderMatrix> {
    bundled_matrix_path_from_env().and_then(|path| load_matrix_from_path(&path))
}

pub fn builtin_matrix() -> ProviderMatrix {
    ProviderMatrix::default()
}

pub async fn load_matrix(_data_root: &Path) -> ProviderMatrix {
    if let Ok(Some(matrix)) = load_explicit_matrix_from_env() {
        return matrix;
    }
    if let Some(matrix) = load_bundled_matrix_from_env() {
        return matrix;
    }
    builtin_matrix()
}

#[derive(Debug, Clone)]
pub struct MatrixRefreshOutcome {
    pub matrix: ProviderMatrix,
    pub source: MatrixRefreshSource,
    pub degraded: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatrixRefreshSource {
    Bundled,
    Builtin,
    Explicit,
}

impl MatrixRefreshSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::Builtin => "builtin",
            Self::Explicit => "explicit",
        }
    }
}

fn explicit_provider_matrix_override_enabled() -> bool {
    std::env::var("CTX_BUNDLE_MATRIX_JSON")
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

pub async fn refresh_matrix_from_local_sources(
    _data_root: &Path,
    cache: &tokio::sync::Mutex<ProviderMatrixCache>,
) -> MatrixRefreshOutcome {
    if explicit_provider_matrix_override_enabled() {
        return match load_explicit_matrix_from_env() {
            Ok(Some(matrix)) => {
                replace_matrix_cache(cache, matrix.clone()).await;
                MatrixRefreshOutcome {
                    matrix,
                    source: MatrixRefreshSource::Explicit,
                    degraded: false,
                    last_error: None,
                }
            }
            Ok(None) => {
                fallback_matrix_outcome(cache, "explicit provider matrix override is empty").await
            }
            Err(err) => fallback_matrix_outcome(cache, err.to_string()).await,
        };
    }

    if let Some(matrix) = load_bundled_matrix_from_env() {
        replace_matrix_cache(cache, matrix.clone()).await;
        return MatrixRefreshOutcome {
            matrix,
            source: MatrixRefreshSource::Bundled,
            degraded: false,
            last_error: None,
        };
    }

    fallback_matrix_outcome(
        cache,
        "bundled provider matrix is unavailable; using built-in provider matrix",
    )
    .await
}

async fn fallback_matrix_outcome(
    cache: &tokio::sync::Mutex<ProviderMatrixCache>,
    last_error: impl Into<String>,
) -> MatrixRefreshOutcome {
    if let Some(matrix) = load_bundled_matrix_from_env() {
        replace_matrix_cache(cache, matrix.clone()).await;
        return MatrixRefreshOutcome {
            matrix,
            source: MatrixRefreshSource::Bundled,
            degraded: true,
            last_error: Some(last_error.into()),
        };
    }
    let matrix = builtin_matrix();
    replace_matrix_cache(cache, matrix.clone()).await;
    MatrixRefreshOutcome {
        matrix,
        source: MatrixRefreshSource::Builtin,
        degraded: true,
        last_error: Some(last_error.into()),
    }
}

pub async fn load_matrix_cached(
    data_root: &Path,
    cache: &tokio::sync::Mutex<ProviderMatrixCache>,
) -> ProviderMatrix {
    let cached = {
        let guard = cache.lock().await;
        if let Some(at) = guard.cached_at {
            if at.elapsed() < MATRIX_CACHE_TTL {
                guard.matrix.clone()
            } else {
                None
            }
        } else {
            None
        }
    };

    if let Some(matrix) = cached {
        return matrix;
    }

    let matrix = load_matrix(data_root).await;
    let mut guard = cache.lock().await;
    guard.cached_at = Some(Instant::now());
    guard.matrix = Some(matrix.clone());
    matrix
}

pub async fn invalidate_matrix_cache(cache: &tokio::sync::Mutex<ProviderMatrixCache>) {
    let mut guard = cache.lock().await;
    guard.cached_at = None;
    guard.matrix = None;
}

pub async fn replace_matrix_cache(
    cache: &tokio::sync::Mutex<ProviderMatrixCache>,
    matrix: ProviderMatrix,
) {
    let mut guard = cache.lock().await;
    guard.cached_at = Some(Instant::now());
    guard.matrix = Some(matrix);
}

pub fn load_cached_matrix(data_root: &Path) -> Option<ProviderMatrix> {
    let path = matrix_cache_path(data_root);
    load_matrix_from_path(&path)
}

pub async fn save_cached_matrix(data_root: &Path, matrix: &ProviderMatrix) -> anyhow::Result<()> {
    let path = matrix_cache_path(data_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let txt = serde_json::to_string_pretty(matrix).context("serializing provider matrix")?;
    let sequence = MATRIX_CACHE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{sequence}", std::process::id()));
    tokio::fs::write(&tmp, txt)
        .await
        .with_context(|| format!("writing {}", tmp.display()))?;
    tokio::fs::rename(&tmp, &path)
        .await
        .with_context(|| format!("committing {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}
