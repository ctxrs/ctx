export const PROMPT_SNOOZE_STORAGE_KEY = "ctx_update_prompt_next_allowed_at_v1";
export const IDLE_UPDATE_VERSION_STORAGE_KEY = "ctx_update_prompt_idle_versions_v1";
export const RESTART_REQUIRED_VERSION_STORAGE_KEY = "ctx_update_restart_required_version_v1";
export const RESTART_READY_DISMISSED_VERSION_STORAGE_KEY =
  "ctx_update_restart_ready_dismissed_version_v1";

export const POLL_INTERVAL_MS = 60 * 60 * 1000;
export const PROMPT_SNOOZE_MS = 24 * 60 * 60 * 1000;
export const RESTART_READY_MESSAGE =
  "Update takes ~1 second and preserves data. Active agents will be paused.";
