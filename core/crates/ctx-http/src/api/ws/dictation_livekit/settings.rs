use axum::extract::ws::{Message as WsMessage, WebSocket};
use ctx_settings_service::DictationConfigError;
use ctx_transport_runtime::dictation_livekit::LiveKitDictationConfig;
use serde::Serialize;

use ctx_daemon::daemon::DictationHandle;

#[derive(Debug)]
pub(super) struct DictationStreamError {
    message: String,
    fallback_json: &'static str,
}

impl DictationStreamError {
    pub(super) fn new(message: impl Into<String>, fallback_json: &'static str) -> Self {
        Self {
            message: message.into(),
            fallback_json,
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorMsg {
    r#type: &'static str,
    message: String,
}

pub(super) async fn send_dictation_error(socket: &mut WebSocket, error: DictationStreamError) {
    let _ = socket
        .send(WsMessage::Text(
            serde_json::to_string(&ErrorMsg {
                r#type: "error",
                message: error.message,
            })
            .unwrap_or_else(|_| error.fallback_json.to_string()),
        ))
        .await;
}

pub(super) async fn load_livekit_dictation_config(
    state: &DictationHandle,
) -> Result<LiveKitDictationConfig, DictationStreamError> {
    state
        .resolve_livekit_dictation_config()
        .await
        .map_err(dictation_stream_error_for_config_error)
}

fn dictation_stream_error_for_config_error(error: DictationConfigError) -> DictationStreamError {
    match error {
        DictationConfigError::Unavailable { message } => DictationStreamError::new(
            format!("Failed to load dictation settings: {message}"),
            "{\"type\":\"error\",\"message\":\"dictation unavailable\"}",
        ),
        DictationConfigError::NotConfigured => DictationStreamError::new(
            "Dictation settings not configured.",
            "{\"type\":\"error\",\"message\":\"dictation unavailable\"}",
        ),
        DictationConfigError::Disabled => DictationStreamError::new(
            "Dictation is disabled.",
            "{\"type\":\"error\",\"message\":\"dictation disabled\"}",
        ),
        DictationConfigError::MissingLiveKitConfig => DictationStreamError::new(
            "LiveKit dictation settings not configured.",
            "{\"type\":\"error\",\"message\":\"missing livekit config\"}",
        ),
        DictationConfigError::InvalidLiveKitConfig { message } => DictationStreamError::new(
            message,
            "{\"type\":\"error\",\"message\":\"missing credentials\"}",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapped_error(error: DictationConfigError) -> DictationStreamError {
        dictation_stream_error_for_config_error(error)
    }

    #[test]
    fn dictation_config_error_mapping_preserves_current_messages() {
        let error = mapped_error(DictationConfigError::Unavailable {
            message: "database offline".to_string(),
        });
        assert_eq!(
            error.message,
            "Failed to load dictation settings: database offline"
        );
        assert_eq!(
            error.fallback_json,
            "{\"type\":\"error\",\"message\":\"dictation unavailable\"}"
        );

        let error = mapped_error(DictationConfigError::NotConfigured);
        assert_eq!(error.message, "Dictation settings not configured.");
        assert_eq!(
            error.fallback_json,
            "{\"type\":\"error\",\"message\":\"dictation unavailable\"}"
        );

        let error = mapped_error(DictationConfigError::Disabled);
        assert_eq!(error.message, "Dictation is disabled.");
        assert_eq!(
            error.fallback_json,
            "{\"type\":\"error\",\"message\":\"dictation disabled\"}"
        );

        let error = mapped_error(DictationConfigError::MissingLiveKitConfig);
        assert_eq!(error.message, "LiveKit dictation settings not configured.");
        assert_eq!(
            error.fallback_json,
            "{\"type\":\"error\",\"message\":\"missing livekit config\"}"
        );

        let error = mapped_error(DictationConfigError::InvalidLiveKitConfig {
            message: "missing credentials".to_string(),
        });
        assert_eq!(error.message, "missing credentials");
        assert_eq!(
            error.fallback_json,
            "{\"type\":\"error\",\"message\":\"missing credentials\"}"
        );
    }
}
