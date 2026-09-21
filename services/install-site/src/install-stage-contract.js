export const INSTALL_STAGE_EVENT_NAME = "install_stage";
export const INSTALL_STAGE_EVENT_VERSION = 1;

export const INSTALL_STAGES = Object.freeze([
  "installer",
  "artifact_download",
  "binary_install",
  "skill_install",
  "setup",
  "uninstall",
]);

export const INSTALL_STAGE_STATUSES = Object.freeze([
  "started",
  "completed",
  "failed",
  "skipped",
]);

export const INSTALL_STAGE_STATUS_PAIRS = Object.freeze({
  installer: Object.freeze(["started", "completed", "failed"]),
  artifact_download: Object.freeze(["started", "completed"]),
  binary_install: Object.freeze(["completed"]),
  skill_install: Object.freeze(["started", "completed", "failed", "skipped"]),
  setup: Object.freeze(["started", "completed", "failed", "skipped"]),
  uninstall: Object.freeze(["started", "completed", "failed"]),
});

export const INSTALL_SCRIPT_FAMILIES = Object.freeze([
  "posix",
  "powershell",
]);

export const INSTALL_STAGE_PAYLOAD_KEYS = Object.freeze([
  "arch",
  "event_name",
  "event_version",
  "install_attempt_id",
  "platform",
  "script_family",
  "stage",
  "status",
]);
