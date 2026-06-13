export type ProviderStatus = {
  provider_id: string;
  installed: boolean;
  detected_path?: string | null;
  version?: string | null;
  health: string;
  diagnostics: string[];
  details?: Record<string, string>;
  usability: ProviderUsability;
};

export type ProviderUsabilityStatus = "ready" | "installable" | "blocked" | "unsupported";

export type ProviderRecommendedAction =
  | "none"
  | "install"
  | "resolve_dependency"
  | "configure_runtime"
  | "switch_target";

export type ProviderUsability = {
  usable: boolean;
  status: ProviderUsabilityStatus;
  reason_code?: string | null;
  reason?: string | null;
  blocking_provider_ids: string[];
  recommended_action: ProviderRecommendedAction;
};

export type InstallEventLevel = "info" | "warning" | "error" | "success";

export type InstallProgressEvent = {
  install_id: string;
  provider_id: string;
  at: string;
  stage: string;
  message: string;
  level: InstallEventLevel;
  bytes?: number;
  total_bytes?: number;
  attempt?: number;
};

export type Diagnostics = {
  daemon: {
    version: string;
    daemon_version: string;
    pid: number;
    data_root: string;
    daemon_url: string;
    auth_required: boolean;
    storage?: {
      level: "normal" | "warning" | "emergency";
      warning_threshold_bytes: number;
      emergency_threshold_bytes: number;
      reserve_bytes: number;
      reserve_file_active: boolean;
      updated_at: string;
      active?: {
        label: string;
        path: string;
        mount_point: string;
        free_bytes: number;
        total_bytes: number;
      } | null;
    } | null;
    compatibility: {
      desktop_exact_version: string;
      mobile_api_min: number;
      mobile_api_max: number;
    };
  };
  platform: { os: string; arch: string };
  logs: { dir: string; files: { name: string; bytes: number; modified_utc?: string | null }[] };
  providers: ProviderStatus[];
  managed_installs: Record<string, unknown>;
};
