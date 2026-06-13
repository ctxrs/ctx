use super::*;

impl<'a> ProviderRuntimeContext<'a> {
    pub(super) fn runtime_data_root(&self) -> &'a Path {
        self.runtime_data_root.unwrap_or(self.data_root)
    }

    pub(super) fn subscription_env(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();
        if self.canonical == PROVIDER_AMP {
            let home = amp_subscription_home(self.data_root, self.runtime_data_root);
            env.insert("HOME".to_string(), home.to_string_lossy().to_string());
            env.insert(
                "XDG_CONFIG_HOME".to_string(),
                home.join(".config").to_string_lossy().to_string(),
            );
            env.insert(
                "XDG_CACHE_HOME".to_string(),
                home.join(".cache").to_string_lossy().to_string(),
            );
        }
        if self.canonical == PROVIDER_GOOSE {
            let path_root = goose_subscription_path_root(self.data_root, self.runtime_data_root);
            env.insert(
                "GOOSE_PATH_ROOT".to_string(),
                path_root.to_string_lossy().to_string(),
            );
        }
        env
    }

    pub(in super::super) async fn cleanup_endpoint_runtime(&self, endpoint_id: &str) -> Result<()> {
        let Some(endpoint_homes) = (match self.canonical {
            PROVIDER_CODEX => Some(vec![
                codex_endpoint_home(self.data_root, endpoint_id),
                legacy_codex_endpoint_home(self.data_root, endpoint_id),
            ]),
            PROVIDER_CLINE => Some(vec![cline_endpoint_home(self.data_root, endpoint_id)]),
            PROVIDER_GOOSE => Some(vec![goose_endpoint_path_root(self.data_root, endpoint_id)]),
            PROVIDER_KIMI => Some(vec![kimi_endpoint_home(self.data_root, endpoint_id)]),
            PROVIDER_QWEN => Some(vec![qwen_endpoint_home(self.data_root, endpoint_id)]),
            PROVIDER_GEMINI => Some(vec![gemini_endpoint_home(self.data_root, endpoint_id)]),
            PROVIDER_DROID => Some(vec![droid_endpoint_home(self.data_root, endpoint_id)]),
            PROVIDER_OPENHANDS => Some(vec![provider_fs::openhands_endpoint_home(
                self.data_root,
                endpoint_id,
            )]),
            _ => None,
        }) else {
            return Ok(());
        };

        validation::ensure_safe_endpoint_id(endpoint_id)?;
        for endpoint_home in endpoint_homes {
            match tokio::fs::remove_dir_all(&endpoint_home).await {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!(
                            "removing {} endpoint home for endpoint {}",
                            self.canonical, endpoint_id
                        )
                    });
                }
            }
        }
        Ok(())
    }
}
