use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ctx_harness_setup::{
    observe_log, observe_phase, HarnessSetupLogLevel, HarnessSetupObserver, HarnessSetupPhase,
    ManagedArtifactDownloadReporter,
};
use fs2::FileExt;
use futures::StreamExt;
use sha2::Digest;
use tokio::fs;
use tokio::io::AsyncWriteExt;

const DEFAULT_DOWNLOAD_BASE_URL: &str = "https://api.ctx.rs/functions/v1";
const MANAGED_ARTIFACT_RETRY_COUNT: u32 = 4;
const MANAGED_ARTIFACT_DISK_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;

fn default_download_base_url() -> String {
    std::env::var("CTX_DOWNLOAD_BASE_URL").unwrap_or_else(|_| DEFAULT_DOWNLOAD_BASE_URL.to_string())
}

fn join_url(base_url: &str, url_path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), url_path)
}

async fn sha256_hex_file(path: &Path) -> Result<String> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn resolve_download_resume(
    existing_len: u64,
    status: reqwest::StatusCode,
    content_length: Option<u64>,
) -> (bool, Option<u64>) {
    let resumed = existing_len > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;
    let total = match content_length {
        Some(remaining) if resumed => Some(existing_len.saturating_add(remaining)),
        other => other,
    };
    (resumed, total)
}

fn managed_artifact_connect_timeout() -> Duration {
    Duration::from_secs(20)
}

fn managed_artifact_no_progress_timeout() -> Duration {
    if cfg!(test) {
        Duration::from_millis(250)
    } else {
        Duration::from_secs(30)
    }
}

fn managed_artifact_retry_backoff(attempt: u32) -> Duration {
    if cfg!(test) {
        Duration::from_millis(20 * attempt as u64)
    } else {
        Duration::from_secs(attempt as u64)
    }
}

fn resolve_managed_artifact_download_url_with_base(url: &str, base_url: &str) -> Result<String> {
    if let Some(raw_path) = url.strip_prefix("locked://") {
        let normalized_path = raw_path.trim().trim_start_matches('/');
        if normalized_path.is_empty() {
            anyhow::bail!("managed artifact locked URL is missing a path: {url}");
        }
        if normalized_path.starts_with("runtimes/avf-linux-guest/") {
            anyhow::bail!(
                "managed AVF runtime source is unresolved in runtime lock; expected an immutable published URL instead of {url}"
            );
        }
        return Ok(join_url(base_url, &format!("/{normalized_path}")));
    }
    Ok(url.to_string())
}

fn resolve_managed_artifact_download_url(url: &str) -> Result<String> {
    resolve_managed_artifact_download_url_with_base(url, &default_download_base_url())
}

pub fn managed_artifact_partial_path(final_path: &Path) -> PathBuf {
    let file_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("managed-artifact");
    final_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{file_name}.partial"))
}

pub fn managed_artifact_lock_path(final_path: &Path) -> PathBuf {
    let file_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("managed-artifact");
    final_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{file_name}.lock"))
}

pub struct ManagedArtifactFileLockGuard {
    _file: std::fs::File,
}

pub async fn acquire_managed_artifact_file_lock(
    lock_path: &Path,
    artifact_label: &str,
    observer: Option<&dyn HarnessSetupObserver>,
    phase: HarnessSetupPhase,
) -> Result<ManagedArtifactFileLockGuard> {
    let Some(parent) = lock_path.parent() else {
        anyhow::bail!(
            "managed artifact lock path missing parent: {}",
            lock_path.display()
        );
    };
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("creating {}", parent.display()))?;

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .with_context(|| format!("opening managed artifact lock {}", lock_path.display()))?;

    match file.try_lock_exclusive() {
        Ok(()) => Ok(ManagedArtifactFileLockGuard { _file: file }),
        Err(err) if err.kind() == ErrorKind::WouldBlock => {
            observe_log(
                observer,
                phase,
                HarnessSetupLogLevel::Info,
                &format!("waiting for another ctx process to finish {artifact_label} preparation"),
            );
            let lock_path = lock_path.to_path_buf();
            let file = tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
                file.lock_exclusive().with_context(|| {
                    format!("locking managed artifact lock {}", lock_path.display())
                })?;
                Ok(file)
            })
            .await
            .context("joining managed artifact lock task")??;
            Ok(ManagedArtifactFileLockGuard { _file: file })
        }
        Err(err) => Err(err)
            .with_context(|| format!("locking managed artifact lock {}", lock_path.display())),
    }
}

async fn ensure_managed_artifact_free_space(
    parent: &Path,
    artifact_label: &str,
    bytes_to_write: u64,
) -> Result<()> {
    let required_bytes = bytes_to_write.saturating_add(MANAGED_ARTIFACT_DISK_HEADROOM_BYTES);
    let parent = parent.to_path_buf();
    let parent_for_check = parent.clone();
    let available_bytes =
        tokio::task::spawn_blocking(move || fs2::available_space(&parent_for_check))
            .await
            .context("joining managed artifact free-space check")?
            .with_context(|| format!("checking free space for {}", parent.display()))?;
    if available_bytes < required_bytes {
        anyhow::bail!(
            "insufficient disk space for {artifact_label} download in {}: need {} free, found {}",
            parent.display(),
            format_byte_count(required_bytes),
            format_byte_count(available_bytes)
        );
    }
    Ok(())
}

async fn verify_managed_artifact_checksum(path: &Path, expected_sha256: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let digest = sha256_hex_file(path)
        .await
        .with_context(|| format!("computing sha256 for {}", path.display()))?;
    Ok(digest.eq_ignore_ascii_case(expected_sha256.trim()))
}

pub async fn finalize_managed_artifact_download(
    tmp_path: &Path,
    final_path: &Path,
    expected_sha256: &str,
    artifact_label: &str,
) -> Result<()> {
    let digest = sha256_hex_file(tmp_path)
        .await
        .with_context(|| format!("computing sha256 for {}", tmp_path.display()))?;
    if !digest.eq_ignore_ascii_case(expected_sha256.trim()) {
        let _ = fs::remove_file(tmp_path).await;
        anyhow::bail!(
            "{artifact_label} checksum mismatch: expected {}, got {}",
            expected_sha256.trim(),
            digest
        );
    }

    match fs::rename(tmp_path, final_path).await {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            if verify_managed_artifact_checksum(final_path, expected_sha256).await? {
                let _ = fs::remove_file(tmp_path).await;
                return Ok(());
            }
            Err(rename_err).with_context(|| {
                format!(
                    "moving {artifact_label} into place: {} -> {}",
                    tmp_path.display(),
                    final_path.display()
                )
            })
        }
    }
}

fn format_byte_count(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.1} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

fn format_duration_compact(duration: Duration) -> String {
    if duration.as_millis() < 1000 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{}s", duration.as_secs())
    }
}

fn managed_artifact_error_is_retryable(err: &anyhow::Error) -> bool {
    let rendered = format!("{err:#}");
    !rendered.contains("insufficient disk space")
        && !rendered.contains("managed artifact server did not provide content length")
        && !rendered.contains("managed artifact total size is unavailable")
}

pub async fn download_managed_artifact(
    url: &str,
    dest: &Path,
    reporter: Option<ManagedArtifactDownloadReporter<'_>>,
) -> Result<()> {
    let resolved_url = resolve_managed_artifact_download_url(url)?;
    let Some(parent) = dest.parent() else {
        anyhow::bail!("download destination missing parent: {}", dest.display());
    };
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("creating {}", parent.display()))?;
    for attempt in 1..=MANAGED_ARTIFACT_RETRY_COUNT {
        let attempt_res: Result<()> = async {
            let client = reqwest::Client::builder()
                .connect_timeout(managed_artifact_connect_timeout())
                .build()
                .context("building reqwest client for managed artifact download")?;
            let existing_len = fs::metadata(dest).await.map(|meta| meta.len()).unwrap_or(0);
            let mut request = client.get(&resolved_url);
            if existing_len > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={existing_len}-"));
            }
            let response = request
                .send()
                .await
                .with_context(|| format!("downloading managed artifact: {resolved_url}"))?;
            let status = response.status();
            if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
                let _ = fs::remove_file(dest).await;
                anyhow::bail!("server rejected ranged resume request");
            }
            let response = response
                .error_for_status()
                .with_context(|| format!("managed artifact download http error: {resolved_url}"))?;
            let content_length = response.content_length().ok_or_else(|| {
                anyhow::anyhow!(
                    "managed artifact server did not provide content length: {resolved_url}"
                )
            })?;
            let (resumed, total_opt) =
                resolve_download_resume(existing_len, status, Some(content_length));
            let total_bytes = total_opt.ok_or_else(|| {
                anyhow::anyhow!("managed artifact total size is unavailable: {resolved_url}")
            })?;

            let download_start_bytes = if resumed { existing_len } else { 0 };
            if let Some(reporter) = reporter.as_ref() {
                observe_phase(
                    reporter.observer,
                    reporter.phase,
                    "downloading required artifacts",
                );
                if attempt > 1 {
                    observe_log(
                        reporter.observer,
                        reporter.phase,
                        HarnessSetupLogLevel::Info,
                        &format!(
                            "retrying {} download (attempt {attempt}/{MANAGED_ARTIFACT_RETRY_COUNT})",
                            reporter.artifact
                        ),
                    );
                }
                let size_suffix = format!(" ({})", format_byte_count(total_bytes));
                if resumed {
                    observe_log(
                        reporter.observer,
                        reporter.phase,
                        HarnessSetupLogLevel::Info,
                        &format!(
                            "resuming {} download from {}{}",
                            reporter.artifact,
                            format_byte_count(existing_len),
                            size_suffix
                        ),
                    );
                } else {
                    observe_log(
                        reporter.observer,
                        reporter.phase,
                        HarnessSetupLogLevel::Info,
                        &format!("starting {} download{}", reporter.artifact, size_suffix),
                    );
                    if existing_len > 0 {
                        observe_log(
                            reporter.observer,
                            reporter.phase,
                            HarnessSetupLogLevel::Warn,
                            &format!(
                                "{} server does not support resume; restarting from byte 0",
                                reporter.artifact
                            ),
                        );
                    }
                }
                reporter.emit_progress(download_start_bytes, Some(total_bytes), None, false);
            }

            let bytes_to_write = if resumed {
                total_bytes.saturating_sub(existing_len)
            } else {
                total_bytes
            };
            ensure_managed_artifact_free_space(
                parent,
                reporter
                    .as_ref()
                    .map(|value| value.artifact.as_str())
                    .unwrap_or("managed artifact"),
                bytes_to_write,
            )
            .await?;

            let mut stream = response.bytes_stream();
            let mut file = if resumed {
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dest)
                    .await
                    .with_context(|| format!("opening {} for append", dest.display()))?
            } else {
                fs::File::create(dest)
                    .await
                    .with_context(|| format!("creating {}", dest.display()))?
            };
            let started = Instant::now();
            let mut downloaded_bytes = download_start_bytes;
            let mut attempt_downloaded_bytes = 0u64;
            let mut next_progress_pct = if total_bytes == 0 {
                100
            } else {
                (((downloaded_bytes as f64 / total_bytes as f64) * 100.0).floor() as u64 / 10 + 1)
                    * 10
            };
            let mut last_progress_snapshot = Instant::now();
            let mut last_progress_log = Instant::now();
            loop {
                let next_chunk =
                    tokio::time::timeout(managed_artifact_no_progress_timeout(), stream.next())
                        .await;
                let chunk = match next_chunk {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(_) => {
                        anyhow::bail!(
                            "no download progress from {resolved_url} for {}",
                            format_duration_compact(managed_artifact_no_progress_timeout())
                        );
                    }
                };
                let chunk = chunk
                    .with_context(|| format!("reading download stream from {resolved_url}"))?;
                file.write_all(&chunk)
                    .await
                    .with_context(|| format!("writing {}", dest.display()))?;
                downloaded_bytes = downloaded_bytes.saturating_add(chunk.len() as u64);
                attempt_downloaded_bytes =
                    attempt_downloaded_bytes.saturating_add(chunk.len() as u64);

                let elapsed = started.elapsed();
                let bytes_per_sec = if elapsed.as_secs_f64() > 0.0 {
                    Some((attempt_downloaded_bytes as f64 / elapsed.as_secs_f64()).round() as u64)
                } else {
                    None
                };

                if let Some(reporter) = reporter.as_ref() {
                    let now = Instant::now();
                    let should_emit_snapshot = now.duration_since(last_progress_snapshot)
                        >= Duration::from_secs(1)
                        || downloaded_bytes >= total_bytes;
                    if should_emit_snapshot {
                        reporter.emit_progress(
                            downloaded_bytes,
                            Some(total_bytes),
                            bytes_per_sec,
                            false,
                        );
                        last_progress_snapshot = now;
                    }

                    let pct = if total_bytes == 0 {
                        100
                    } else {
                        ((downloaded_bytes as f64 / total_bytes as f64) * 100.0).floor() as u64
                    };
                    let should_emit_log = if pct >= next_progress_pct {
                        next_progress_pct = ((pct / 10) + 1) * 10;
                        true
                    } else {
                        now.duration_since(last_progress_log) >= Duration::from_secs(15)
                    };

                    if should_emit_log {
                        observe_log(
                            reporter.observer,
                            reporter.phase,
                            HarnessSetupLogLevel::Info,
                            &format!(
                                "{} download {}% ({} / {})",
                                reporter.artifact,
                                pct.min(100),
                                format_byte_count(downloaded_bytes),
                                format_byte_count(total_bytes),
                            ),
                        );
                        last_progress_log = now;
                    }
                }
            }
            file.flush()
                .await
                .with_context(|| format!("flushing {}", dest.display()))?;
            if let Some(reporter) = reporter.as_ref() {
                let elapsed = started.elapsed();
                let bytes_per_sec = if elapsed.as_secs_f64() > 0.0 {
                    Some((attempt_downloaded_bytes as f64 / elapsed.as_secs_f64()).round() as u64)
                } else {
                    None
                };
                reporter.emit_progress(downloaded_bytes, Some(total_bytes), bytes_per_sec, true);
                observe_log(
                    reporter.observer,
                    reporter.phase,
                    HarnessSetupLogLevel::Info,
                    &format!(
                        "{} download complete ({})",
                        reporter.artifact,
                        format_byte_count(total_bytes)
                    ),
                );
            }
            Ok(())
        }
        .await;

        match attempt_res {
            Ok(()) => return Ok(()),
            Err(err)
                if attempt < MANAGED_ARTIFACT_RETRY_COUNT
                    && managed_artifact_error_is_retryable(&err) =>
            {
                if let Some(reporter) = reporter.as_ref() {
                    observe_log(
                        reporter.observer,
                        reporter.phase,
                        HarnessSetupLogLevel::Warn,
                        &format!(
                            "{} download attempt {attempt}/{MANAGED_ARTIFACT_RETRY_COUNT} failed: {err:#}",
                            reporter.artifact
                        ),
                    );
                }
                tokio::time::sleep(managed_artifact_retry_backoff(attempt)).await;
            }
            Err(err) => return Err(err),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;
