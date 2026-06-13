use anyhow::Result;
use ctx_core::models::{Artifact, Session, SessionEventType};

use crate::daemon::{SessionArtifactsHandle, SessionMessageCommandHandle};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SessionImageBlobStoreError {
    PayloadTooLarge,
    UnsupportedMediaType,
    Internal,
}

impl From<ctx_session_artifacts::ImageBlobStoreError> for SessionImageBlobStoreError {
    fn from(error: ctx_session_artifacts::ImageBlobStoreError) -> Self {
        match error {
            ctx_session_artifacts::ImageBlobStoreError::PayloadTooLarge => Self::PayloadTooLarge,
            ctx_session_artifacts::ImageBlobStoreError::UnsupportedMediaType => {
                Self::UnsupportedMediaType
            }
            ctx_session_artifacts::ImageBlobStoreError::Internal => Self::Internal,
        }
    }
}

impl SessionMessageCommandHandle {
    pub async fn get_blob(
        &self,
        id: &str,
    ) -> Result<
        Option<(
            String,
            String,
            i64,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
        )>,
    > {
        self.global_store().get_blob(id).await
    }

    pub async fn store_inline_image_blob(
        &self,
        bytes: &[u8],
        mime_type: &str,
        name: Option<&str>,
    ) -> Result<String, SessionImageBlobStoreError> {
        ctx_session_artifacts::store_image_blob(
            self.data_root(),
            self.global_store(),
            bytes,
            mime_type,
            name,
        )
        .await
        .map(|stored| stored.blob_id)
        .map_err(SessionImageBlobStoreError::from)
    }
}

impl SessionArtifactsHandle {
    pub(in crate::daemon) async fn replace_session_artifacts_and_publish(
        &self,
        session: &Session,
        artifacts: &[Artifact],
    ) -> Result<()> {
        let store = self
            .existing_session_store_for_write(session.id)
            .await
            .map_err(|error| anyhow::anyhow!("session artifact store unavailable: {error:?}"))?;
        store
            .replace_session_artifacts(session.id, artifacts)
            .await?;
        let event = store
            .append_session_event(
                session.id,
                None,
                None,
                SessionEventType::ArtifactsSet,
                serde_json::json!({ "artifacts": artifacts }),
            )
            .await?;
        self.publish_event(event).await;
        Ok(())
    }
}
