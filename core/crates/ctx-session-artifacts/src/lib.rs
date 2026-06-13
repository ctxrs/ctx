use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::Utc;
use ctx_core::ids::{ArtifactId, SessionId, TaskId, TurnId, WorkspaceId, WorktreeId};
use ctx_core::models::{Artifact, Session};
use ctx_store::Store;
use sha2::Digest;

pub mod route_contract;

pub const SESSION_IMAGE_BLOB_MAX_BYTES: usize = 25 * 1024 * 1024;
pub const SESSION_IMAGE_BLOB_MULTIPART_MAX_BYTES: usize = SESSION_IMAGE_BLOB_MAX_BYTES + 64 * 1024;
pub const SESSION_IMAGE_BLOB_TOO_LARGE_MESSAGE: &str =
    "Image attachments must be 25 MiB or smaller.";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ImageBlobStoreError {
    PayloadTooLarge,
    UnsupportedMediaType,
    Internal,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BlobReadError {
    NotFound,
    Internal,
}

#[derive(Debug, Clone)]
pub struct StoredImageBlob {
    pub blob_id: String,
    pub sha256: String,
    pub bytes: i64,
    pub mime_type: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedBlob {
    pub path: PathBuf,
    pub mime_type: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SessionArtifactError {
    NotFound,
    BadRequest(String),
    Internal(String),
}

impl std::fmt::Display for SessionArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("session artifact not found"),
            Self::BadRequest(message) => f.write_str(message),
            Self::Internal(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SessionArtifactError {}

#[derive(Debug, Clone)]
pub struct SessionArtifactInput {
    pub absolute_file_path: String,
    pub name: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionArtifactDownload {
    pub canonical_path: PathBuf,
    pub mime_type: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct ToolOutputArtifactScope {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub worktree_id: WorktreeId,
    pub turn_id: TurnId,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ToolOutputArtifactRef {
    pub artifact_id: String,
    pub name: Option<String>,
    pub mime_type: String,
    pub bytes: i64,
}

pub fn blobs_dir(data_root: &Path) -> PathBuf {
    data_root.join("blobs")
}

pub fn normalize_session_artifact_name(name: Option<String>, path: &Path) -> Option<String> {
    if let Some(name) = name {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    path.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

pub fn infer_session_artifact_mime_type(path: &Path, override_value: Option<String>) -> String {
    if let Some(value) = override_value {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mdx"))
    {
        return "text/markdown".to_string();
    }
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .essence_str()
        .to_string()
}

pub fn infer_session_upload_blob_mime_type(
    file_name: Option<&str>,
    override_value: Option<String>,
) -> String {
    match file_name {
        Some(name) => infer_session_artifact_mime_type(Path::new(name), override_value),
        None => override_value.unwrap_or_else(|| "application/octet-stream".to_string()),
    }
}

pub fn build_session_artifact_etag(size: u64, modified: SystemTime) -> Option<String> {
    let modified = modified.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(format!("\"{:x}-{:x}\"", size, modified.as_nanos()))
}

pub fn build_session_artifact_last_modified(modified: SystemTime) -> String {
    let modified = chrono::DateTime::<chrono::Utc>::from(modified);
    modified.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

pub fn sanitize_spool_segment(raw: &str) -> String {
    let mut output: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if output.is_empty() {
        output.push_str("tool_output");
    }
    if output.len() > 80 {
        output.truncate(80);
    }
    output
}

pub async fn store_image_blob(
    data_root: &Path,
    store: &Store,
    bytes: &[u8],
    mime_type: &str,
    name: Option<&str>,
) -> Result<StoredImageBlob, ImageBlobStoreError> {
    if bytes.len() > SESSION_IMAGE_BLOB_MAX_BYTES {
        return Err(ImageBlobStoreError::PayloadTooLarge);
    }
    if !mime_type.starts_with("image/") {
        return Err(ImageBlobStoreError::UnsupportedMediaType);
    }

    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    let sha256 = hex::encode(hasher.finalize());
    let blob_id = uuid::Uuid::new_v4().to_string();

    let dir = blobs_dir(data_root);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|_| ImageBlobStoreError::Internal)?;
    let path = dir.join(&blob_id);
    let tmp = dir.join(format!("{blob_id}.tmp"));

    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|_| ImageBlobStoreError::Internal)?;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(|_| ImageBlobStoreError::Internal)?;

    store
        .insert_blob(
            &blob_id,
            &sha256,
            bytes.len() as i64,
            mime_type,
            name,
            Utc::now(),
        )
        .await
        .map_err(|_| ImageBlobStoreError::Internal)?;

    Ok(StoredImageBlob {
        blob_id,
        sha256,
        bytes: bytes.len() as i64,
        mime_type: mime_type.to_string(),
        name: name.map(str::to_string),
    })
}

pub async fn resolve_blob_for_read(
    data_root: &Path,
    store: &Store,
    id: &str,
) -> Result<ResolvedBlob, BlobReadError> {
    let Some((_sha256, mime_type, _bytes, name, _created_at)) = store
        .get_blob(id)
        .await
        .map_err(|_| BlobReadError::Internal)?
    else {
        return Err(BlobReadError::NotFound);
    };

    Ok(ResolvedBlob {
        path: blobs_dir(data_root).join(id),
        mime_type,
        name,
    })
}

pub async fn list_session_artifacts_with_missing(
    store: &Store,
    session: &Session,
    session_spool_dir: &Path,
) -> Result<Vec<Artifact>, SessionArtifactError> {
    let mut artifacts = store
        .list_session_artifacts(session.id)
        .await
        .map_err(session_artifact_internal_error)?;
    for artifact in artifacts.iter_mut() {
        if !session_artifact_path_is_accessible(
            store,
            session,
            session_spool_dir,
            Path::new(&artifact.absolute_path),
        )
        .await?
        {
            artifact.missing = Some(true);
        }
    }
    Ok(artifacts)
}

pub async fn build_session_artifacts(
    store: &Store,
    session: &Session,
    session_spool_dir: &Path,
    inputs: Vec<SessionArtifactInput>,
) -> Result<Vec<Artifact>, SessionArtifactError> {
    let mut artifacts = Vec::with_capacity(inputs.len());
    for (idx, artifact) in inputs.into_iter().enumerate() {
        let raw = artifact.absolute_file_path.trim();
        if raw.is_empty() {
            return Err(SessionArtifactError::BadRequest(format!(
                "artifact {} missing absolute_file_path",
                idx + 1
            )));
        }
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err(SessionArtifactError::BadRequest(format!(
                "artifact {} absolute_file_path must be absolute",
                idx + 1
            )));
        }
        let meta = tokio::fs::metadata(&path).await.map_err(|e| {
            SessionArtifactError::BadRequest(format!(
                "artifact {} path not accessible: {}",
                idx + 1,
                e
            ))
        })?;
        if !meta.is_file() {
            return Err(SessionArtifactError::BadRequest(format!(
                "artifact {} path is not a file",
                idx + 1
            )));
        }
        let path = validate_session_artifact_write_path(store, session, session_spool_dir, &path)
            .await
            .map_err(|error| {
                SessionArtifactError::BadRequest(format!("artifact {} {error}", idx + 1))
            })?;

        let name = normalize_session_artifact_name(artifact.name, &path);
        let mime_type = infer_session_artifact_mime_type(&path, artifact.mime_type);
        let bytes = meta.len() as i64;
        let created_at = Utc::now();

        artifacts.push(Artifact {
            id: ArtifactId::new(),
            session_id: session.id,
            task_id: session.task_id,
            workspace_id: session.workspace_id,
            worktree_id: session.worktree_id,
            name,
            absolute_path: path.to_string_lossy().to_string(),
            mime_type,
            bytes,
            created_at,
            missing: None,
        });
    }
    Ok(artifacts)
}

pub async fn resolve_session_artifact_download(
    store: &Store,
    session: &Session,
    session_spool_dir: &Path,
    artifact_id: ArtifactId,
) -> Result<SessionArtifactDownload, SessionArtifactError> {
    let artifact = store
        .get_artifact(artifact_id)
        .await
        .map_err(session_artifact_internal_error)?
        .filter(|artifact| artifact.session_id == session.id)
        .ok_or(SessionArtifactError::NotFound)?;
    let path = PathBuf::from(&artifact.absolute_path);
    let canonical_path =
        resolve_session_artifact_accessible_path(store, session, session_spool_dir, &path)
            .await?
            .ok_or(SessionArtifactError::NotFound)?;
    Ok(SessionArtifactDownload {
        canonical_path,
        mime_type: artifact.mime_type,
        name: artifact.name,
    })
}

pub async fn resolve_session_artifact_accessible_path(
    store: &Store,
    session: &Session,
    session_spool_dir: &Path,
    path: &Path,
) -> Result<Option<PathBuf>, SessionArtifactError> {
    let roots = session_artifact_allowed_roots(store, session, session_spool_dir).await?;
    let canonical = match tokio::fs::canonicalize(path).await {
        Ok(canonical) => canonical,
        Err(_) => return Ok(None),
    };
    Ok(roots
        .iter()
        .any(|root| canonical.starts_with(root))
        .then_some(canonical))
}

pub async fn validate_session_artifact_write_path(
    store: &Store,
    session: &Session,
    session_spool_dir: &Path,
    path: &Path,
) -> Result<PathBuf, String> {
    let roots = session_artifact_allowed_roots(store, session, session_spool_dir)
        .await
        .map_err(|error| format!("failed to resolve session artifact roots: {error}"))?;
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|e| e.to_string())?;
    if roots.iter().any(|root| canonical.starts_with(root)) {
        return Ok(canonical);
    }
    Err("absolute_file_path must stay inside the session worktree or tool-output spool".into())
}

pub async fn session_artifact_path_is_accessible(
    store: &Store,
    session: &Session,
    session_spool_dir: &Path,
    path: &Path,
) -> Result<bool, SessionArtifactError> {
    let roots = session_artifact_allowed_roots(store, session, session_spool_dir).await?;
    let canonical = match tokio::fs::canonicalize(path).await {
        Ok(canonical) => canonical,
        Err(_) => return Ok(false),
    };
    Ok(roots.iter().any(|root| canonical.starts_with(root)))
}

pub async fn session_artifact_allowed_roots(
    store: &Store,
    session: &Session,
    session_spool_dir: &Path,
) -> Result<Vec<PathBuf>, SessionArtifactError> {
    let mut roots = Vec::with_capacity(2);
    if let Some(worktree) = store
        .get_worktree(session.worktree_id)
        .await
        .map_err(session_artifact_internal_error)?
    {
        roots.push(canonicalize_existing_or_raw(&PathBuf::from(worktree.root_path)).await);
    }
    roots.push(canonicalize_existing_or_raw(session_spool_dir).await);
    Ok(roots)
}

pub async fn spool_tool_output_artifact(
    store: &Store,
    spool_root: &Path,
    scope: ToolOutputArtifactScope,
    tool_call_id: &str,
    output: &str,
) -> Result<ToolOutputArtifactRef, SessionArtifactError> {
    let dir = spool_root
        .join(scope.session_id.0.to_string())
        .join(scope.turn_id.0.to_string());
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(session_artifact_internal_error)?;

    let file_name = format!("{}.txt", sanitize_spool_segment(tool_call_id));
    let path = dir.join(file_name);
    tokio::fs::write(&path, output.as_bytes())
        .await
        .map_err(session_artifact_internal_error)?;
    let name = format!("tool-output-{}.txt", sanitize_spool_segment(tool_call_id));
    let artifact = Artifact {
        id: ArtifactId::new(),
        session_id: scope.session_id,
        task_id: scope.task_id,
        workspace_id: scope.workspace_id,
        worktree_id: scope.worktree_id,
        name: Some(name),
        absolute_path: path.to_string_lossy().to_string(),
        mime_type: "text/plain".to_string(),
        bytes: output.len() as i64,
        created_at: Utc::now(),
        missing: None,
    };
    let artifact = store
        .upsert_session_artifact_by_path(&artifact)
        .await
        .map_err(session_artifact_internal_error)?;

    Ok(ToolOutputArtifactRef {
        artifact_id: artifact.id.0.to_string(),
        name: artifact.name,
        mime_type: artifact.mime_type,
        bytes: artifact.bytes,
    })
}

async fn canonicalize_existing_or_raw(path: &Path) -> PathBuf {
    tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| path.to_path_buf())
}

fn session_artifact_internal_error(error: impl std::fmt::Display) -> SessionArtifactError {
    SessionArtifactError::Internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use ctx_core::models::{ExecutionEnvironment, Session, VcsKind};

    use super::*;

    #[test]
    fn infer_artifact_mime_type_treats_mdx_as_markdown() {
        assert_eq!(
            infer_session_artifact_mime_type(Path::new("/tmp/merge-queue-for-agents.mdx"), None),
            "text/markdown"
        );
    }

    #[test]
    fn infer_artifact_mime_type_preserves_explicit_override() {
        assert_eq!(
            infer_session_artifact_mime_type(
                Path::new("/tmp/merge-queue-for-agents.mdx"),
                Some("application/mdx".to_string())
            ),
            "application/mdx"
        );
    }

    #[test]
    fn normalize_session_artifact_name_uses_trimmed_display_name() {
        assert_eq!(
            normalize_session_artifact_name(
                Some("  Display name  ".to_string()),
                Path::new("/tmp/fallback.txt")
            ),
            Some("Display name".to_string())
        );
    }

    #[test]
    fn session_artifact_etag_includes_size_and_modified_nanos() {
        let modified = SystemTime::UNIX_EPOCH + Duration::from_nanos(0x20);
        assert_eq!(
            build_session_artifact_etag(0x10, modified),
            Some("\"10-20\"".to_string())
        );
    }

    #[test]
    fn spool_segment_is_sanitized_and_bounded() {
        assert_eq!(sanitize_spool_segment(""), "tool_output");
        assert_eq!(sanitize_spool_segment("tool/id:1"), "tool_id_1");
        assert_eq!(sanitize_spool_segment(&"x".repeat(120)).len(), 80);
    }

    async fn session_fixture() -> (tempfile::TempDir, Store, Session, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("db.sqlite"))
            .await
            .expect("open store");
        let worktree_root = dir.path().join("worktree");
        tokio::fs::create_dir_all(&worktree_root)
            .await
            .expect("worktree dir");
        let workspace = store
            .create_workspace(
                "test".to_string(),
                worktree_root.to_string_lossy().to_string(),
                VcsKind::Git,
            )
            .await
            .expect("workspace");
        let task = store
            .create_task(workspace.id, "task".to_string(), None)
            .await
            .expect("task");
        let worktree = store
            .create_worktree(
                workspace.id,
                worktree_root.to_string_lossy().to_string(),
                "deadbeef".to_string(),
                None,
            )
            .await
            .expect("worktree");
        let session = store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Host,
                "fake".to_string(),
                "fake".to_string(),
                "implementer".to_string(),
                None,
                None,
                None,
            )
            .await
            .expect("session");
        (dir, store, session, worktree_root)
    }

    #[tokio::test]
    async fn build_session_artifacts_accepts_worktree_files_and_rejects_outside_paths() {
        let (_dir, store, session, worktree_root) = session_fixture().await;
        let artifact_path = worktree_root.join("artifact.mdx");
        tokio::fs::write(&artifact_path, b"artifact")
            .await
            .expect("artifact file");
        let spool_dir = worktree_root.join(".spool");

        let artifacts = build_session_artifacts(
            &store,
            &session,
            &spool_dir,
            vec![SessionArtifactInput {
                absolute_file_path: artifact_path.to_string_lossy().to_string(),
                name: Some(" Artifact ".to_string()),
                mime_type: None,
            }],
        )
        .await
        .expect("build artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].name.as_deref(), Some("Artifact"));
        assert_eq!(artifacts[0].mime_type, "text/markdown");

        let outside = tempfile::NamedTempFile::new().expect("outside file");
        let error = build_session_artifacts(
            &store,
            &session,
            &spool_dir,
            vec![SessionArtifactInput {
                absolute_file_path: outside.path().to_string_lossy().to_string(),
                name: None,
                mime_type: None,
            }],
        )
        .await
        .expect_err("outside path must fail");
        assert!(
            matches!(error, SessionArtifactError::BadRequest(message) if message.contains("absolute_file_path must stay inside"))
        );
    }

    #[tokio::test]
    async fn spool_tool_output_artifact_writes_and_registers_artifact() {
        let (_dir, store, session, worktree_root) = session_fixture().await;
        let spool_root = worktree_root.join(".tool-output");
        let artifact = spool_tool_output_artifact(
            &store,
            &spool_root,
            ToolOutputArtifactScope {
                session_id: session.id,
                task_id: session.task_id,
                workspace_id: session.workspace_id,
                worktree_id: session.worktree_id,
                turn_id: TurnId::new(),
            },
            "tool/id:1",
            "large output",
        )
        .await
        .expect("spool artifact");

        assert_eq!(artifact.name.as_deref(), Some("tool-output-tool_id_1.txt"));
        assert_eq!(artifact.mime_type, "text/plain");
        assert_eq!(artifact.bytes, "large output".len() as i64);
        let stored = store
            .get_artifact(ArtifactId(
                uuid::Uuid::parse_str(&artifact.artifact_id).unwrap(),
            ))
            .await
            .expect("lookup")
            .expect("artifact row");
        assert_eq!(
            tokio::fs::read_to_string(stored.absolute_path)
                .await
                .expect("read spooled"),
            "large output"
        );
    }

    #[tokio::test]
    async fn store_image_blob_writes_bytes_and_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("db.sqlite"))
            .await
            .expect("open store");
        let stored = store_image_blob(
            dir.path(),
            &store,
            b"png-bytes",
            "image/png",
            Some("image.png"),
        )
        .await
        .expect("store blob");

        assert_eq!(
            stored.sha256,
            hex::encode(sha2::Sha256::digest(b"png-bytes"))
        );
        assert_eq!(stored.bytes, 9);
        assert_eq!(stored.mime_type, "image/png");
        assert_eq!(stored.name.as_deref(), Some("image.png"));
        assert_eq!(
            tokio::fs::read(blobs_dir(dir.path()).join(&stored.blob_id))
                .await
                .expect("blob bytes"),
            b"png-bytes"
        );
    }
}
