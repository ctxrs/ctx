use std::path::{Path, PathBuf};

use ctx_session_artifacts::{BlobReadError, ImageBlobStoreError, StoredImageBlob};
use ctx_store::Store;
use tokio::fs::File;

#[derive(Clone)]
pub struct BlobHandle {
    pub(in crate::daemon) data_root: PathBuf,
    pub(in crate::daemon) store: Store,
}

impl BlobHandle {
    pub(in crate::daemon) fn new(data_root: PathBuf, store: Store) -> Self {
        Self { data_root, store }
    }
}

pub struct OpenedBlob {
    pub file: File,
    pub mime_type: String,
    pub name: Option<String>,
}

async fn store_image_blob_for_parts(
    data_root: &Path,
    store: &Store,
    bytes: &[u8],
    mime_type: &str,
    name: Option<&str>,
) -> Result<StoredImageBlob, ImageBlobStoreError> {
    ctx_session_artifacts::store_image_blob(data_root, store, bytes, mime_type, name).await
}

impl BlobHandle {
    pub async fn store_image_blob(
        &self,
        bytes: &[u8],
        mime_type: &str,
        name: Option<&str>,
    ) -> Result<StoredImageBlob, ImageBlobStoreError> {
        store_image_blob_for_parts(&self.data_root, &self.store, bytes, mime_type, name).await
    }

    pub async fn open_blob_for_read(&self, id: &str) -> Result<OpenedBlob, BlobReadError> {
        let resolved =
            ctx_session_artifacts::resolve_blob_for_read(&self.data_root, &self.store, id).await?;
        let file = File::open(&resolved.path)
            .await
            .map_err(|_| BlobReadError::NotFound)?;
        Ok(OpenedBlob {
            file,
            mime_type: resolved.mime_type,
            name: resolved.name,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;
    use crate::test_support::TestDaemon;
    use sha2::Digest;
    use tokio::io::AsyncReadExt;

    async fn test_blob_handle() -> (tempfile::TempDir, BlobHandle) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let daemon =
            TestDaemon::new_for_test(data_dir.path().to_path_buf(), "http://127.0.0.1:0".into())
                .await
                .expect("test daemon");
        (data_dir, daemon.blob_handle_for_test())
    }

    #[tokio::test]
    async fn store_image_blob_writes_bytes_and_metadata() {
        let (data_dir, blob) = test_blob_handle().await;
        let stored = blob
            .store_image_blob(b"png-bytes", "image/png", Some("image.png"))
            .await
            .expect("store image blob");

        assert_eq!(
            stored.sha256,
            hex::encode(sha2::Sha256::digest(b"png-bytes"))
        );
        assert_eq!(stored.bytes, 9);
        assert_eq!(stored.mime_type, "image/png");
        assert_eq!(stored.name.as_deref(), Some("image.png"));

        let metadata = blob
            .store
            .get_blob(&stored.blob_id)
            .await
            .expect("metadata lookup")
            .expect("stored metadata");
        assert_eq!(metadata.0, stored.sha256);
        assert_eq!(metadata.1, "image/png");
        assert_eq!(metadata.2, 9);
        assert_eq!(metadata.3.as_deref(), Some("image.png"));

        let path = ctx_session_artifacts::blobs_dir(data_dir.path()).join(&stored.blob_id);
        assert_eq!(
            tokio::fs::read(path).await.expect("blob bytes"),
            b"png-bytes"
        );
    }

    #[tokio::test]
    async fn store_image_blob_rejects_invalid_inputs() {
        let (_data_dir, blob) = test_blob_handle().await;
        let too_large = vec![0u8; ctx_session_artifacts::SESSION_IMAGE_BLOB_MAX_BYTES + 1];
        assert!(matches!(
            blob.store_image_blob(&too_large, "image/png", None).await,
            Err(ImageBlobStoreError::PayloadTooLarge)
        ));
        assert!(matches!(
            blob.store_image_blob(b"text", "text/plain", None).await,
            Err(ImageBlobStoreError::UnsupportedMediaType)
        ));
    }

    #[tokio::test]
    async fn open_blob_for_read_returns_file_and_metadata() {
        let (_data_dir, blob) = test_blob_handle().await;
        let stored = blob
            .store_image_blob(b"gif-bytes", "image/gif", Some("quoted\"name.gif"))
            .await
            .expect("store image blob");

        let opened = blob
            .open_blob_for_read(&stored.blob_id)
            .await
            .expect("open blob");

        assert_eq!(opened.mime_type, "image/gif");
        assert_eq!(opened.name.as_deref(), Some("quoted\"name.gif"));
        let mut file = opened.file;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).await.expect("opened bytes");
        assert_eq!(bytes, b"gif-bytes");
    }

    #[tokio::test]
    async fn open_blob_for_read_classifies_missing_metadata_as_not_found() {
        let (_data_dir, blob) = test_blob_handle().await;
        assert!(matches!(
            blob.open_blob_for_read("missing").await,
            Err(BlobReadError::NotFound)
        ));
    }

    #[tokio::test]
    async fn open_blob_for_read_classifies_missing_backing_file_as_not_found() {
        let (data_dir, blob) = test_blob_handle().await;
        let stored = blob
            .store_image_blob(b"jpeg-bytes", "image/jpeg", None)
            .await
            .expect("store image blob");
        let path = ctx_session_artifacts::blobs_dir(data_dir.path()).join(&stored.blob_id);
        match tokio::fs::remove_file(path).await {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => panic!("remove blob file: {error}"),
        }

        assert!(matches!(
            blob.open_blob_for_read(&stored.blob_id).await,
            Err(BlobReadError::NotFound)
        ));
    }
}
