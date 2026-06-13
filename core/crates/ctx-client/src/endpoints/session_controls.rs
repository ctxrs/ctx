use std::collections::HashMap;

use anyhow::Result;
use reqwest::Method;

use ctx_core::ids::SessionId;
use ctx_core::models::Session;

use crate::client::Client;
use crate::types::*;

impl Client {
    pub async fn cancel_session(&self, session_id: SessionId) -> Result<()> {
        let path = format!("/api/sessions/{}/cancel", session_id.0);
        self.request_empty(Method::POST, &path, None::<&()>).await
    }

    pub async fn interrupt_session(&self, session_id: SessionId) -> Result<()> {
        let path = format!("/api/sessions/{}/interrupt", session_id.0);
        self.request_empty(Method::POST, &path, None::<&()>).await
    }

    pub async fn set_session_model(
        &self,
        session_id: SessionId,
        model_id: &str,
    ) -> Result<Session> {
        let path = format!("/api/sessions/{}/model", session_id.0);
        let req = SetSessionModelRequest {
            model_id: model_id.to_string(),
        };
        self.request_json(Method::POST, &path, Some(&req)).await
    }

    pub async fn set_session_mode(&self, session_id: SessionId, mode_id: &str) -> Result<()> {
        let path = format!("/api/sessions/{}/mode", session_id.0);
        let req = SetSessionModeRequest {
            mode_id: mode_id.to_string(),
        };
        self.request_empty(Method::POST, &path, Some(&req)).await
    }

    pub async fn authenticate_session(
        &self,
        session_id: SessionId,
        method_id: Option<&str>,
    ) -> Result<()> {
        let path = format!("/api/sessions/{}/authenticate", session_id.0);
        let req = AuthenticateSessionRequest {
            method_id: method_id.map(|value| value.to_string()),
        };
        self.request_empty(Method::POST, &path, Some(&req)).await
    }

    pub async fn submit_ask_user_question(
        &self,
        session_id: SessionId,
        req: &AskUserQuestionRequest,
    ) -> Result<()> {
        let path = format!("/api/sessions/{}/ask_user_question", session_id.0);
        self.request_empty(Method::POST, &path, Some(req)).await
    }

    pub async fn ask_user_question(
        &self,
        session_id: SessionId,
        tool_call_id: &str,
        outcome: AskUserQuestionOutcome,
        answers: Option<HashMap<String, String>>,
    ) -> Result<()> {
        let req = AskUserQuestionRequest {
            tool_call_id: tool_call_id.to_string(),
            outcome,
            answers,
        };
        self.submit_ask_user_question(session_id, &req).await
    }
}
