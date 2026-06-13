use base64::Engine;
use ctx_core::models::MessageAttachment;
use ctx_session_message_service::message_admission::{
    MessageAttachmentSignature, MessageAttachmentSignatureError, MessageAttachmentSignatureResolver,
};
use sha2::Digest;

use crate::daemon::SessionMessageCommandHandle;

#[async_trait::async_trait]
impl MessageAttachmentSignatureResolver for SessionMessageCommandHandle {
    async fn message_attachment_signature(
        &self,
        attachment: &MessageAttachment,
    ) -> Result<MessageAttachmentSignature, MessageAttachmentSignatureError> {
        match attachment {
            MessageAttachment::Image {
                mime_type,
                data_base64,
                name,
            } => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data_base64.as_bytes())
                    .map_err(|_| {
                        MessageAttachmentSignatureError::BadRequest(
                            "Invalid image attachment.".to_string(),
                        )
                    })?;
                let mut hasher = sha2::Sha256::new();
                hasher.update(&bytes);
                Ok(MessageAttachmentSignature {
                    mime_type: mime_type.clone(),
                    name: name.clone(),
                    sha256: hex::encode(hasher.finalize()),
                })
            }
            MessageAttachment::ImageRef { blob_id, name, .. } => {
                let Some((sha256, mime_type, _bytes, _stored_name, _created_at)) =
                    self.get_blob(blob_id).await.map_err(|_| {
                        MessageAttachmentSignatureError::Internal(
                            "Failed to inspect image attachment.".to_string(),
                        )
                    })?
                else {
                    return Err(MessageAttachmentSignatureError::BadRequest(
                        "Image attachment blob was not found.".to_string(),
                    ));
                };
                Ok(MessageAttachmentSignature {
                    mime_type,
                    name: name.clone(),
                    sha256,
                })
            }
        }
    }
}
