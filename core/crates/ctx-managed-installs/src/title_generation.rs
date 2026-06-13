use super::*;

pub async fn install_title_generation_local_with_progress(
    state: std::sync::Arc<dyn TitleGenerationLocalInstallHost>,
    install_id: InstallId,
) -> Result<()> {
    let res = install_title_generation_local_impl(state.as_ref(), Some(install_id)).await;
    match &res {
        Ok(()) => state.finish_install(install_id, true, None, None).await,
        Err(e) => {
            let code = classify_install_error("title_generation_install", e);
            state
                .finish_install(
                    install_id,
                    false,
                    Some(truncate_for_storage(&format!("{e:#}"), 12_000)),
                    Some(code),
                )
                .await
        }
    }
    res
}

pub trait TitleGenerationLocalInstallHost: InstallProgressHost {
    fn data_root(&self) -> &Path;
}

impl<T> TitleGenerationLocalInstallHost for T
where
    T: ManagedInstallHost + ?Sized,
{
    fn data_root(&self) -> &Path {
        ManagedInstallHost::data_root(self)
    }
}

async fn install_title_generation_local_impl(
    state: &dyn TitleGenerationLocalInstallHost,
    install_id: Option<InstallId>,
) -> Result<()> {
    let Some(runtime_spec) = title_generation_local::runtime_download_spec() else {
        anyhow::bail!("llama.cpp runtime not available for this platform");
    };

    let runtime_bin = title_generation_local::find_runtime_binary(state.data_root());
    if runtime_bin.is_none() {
        emit_install(
            state,
            install_id,
            TITLE_GENERATION_LOCAL_INSTALL_KEY,
            InstallEventLevel::Info,
            "runtime_download",
            "downloading llama.cpp runtime".to_string(),
            None,
            None,
            None,
        )
        .await;

        let tmp = std::env::temp_dir().join(format!(
            "ctx-title-runtime-{}.download",
            install_id.unwrap_or_else(InstallId::new_v4)
        ));
        download_to_file(
            state,
            install_id,
            TITLE_GENERATION_LOCAL_INSTALL_KEY,
            "runtime_download",
            runtime_spec.url,
            &tmp,
        )
        .await?;

        emit_install(
            state,
            install_id,
            TITLE_GENERATION_LOCAL_INSTALL_KEY,
            InstallEventLevel::Info,
            "runtime_verify",
            "verifying runtime checksum".to_string(),
            None,
            None,
            None,
        )
        .await;

        let digest = sha256_file(&tmp).await?;
        if !digest.eq_ignore_ascii_case(runtime_spec.sha256) {
            anyhow::bail!(
                "llama.cpp runtime checksum mismatch: expected {}, got {}",
                runtime_spec.sha256,
                digest
            );
        }

        emit_install(
            state,
            install_id,
            TITLE_GENERATION_LOCAL_INSTALL_KEY,
            InstallEventLevel::Info,
            "runtime_extract",
            "extracting runtime".to_string(),
            None,
            None,
            None,
        )
        .await;

        let runtime_dir = title_generation_local::runtime_dir(state.data_root());
        let staging_dir = prepare_atomic_install_dir(&runtime_dir).await?;
        let extract_result = match runtime_spec.archive_kind {
            title_generation_local::RuntimeArchiveKind::TarGz => {
                extract_tar_gz_to_dir(&tmp, &staging_dir).context("extract runtime tar.gz")
            }
            title_generation_local::RuntimeArchiveKind::Zip => {
                extract_zip_to_dir(&tmp, &staging_dir)
            }
        };
        if let Err(error) = extract_result {
            tokio::fs::remove_dir_all(&staging_dir).await.ok();
            return Err(error);
        }

        let staged_runtime_bin = match find_unique_path_ending_with(
            &staging_dir,
            title_generation_local::runtime_binary_name(),
        )
        .with_context(|| {
            format!(
                "llama-server binary not found after extracting {}",
                runtime_dir.display()
            )
        }) {
            Ok(path) => path,
            Err(error) => {
                tokio::fs::remove_dir_all(&staging_dir).await.ok();
                return Err(error);
            }
        };
        if let Err(error) = ensure_executable(&staged_runtime_bin) {
            tokio::fs::remove_dir_all(&staging_dir).await.ok();
            return Err(error);
        }
        let relative_runtime_bin = match staged_runtime_bin.strip_prefix(&staging_dir) {
            Ok(path) => path.to_path_buf(),
            Err(_) => {
                tokio::fs::remove_dir_all(&staging_dir).await.ok();
                anyhow::bail!(
                    "extracted runtime binary escaped staging dir: {}",
                    staged_runtime_bin.display()
                );
            }
        };
        if let Err(error) = commit_atomic_install_dir(&staging_dir, &runtime_dir).await {
            tokio::fs::remove_dir_all(&staging_dir).await.ok();
            return Err(error);
        }
        let runtime_bin = runtime_dir.join(relative_runtime_bin);
        ensure_executable(&runtime_bin)?;
        tokio::fs::remove_file(&tmp).await.ok();
    }

    let model_dir = title_generation_local::model_dir(state.data_root());
    tokio::fs::create_dir_all(&model_dir).await.ok();
    let model_path = title_generation_local::model_path(state.data_root());

    let expected_sha = fetch_hf_etag_sha256(title_generation_local::LOCAL_MODEL_URL)
        .await
        .unwrap_or(None);
    let mut model_exists = model_path.exists();
    if model_exists {
        let digest = sha256_file(&model_path).await?;
        let needs_metadata = title_generation_local::load_model_metadata(state.data_root())
            .await
            .is_none();
        if let Some(expected) = expected_sha.as_ref() {
            if !digest.eq_ignore_ascii_case(expected) {
                emit_install(
                    state,
                    install_id,
                    TITLE_GENERATION_LOCAL_INSTALL_KEY,
                    InstallEventLevel::Warning,
                    "model_verify",
                    "installed model checksum mismatch; re-downloading".to_string(),
                    None,
                    None,
                    None,
                )
                .await;
                tokio::fs::remove_file(&model_path).await.ok();
                model_exists = false;
            } else if needs_metadata {
                let size = tokio::fs::metadata(&model_path).await?.len();
                let meta = title_generation_local::LocalModelMetadata {
                    id: title_generation_local::LOCAL_MODEL_ID.to_string(),
                    version: title_generation_local::LOCAL_MODEL_VERSION.to_string(),
                    sha256: digest,
                    size,
                    installed_at: Utc::now(),
                };
                title_generation_local::write_model_metadata(state.data_root(), &meta).await?;
            }
        } else if needs_metadata {
            let size = tokio::fs::metadata(&model_path).await?.len();
            let meta = title_generation_local::LocalModelMetadata {
                id: title_generation_local::LOCAL_MODEL_ID.to_string(),
                version: title_generation_local::LOCAL_MODEL_VERSION.to_string(),
                sha256: digest,
                size,
                installed_at: Utc::now(),
            };
            title_generation_local::write_model_metadata(state.data_root(), &meta).await?;
        }
    }

    if !model_exists {
        emit_install(
            state,
            install_id,
            TITLE_GENERATION_LOCAL_INSTALL_KEY,
            InstallEventLevel::Info,
            "model_download",
            "downloading model".to_string(),
            None,
            None,
            None,
        )
        .await;

        let tmp = std::env::temp_dir().join(format!(
            "ctx-title-model-{}.download",
            install_id.unwrap_or_else(InstallId::new_v4)
        ));
        download_to_file(
            state,
            install_id,
            TITLE_GENERATION_LOCAL_INSTALL_KEY,
            "model_download",
            title_generation_local::LOCAL_MODEL_URL,
            &tmp,
        )
        .await?;

        emit_install(
            state,
            install_id,
            TITLE_GENERATION_LOCAL_INSTALL_KEY,
            InstallEventLevel::Info,
            "model_verify",
            "verifying model checksum".to_string(),
            None,
            None,
            None,
        )
        .await;

        let digest = sha256_file(&tmp).await?;
        if let Some(expected) = expected_sha.as_ref() {
            if !digest.eq_ignore_ascii_case(expected) {
                anyhow::bail!("model checksum mismatch: expected {expected}, got {digest}");
            }
        }

        tokio::fs::rename(&tmp, &model_path)
            .await
            .with_context(|| format!("move model to {}", model_path.display()))?;
        let size = tokio::fs::metadata(&model_path).await?.len();
        let meta = title_generation_local::LocalModelMetadata {
            id: title_generation_local::LOCAL_MODEL_ID.to_string(),
            version: title_generation_local::LOCAL_MODEL_VERSION.to_string(),
            sha256: digest,
            size,
            installed_at: Utc::now(),
        };
        title_generation_local::write_model_metadata(state.data_root(), &meta).await?;
    }

    Ok(())
}

fn normalize_etag_sha256(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let trimmed = trimmed.strip_prefix("W/").unwrap_or(trimmed);
    let trimmed = trimmed.trim_matches('"');
    if trimmed.len() != 64 {
        return None;
    }
    if !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(trimmed.to_string())
}

async fn fetch_hf_etag_sha256(url: &str) -> Result<Option<String>> {
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .context("building http client")?;
    let resp = client
        .head(url)
        .send()
        .await
        .context("requesting model header")?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let header = resp
        .headers()
        .get("x-linked-etag")
        .and_then(|value| value.to_str().ok());
    Ok(header.and_then(normalize_etag_sha256))
}

async fn sha256_file(path: &Path) -> Result<String> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("open {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
