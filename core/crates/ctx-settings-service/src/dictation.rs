#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictationConfigError {
    Unavailable { message: String },
    NotConfigured,
    Disabled,
    MissingLiveKitConfig,
    InvalidLiveKitConfig { message: String },
}

#[cfg(test)]
mod tests {
    use super::DictationConfigError;

    #[test]
    fn dictation_config_error_preserves_unavailable_message() {
        let error = DictationConfigError::Unavailable {
            message: "database offline".to_string(),
        };

        assert_eq!(
            error,
            DictationConfigError::Unavailable {
                message: "database offline".to_string()
            }
        );
    }
}
