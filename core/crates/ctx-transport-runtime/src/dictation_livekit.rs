use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures::SinkExt;
use jsonwebtoken::{EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as TMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

pub const DEFAULT_LIVEKIT_INFERENCE_BASE_URL: &str = "https://agent-gateway.livekit.cloud/v1";

#[derive(Debug, Clone)]
pub struct LiveKitDictationConfig {
    pub api_key: String,
    pub api_secret: String,
    pub base_url: String,
    pub model: String,
    pub language: String,
}

impl LiveKitDictationConfig {
    pub fn normalized_base_url(&self) -> &str {
        let base_url = self.base_url.trim();
        if base_url.is_empty() {
            DEFAULT_LIVEKIT_INFERENCE_BASE_URL
        } else {
            base_url
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveKitDictationConfigInput {
    pub api_key: String,
    pub api_secret: Option<String>,
    pub base_url: String,
    pub model: String,
    pub language: String,
}

pub fn normalize_livekit_dictation_config(
    input: LiveKitDictationConfigInput,
) -> anyhow::Result<LiveKitDictationConfig> {
    let api_key = input.api_key.trim().to_string();
    let api_secret = input.api_secret.unwrap_or_default().trim().to_string();
    anyhow::ensure!(
        !api_key.is_empty() && !api_secret.is_empty(),
        "LiveKit API credentials missing. Configure them in Settings."
    );

    Ok(LiveKitDictationConfig {
        api_key,
        api_secret,
        base_url: input.base_url,
        model: input.model,
        language: input.language,
    })
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LiveKitDictationClientControl {
    Stop,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum LiveKitDictationUpstreamEvent {
    InterimTranscript { payload: String },
    FinalTranscript { payload: String },
    Error { payload: String },
    SessionFinalized,
    SessionClosed,
    Ignore,
}

pub fn livekit_client_control_requests_stop(text: &str) -> bool {
    matches!(
        serde_json::from_str::<LiveKitDictationClientControl>(text),
        Ok(LiveKitDictationClientControl::Stop)
    )
}

pub fn livekit_dictation_input_audio_payload(audio: &[u8]) -> String {
    json!({ "type": "input_audio", "audio": BASE64.encode(audio) }).to_string()
}

pub fn livekit_dictation_finalize_payload() -> String {
    json!({ "type": "session.finalize" }).to_string()
}

pub fn translate_livekit_dictation_text_message(text: &str) -> LiveKitDictationUpstreamEvent {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return LiveKitDictationUpstreamEvent::Ignore;
    };
    match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "interim_transcript" | "final_transcript" => {
            let payload = json!({
                "type": if v.get("type").and_then(|t| t.as_str()) == Some("final_transcript") {
                    "final"
                } else {
                    "interim"
                },
                "text": v.get("transcript").and_then(|t| t.as_str()).unwrap_or(""),
                "language": v.get("language").and_then(|t| t.as_str()).unwrap_or(""),
            })
            .to_string();
            if v.get("type").and_then(|t| t.as_str()) == Some("final_transcript") {
                LiveKitDictationUpstreamEvent::FinalTranscript { payload }
            } else {
                LiveKitDictationUpstreamEvent::InterimTranscript { payload }
            }
        }
        "session.finalized" => LiveKitDictationUpstreamEvent::SessionFinalized,
        "session.closed" => LiveKitDictationUpstreamEvent::SessionClosed,
        "error" => LiveKitDictationUpstreamEvent::Error {
            payload: json!({
                "type": "error",
                "message": v.get("message").and_then(|t| t.as_str()).unwrap_or("LiveKit STT error"),
            })
            .to_string(),
        },
        _ => LiveKitDictationUpstreamEvent::Ignore,
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct LiveKitInferenceClaims {
    iss: String,
    sub: String,
    nbf: usize,
    exp: usize,
    inference: LiveKitInferenceGrant,
}

#[derive(Debug, Serialize, Deserialize)]
struct LiveKitInferenceGrant {
    perform: bool,
}

fn make_inference_token(api_key: &str, api_secret: &str) -> anyhow::Result<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as usize;
    let exp = now + Duration::from_secs(600).as_secs() as usize;
    let claims = LiveKitInferenceClaims {
        iss: api_key.to_string(),
        sub: "agent".to_string(),
        nbf: now,
        exp,
        inference: LiveKitInferenceGrant { perform: true },
    };

    Ok(jsonwebtoken::encode(
        &Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(api_secret.as_bytes()),
    )?)
}

fn ws_url_for_inference(base_url: &str) -> anyhow::Result<Url> {
    let base_url = base_url.trim().trim_end_matches('/');
    let mut url = Url::parse(base_url).context("invalid base_url")?;
    match url.scheme() {
        "http" => {
            url.set_scheme("ws")
                .map_err(|_| anyhow::anyhow!("failed to set ws scheme"))?;
        }
        "https" => {
            url.set_scheme("wss")
                .map_err(|_| anyhow::anyhow!("failed to set wss scheme"))?;
        }
        "ws" | "wss" => {}
        other => anyhow::bail!("unsupported base_url scheme: {other}"),
    };
    url.set_path(&format!("{}/stt", url.path().trim_end_matches('/')));
    Ok(url)
}

fn normalize_model_id(model: &str) -> String {
    let m = model.trim();
    if m.is_empty() || m.eq_ignore_ascii_case("auto") {
        // LiveKit Inference's STT WebSocket requires an explicit model id.
        return "deepgram/nova-3".to_string();
    }
    if m.eq_ignore_ascii_case("elevenlabs/scribe-v2-realtime") {
        return "elevenlabs/scribe_v2_realtime".to_string();
    }
    if m.eq_ignore_ascii_case("deepgram/flux") {
        return "deepgram/flux-general".to_string();
    }
    m.to_string()
}

pub async fn connect_livekit_inference_stt(
    cfg: &LiveKitDictationConfig,
) -> anyhow::Result<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>> {
    let token = make_inference_token(&cfg.api_key, &cfg.api_secret)?;

    let url = ws_url_for_inference(cfg.normalized_base_url())?;
    let mut req = url.as_str().into_client_request()?;
    req.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))?,
    );

    let (mut ws, _) = tokio_tungstenite::connect_async(req).await?;

    let mut settings = serde_json::Map::new();
    settings.insert(
        "sample_rate".to_string(),
        serde_json::Value::String("16000".to_string()),
    );
    settings.insert(
        "encoding".to_string(),
        serde_json::Value::String("pcm_s16le".to_string()),
    );
    settings.insert(
        "extra".to_string(),
        serde_json::Value::Object(serde_json::Map::new()),
    );
    if !cfg.language.trim().is_empty() {
        settings.insert(
            "language".to_string(),
            serde_json::Value::String(cfg.language.trim().to_string()),
        );
    }

    let mut session_create = serde_json::Map::new();
    session_create.insert(
        "type".to_string(),
        serde_json::Value::String("session.create".to_string()),
    );
    session_create.insert("settings".to_string(), serde_json::Value::Object(settings));
    session_create.insert(
        "model".to_string(),
        serde_json::Value::String(normalize_model_id(&cfg.model)),
    );

    ws.send(TMessage::Text(
        serde_json::Value::Object(session_create).to_string().into(),
    ))
    .await?;
    // tungstenite 0.26 uses an internal Utf8Bytes type.

    Ok(ws)
}

#[cfg(test)]
mod tests {
    use jsonwebtoken::{decode, DecodingKey, Validation};

    use super::*;

    #[test]
    fn inference_url_normalizes_http_schemes_and_appends_stt() {
        assert_eq!(
            ws_url_for_inference("https://agent-gateway.livekit.cloud/v1")
                .unwrap()
                .as_str(),
            "wss://agent-gateway.livekit.cloud/v1/stt"
        );
        assert_eq!(
            ws_url_for_inference("http://127.0.0.1:9000/base/")
                .unwrap()
                .as_str(),
            "ws://127.0.0.1:9000/base/stt"
        );
        assert_eq!(
            ws_url_for_inference("wss://example.test").unwrap().as_str(),
            "wss://example.test/stt"
        );
    }

    #[test]
    fn inference_url_rejects_non_websocket_schemes() {
        let err = ws_url_for_inference("file:///tmp/livekit")
            .expect_err("file URLs must not become STT websocket endpoints");
        assert!(err.to_string().contains("unsupported base_url scheme"));
    }

    #[test]
    fn model_normalization_matches_livekit_inference_names() {
        assert_eq!(normalize_model_id(""), "deepgram/nova-3");
        assert_eq!(normalize_model_id("auto"), "deepgram/nova-3");
        assert_eq!(
            normalize_model_id("elevenlabs/scribe-v2-realtime"),
            "elevenlabs/scribe_v2_realtime"
        );
        assert_eq!(normalize_model_id("deepgram/flux"), "deepgram/flux-general");
        assert_eq!(normalize_model_id("custom/model"), "custom/model");
    }

    #[test]
    fn inference_token_grants_livekit_inference_access() {
        let token = make_inference_token("lk-api-key", "lk-secret").unwrap();
        let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_exp = false;
        validation.validate_nbf = false;
        let claims = decode::<LiveKitInferenceClaims>(
            &token,
            &DecodingKey::from_secret("lk-secret".as_bytes()),
            &validation,
        )
        .unwrap()
        .claims;

        assert_eq!(claims.iss, "lk-api-key");
        assert_eq!(claims.sub, "agent");
        assert!(claims.inference.perform);
        assert!(claims.exp > claims.nbf);
    }

    #[test]
    fn blank_config_base_url_uses_livekit_default() {
        let cfg = LiveKitDictationConfig {
            api_key: "key".to_string(),
            api_secret: "secret".to_string(),
            base_url: "  ".to_string(),
            model: "auto".to_string(),
            language: String::new(),
        };

        assert_eq!(
            cfg.normalized_base_url(),
            DEFAULT_LIVEKIT_INFERENCE_BASE_URL
        );
    }

    #[test]
    fn livekit_dictation_config_normalization_requires_credentials() {
        let error = normalize_livekit_dictation_config(LiveKitDictationConfigInput {
            api_key: "  ".to_string(),
            api_secret: Some("secret".to_string()),
            base_url: String::new(),
            model: "auto".to_string(),
            language: String::new(),
        })
        .expect_err("api key required");
        assert!(error
            .to_string()
            .contains("LiveKit API credentials missing"));

        let error = normalize_livekit_dictation_config(LiveKitDictationConfigInput {
            api_key: "key".to_string(),
            api_secret: None,
            base_url: String::new(),
            model: "auto".to_string(),
            language: String::new(),
        })
        .expect_err("api secret required");
        assert!(error
            .to_string()
            .contains("LiveKit API credentials missing"));
    }

    #[test]
    fn livekit_dictation_config_normalization_trims_credentials() {
        let cfg = normalize_livekit_dictation_config(LiveKitDictationConfigInput {
            api_key: " key ".to_string(),
            api_secret: Some(" secret ".to_string()),
            base_url: "https://example.test".to_string(),
            model: "deepgram/nova-3".to_string(),
            language: "en".to_string(),
        })
        .expect("config");

        assert_eq!(cfg.api_key, "key");
        assert_eq!(cfg.api_secret, "secret");
        assert_eq!(cfg.base_url, "https://example.test");
        assert_eq!(cfg.model, "deepgram/nova-3");
        assert_eq!(cfg.language, "en");
    }

    #[test]
    fn livekit_dictation_client_control_detects_stop() {
        assert!(livekit_client_control_requests_stop(r#"{"type":"stop"}"#));
        assert!(!livekit_client_control_requests_stop(r#"{"type":"noop"}"#));
        assert!(!livekit_client_control_requests_stop("not json"));
    }

    #[test]
    fn livekit_dictation_input_audio_payload_encodes_audio() {
        let payload = livekit_dictation_input_audio_payload(b"abc");
        let value = serde_json::from_str::<serde_json::Value>(&payload).expect("json");
        assert_eq!(
            value.get("type").and_then(|v| v.as_str()),
            Some("input_audio")
        );
        assert_eq!(value.get("audio").and_then(|v| v.as_str()), Some("YWJj"));
    }

    #[test]
    fn livekit_dictation_finalize_payload_requests_session_finalize() {
        let payload = livekit_dictation_finalize_payload();
        let value = serde_json::from_str::<serde_json::Value>(&payload).expect("json");
        assert_eq!(
            value.get("type").and_then(|v| v.as_str()),
            Some("session.finalize")
        );
    }

    #[test]
    fn livekit_dictation_upstream_translation_maps_transcripts_and_terminal_events() {
        assert_eq!(
            translate_livekit_dictation_text_message(
                r#"{"type":"interim_transcript","transcript":"hel","language":"en"}"#
            ),
            LiveKitDictationUpstreamEvent::InterimTranscript {
                payload: r#"{"language":"en","text":"hel","type":"interim"}"#.to_string()
            }
        );
        assert_eq!(
            translate_livekit_dictation_text_message(
                r#"{"type":"final_transcript","transcript":"hello","language":"en"}"#
            ),
            LiveKitDictationUpstreamEvent::FinalTranscript {
                payload: r#"{"language":"en","text":"hello","type":"final"}"#.to_string()
            }
        );
        assert_eq!(
            translate_livekit_dictation_text_message(r#"{"type":"session.finalized"}"#),
            LiveKitDictationUpstreamEvent::SessionFinalized
        );
        assert_eq!(
            translate_livekit_dictation_text_message(r#"{"type":"session.closed"}"#),
            LiveKitDictationUpstreamEvent::SessionClosed
        );
    }

    #[test]
    fn livekit_dictation_upstream_translation_maps_errors_and_ignores_unknown() {
        assert_eq!(
            translate_livekit_dictation_text_message(r#"{"type":"error","message":"bad"}"#),
            LiveKitDictationUpstreamEvent::Error {
                payload: r#"{"message":"bad","type":"error"}"#.to_string()
            }
        );
        assert_eq!(
            translate_livekit_dictation_text_message(r#"{"type":"unknown"}"#),
            LiveKitDictationUpstreamEvent::Ignore
        );
        assert_eq!(
            translate_livekit_dictation_text_message("not json"),
            LiveKitDictationUpstreamEvent::Ignore
        );
    }
}
