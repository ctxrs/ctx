// Leaf vocabularies: no parser imports or initialization cycles.
export const HOSTED_OPERATIONS = new Set([
  "archive_export", "archive_restore", "archive_verify", "remote_connect", "remote_share", "remote_sync",
  "remote_pause", "remote_resume", "remote_status", "remote_remove",
  "server_init", "server_invite", "server_grant", "server_revoke", "server_withdraw",
  "server_backup", "server_restore", "server_collection_create", "server_user_list", "server_user_credentials",
  "server_user_create", "server_user_credential", "server_publications", "server_status",
]);
export const HOSTED_MEASUREMENT_KEYS = new Set(["native_total_duration_bucket", "prepare_duration_bucket", "work_duration_bucket", "output_duration_bucket", "result_count_bucket", "result_empty", "result_truncated", "output_delivery"]);
