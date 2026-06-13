use anyhow::Result;
use ctx_core::ids::SessionId;
use ctx_core::models::Session;
use ctx_session_title_service::title_generation::TitleGenerationOutcome;

use super::title_generation;
use crate::daemon::SessionTitleModelModeHandle;

#[derive(Debug)]
pub enum GenerateSessionTitleError {
    NotFound,
    PromptRequired,
    Skipped,
    Internal(anyhow::Error),
}

impl SessionTitleModelModeHandle {
    pub async fn generate_session_title_for_request(
        &self,
        session_id: SessionId,
        prompt: Option<String>,
        force: Option<bool>,
    ) -> Result<Session, GenerateSessionTitleError> {
        let store = self
            .session_store_or_none(session_id)
            .await
            .map_err(GenerateSessionTitleError::Internal)?
            .ok_or(GenerateSessionTitleError::NotFound)?;
        let session = store
            .get_session(session_id)
            .await
            .map_err(GenerateSessionTitleError::Internal)?
            .ok_or(GenerateSessionTitleError::NotFound)?;

        let prompt = if let Some(prompt) = prompt
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            prompt
        } else {
            store
                .get_first_user_message_content(session_id)
                .await
                .map_err(GenerateSessionTitleError::Internal)?
                .filter(|value| !value.trim().is_empty())
                .ok_or(GenerateSessionTitleError::PromptRequired)?
        };

        let force = force.unwrap_or(true);
        let cfg = self.configured_title_generation_settings().await;
        self.maybe_generate_session_title(session, prompt, force, cfg)
            .await
            .map_err(GenerateSessionTitleError::Internal)?
            .ok_or(GenerateSessionTitleError::Skipped)?;

        store
            .get_session(session_id)
            .await
            .map_err(GenerateSessionTitleError::Internal)?
            .ok_or(GenerateSessionTitleError::NotFound)
    }

    pub async fn configured_title_generation_settings(
        &self,
    ) -> Option<ctx_settings_model::TitleGenerationSettings> {
        title_generation::configured_title_generation_settings_for_store(self.global_store()).await
    }

    pub async fn maybe_generate_session_title(
        &self,
        session: Session,
        prompt: String,
        force: bool,
        cfg: Option<ctx_settings_model::TitleGenerationSettings>,
    ) -> anyhow::Result<Option<TitleGenerationOutcome>> {
        title_generation::maybe_generate_session_title_with_handle(
            self, session, prompt, force, cfg,
        )
        .await
    }

    pub async fn schedule_session_title_generation(
        &self,
        session: Session,
        prompt: String,
        force: bool,
    ) -> bool {
        let cfg = self.configured_title_generation_settings().await;
        if cfg.is_some() {
            let handle = self.clone();
            tokio::spawn(async move {
                let _ = handle
                    .maybe_generate_session_title(session, prompt, force, cfg)
                    .await;
            });
            true
        } else {
            let _ = self
                .maybe_generate_session_title(session, prompt, force, cfg)
                .await;
            false
        }
    }
}
