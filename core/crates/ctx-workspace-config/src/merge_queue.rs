use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeQueueCanonicalSync {
    Never,
    CleanOnly,
    Force,
}

#[derive(Debug, Clone)]
pub struct MergeQueueConfig {
    pub enabled: bool,
    pub target_branch: String,
    pub verify_commands: Vec<String>,
    pub push_on_success: bool,
    pub push_remote: String,
    pub push_branch: String,
    pub canonical_sync: MergeQueueCanonicalSync,
}

impl MergeQueueConfig {
    pub fn new_default() -> Self {
        Self {
            enabled: false,
            target_branch: "main".to_string(),
            verify_commands: Vec::new(),
            push_on_success: false,
            push_remote: "origin".to_string(),
            push_branch: "main".to_string(),
            canonical_sync: MergeQueueCanonicalSync::CleanOnly,
        }
    }
}

pub async fn load_merge_queue_config(store: &Store) -> Result<MergeQueueConfig> {
    let runtime_cfg = load_workspace_settings_doc(store).await?;
    let mut cfg = MergeQueueConfig::new_default();

    let Some(configured) = runtime_cfg.merge_queue else {
        return Ok(cfg);
    };

    if let Some(enabled) = configured.enabled {
        cfg.enabled = enabled;
    }
    if let Some(target_branch) = configured.target_branch {
        let trimmed = target_branch.trim().to_string();
        if !trimmed.is_empty() {
            cfg.target_branch = trimmed;
        }
    }
    if let Some(verify_commands) = configured.verify_commands {
        let trimmed = verify_commands
            .into_iter()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>();
        if !trimmed.is_empty() {
            cfg.verify_commands = trimmed;
        }
    }
    if let Some(push_on_success) = configured.push_on_success {
        cfg.push_on_success = push_on_success;
    }
    if let Some(push_remote) = configured.push_remote {
        let trimmed = push_remote.trim().to_string();
        if !trimmed.is_empty() {
            cfg.push_remote = trimmed;
        }
    }
    if let Some(push_branch) = configured.push_branch {
        let trimmed = push_branch.trim().to_string();
        cfg.push_branch = if trimmed.is_empty() {
            cfg.target_branch.clone()
        } else {
            trimmed
        };
    } else {
        cfg.push_branch = cfg.target_branch.clone();
    }
    if let Some(canonical_sync) = configured.canonical_sync {
        cfg.canonical_sync = canonical_sync;
    }

    Ok(cfg)
}

pub async fn load_merge_queue_target_branch_override(store: &Store) -> Result<Option<String>> {
    let runtime_cfg = load_workspace_settings_doc(store).await?;
    let configured = runtime_cfg.merge_queue.and_then(|mq| mq.target_branch);
    let Some(configured) = configured else {
        return Ok(None);
    };
    let trimmed = configured.trim().to_string();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed))
}

#[derive(Debug, Clone)]
pub struct MergeQueueConfigUpdate {
    pub enabled: bool,
    pub target_branch: Option<String>,
    pub verify_commands: Vec<String>,
    pub push_on_success: Option<bool>,
    pub push_remote: Option<String>,
    pub push_branch: Option<String>,
    pub canonical_sync: Option<MergeQueueCanonicalSync>,
}

impl MergeQueueConfigUpdate {
    pub fn normalized(mut self) -> Self {
        self.target_branch = self
            .target_branch
            .as_ref()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        self.verify_commands = self
            .verify_commands
            .into_iter()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();

        self.push_remote = self
            .push_remote
            .as_ref()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        self.push_branch = self
            .push_branch
            .as_ref()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeQueueConfigTransition {
    pub was_enabled: bool,
    pub now_enabled: bool,
}

pub async fn update_merge_queue_config(
    store: &Store,
    update: MergeQueueConfigUpdate,
) -> Result<()> {
    let update = update.normalized();
    mutate_workspace_settings_doc(store, "merge_queue", move |cfg| {
        if !update.enabled {
            cfg.merge_queue = None;
            return Ok(());
        }

        cfg.merge_queue = Some(WorkspaceMergeQueueConfig {
            enabled: Some(true),
            target_branch: update.target_branch,
            verify_commands: if update.verify_commands.is_empty() {
                None
            } else {
                Some(update.verify_commands)
            },
            push_on_success: update.push_on_success,
            push_remote: update.push_remote,
            push_branch: update.push_branch,
            canonical_sync: update.canonical_sync,
        });

        Ok(())
    })
    .await
}

pub async fn update_merge_queue_config_with_transition(
    store: &Store,
    update: MergeQueueConfigUpdate,
) -> Result<MergeQueueConfigTransition> {
    let was_enabled = load_merge_queue_config(store).await?.enabled;
    update_merge_queue_config(store, update).await?;
    let now_enabled = load_merge_queue_config(store).await?.enabled;
    Ok(MergeQueueConfigTransition {
        was_enabled,
        now_enabled,
    })
}
