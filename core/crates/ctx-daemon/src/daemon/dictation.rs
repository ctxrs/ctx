use ctx_settings_model::{DictationProvider, Settings};
use ctx_settings_service::DictationConfigError;
use ctx_transport_runtime::dictation_livekit::{
    normalize_livekit_dictation_config, LiveKitDictationConfig, LiveKitDictationConfigInput,
};

use ctx_store::Store;

use crate::daemon::DictationHandle;

async fn resolve_livekit_dictation_config_from_store(
    store: &Store,
) -> Result<LiveKitDictationConfig, DictationConfigError> {
    let settings = ctx_settings_service::load_settings(store)
        .await
        .map_err(|error| DictationConfigError::Unavailable {
            message: error.to_string(),
        })?;
    livekit_dictation_config_from_settings(settings)
}

impl DictationHandle {
    pub async fn resolve_livekit_dictation_config(
        &self,
    ) -> Result<LiveKitDictationConfig, DictationConfigError> {
        resolve_livekit_dictation_config_from_store(self.store()).await
    }
}

fn livekit_dictation_config_from_settings(
    settings: Settings,
) -> Result<LiveKitDictationConfig, DictationConfigError> {
    let Some(dictation) = settings.dictation else {
        return Err(DictationConfigError::NotConfigured);
    };

    if !dictation.enabled || !matches!(dictation.provider, DictationProvider::LiveKitInference) {
        return Err(DictationConfigError::Disabled);
    }

    let Some(cfg) = dictation.livekit else {
        return Err(DictationConfigError::MissingLiveKitConfig);
    };

    normalize_livekit_dictation_config(LiveKitDictationConfigInput {
        api_key: cfg.api_key,
        api_secret: cfg.api_secret,
        base_url: cfg.base_url,
        model: cfg.model,
        language: cfg.language,
    })
    .map_err(|error| DictationConfigError::InvalidLiveKitConfig {
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_settings_model::{DictationSettings, LiveKitDictationSettings};

    fn settings_with_dictation(dictation: Option<DictationSettings>) -> Settings {
        Settings {
            dictation,
            ..Settings::default()
        }
    }

    fn valid_livekit_settings() -> LiveKitDictationSettings {
        LiveKitDictationSettings {
            api_key: "api-key".to_string(),
            api_secret: Some("api-secret".to_string()),
            base_url: "https://example.test/v1".to_string(),
            model: "model".to_string(),
            language: "en".to_string(),
        }
    }

    #[test]
    fn livekit_dictation_config_requires_dictation_settings() {
        let error = livekit_dictation_config_from_settings(settings_with_dictation(None))
            .expect_err("missing dictation settings should fail");

        assert!(matches!(error, DictationConfigError::NotConfigured));
    }

    #[test]
    fn livekit_dictation_config_rejects_disabled_or_non_livekit_provider() {
        let mut disabled = DictationSettings {
            enabled: false,
            provider: DictationProvider::LiveKitInference,
            livekit: Some(valid_livekit_settings()),
        };
        let error =
            livekit_dictation_config_from_settings(settings_with_dictation(Some(disabled.clone())))
                .expect_err("disabled dictation should fail");
        assert!(matches!(error, DictationConfigError::Disabled));

        disabled.enabled = true;
        disabled.provider = DictationProvider::TauriStt;
        let error = livekit_dictation_config_from_settings(settings_with_dictation(Some(disabled)))
            .expect_err("non-livekit provider should fail");
        assert!(matches!(error, DictationConfigError::Disabled));
    }

    #[test]
    fn livekit_dictation_config_requires_livekit_block() {
        let error = livekit_dictation_config_from_settings(settings_with_dictation(Some(
            DictationSettings {
                enabled: true,
                provider: DictationProvider::LiveKitInference,
                livekit: None,
            },
        )))
        .expect_err("missing livekit block should fail");

        assert!(matches!(error, DictationConfigError::MissingLiveKitConfig));
    }

    #[test]
    fn livekit_dictation_config_preserves_invalid_config_message() {
        let error = livekit_dictation_config_from_settings(settings_with_dictation(Some(
            DictationSettings {
                enabled: true,
                provider: DictationProvider::LiveKitInference,
                livekit: Some(LiveKitDictationSettings {
                    api_key: String::new(),
                    api_secret: None,
                    base_url: String::new(),
                    model: "model".to_string(),
                    language: "en".to_string(),
                }),
            },
        )))
        .expect_err("missing credentials should fail");

        match error {
            DictationConfigError::InvalidLiveKitConfig { message } => {
                assert!(message.contains("LiveKit API credentials missing"));
            }
            other => panic!("expected invalid config, got {other:?}"),
        }
    }

    #[test]
    fn livekit_dictation_config_normalizes_valid_settings() {
        let config = livekit_dictation_config_from_settings(settings_with_dictation(Some(
            DictationSettings {
                enabled: true,
                provider: DictationProvider::LiveKitInference,
                livekit: Some(valid_livekit_settings()),
            },
        )))
        .expect("valid livekit dictation settings should normalize");

        assert_eq!(config.api_key, "api-key");
        assert_eq!(config.api_secret, "api-secret");
        assert_eq!(config.base_url, "https://example.test/v1");
        assert_eq!(config.model, "model");
        assert_eq!(config.language, "en");
    }
}
