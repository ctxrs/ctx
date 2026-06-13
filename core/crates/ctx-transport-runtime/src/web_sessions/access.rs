use chrono::{DateTime, Utc};

use super::{WebSessionInfo, WebSessionManager};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSessionAccessError {
    MissingToken,
    NotFound,
    Unauthorized,
}

pub struct WebSessionViewConnectPath {
    pub stream_path: String,
    pub expires_at: DateTime<Utc>,
}

pub struct WebSessionViewPage {
    pub info: WebSessionInfo,
    pub signal_path: String,
}

impl WebSessionManager {
    pub async fn mint_view_connect_path(
        &self,
        id: &str,
    ) -> Result<WebSessionViewConnectPath, WebSessionAccessError> {
        let handle = self.get(id).await.ok_or(WebSessionAccessError::NotFound)?;
        let (stream_path, expires_at) = handle.issue_view_connect_path().await;
        Ok(WebSessionViewConnectPath {
            stream_path,
            expires_at,
        })
    }

    pub async fn prepare_view_page(
        &self,
        id: &str,
        token: Option<&str>,
    ) -> Result<WebSessionViewPage, WebSessionAccessError> {
        let handle = self.require_view_access(id, token).await?;
        let info = handle.snapshot().await;
        let (signal_path, _) = handle.issue_signal_connect_path().await;
        Ok(WebSessionViewPage { info, signal_path })
    }

    pub async fn authorize_signal_access(
        &self,
        id: &str,
        token: Option<&str>,
    ) -> Result<(), WebSessionAccessError> {
        self.require_signal_access(id, token).await.map(|_| ())
    }

    async fn require_view_access(
        &self,
        id: &str,
        token: Option<&str>,
    ) -> Result<std::sync::Arc<super::WebSessionHandle>, WebSessionAccessError> {
        let provided_token = token.ok_or(WebSessionAccessError::MissingToken)?;
        let handle = self.get(id).await.ok_or(WebSessionAccessError::NotFound)?;
        if !handle.consume_view_token(provided_token).await {
            return Err(WebSessionAccessError::Unauthorized);
        }
        Ok(handle)
    }

    async fn require_signal_access(
        &self,
        id: &str,
        token: Option<&str>,
    ) -> Result<std::sync::Arc<super::WebSessionHandle>, WebSessionAccessError> {
        let provided_token = token.ok_or(WebSessionAccessError::MissingToken)?;
        let handle = self.get(id).await.ok_or(WebSessionAccessError::NotFound)?;
        if !handle.consume_signal_token(provided_token).await {
            return Err(WebSessionAccessError::Unauthorized);
        }
        Ok(handle)
    }
}
