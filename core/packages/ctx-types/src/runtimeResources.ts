export type ResourceProcess = {
  label: string;
  pid: number;
  cpu_pct: number;
  memory_bytes: number;
  virtual_memory_bytes: number;
  child_count: number;
  children: ResourceChildProcess[];
  children_truncated: boolean;
};

export type ResourceChildProcess = {
  pid: number;
  parent_pid?: number | null;
  name: string;
  cmdline?: string | null;
  cpu_pct: number;
  memory_bytes: number;
  virtual_memory_bytes: number;
};

export type ResourceDisk = {
  name: string;
  mount_point: string;
  total_bytes: number;
  available_bytes: number;
  file_system: string;
};

export type ResourceWorktreeDisk = {
  worktree_id: string;
  root_path: string;
  size_bytes: number;
};

export type ResourceWorkspaceDisk = {
  workspace_id: string;
  root_path: string;
  size_bytes: number;
  size_collected_at: string;
  size_cache_age_ms: number;
  disk?: ResourceDisk | null;
  worktrees: ResourceWorktreeDisk[];
};

export type ResourceUtilization = {
  collected_at: string;
  cache_age_ms: number;
  system: {
    cpu_pct: number;
    memory_total_bytes: number;
    memory_used_bytes: number;
    swap_total_bytes: number;
    swap_used_bytes: number;
  };
  processes: {
    daemon?: ResourceProcess | null;
    providers: ResourceProcess[];
  };
  workspace: ResourceWorkspaceDisk;
};

export type TelemetryMetricKind = "histogram" | "counter" | "gauge";

export type TelemetryMetricSummary = {
  name: string;
  kind: TelemetryMetricKind;
  unit: string;
  labels: Record<string, string>;
  run_id?: string | null;
  count: number;
  sum: number;
  min?: number | null;
  max?: number | null;
  p50?: number | null;
  p95?: number | null;
  p99?: number | null;
};

export type TelemetrySummaryResponse = {
  generated_at: string;
  window_ms?: number | null;
  metrics: TelemetryMetricSummary[];
};

export type ClientTelemetryMetric = {
  name: string;
  kind: TelemetryMetricKind;
  unit: string;
  value: number;
  labels?: Record<string, string>;
  run_id?: string | null;
};

export type ClientTelemetryBatch = {
  events: ClientTelemetryMetric[];
};

export type SemanticTelemetryPlane = "product" | "incident";

export type SemanticTelemetryDelivery = "remote" | "local_only";

export type SemanticTelemetryOriginRuntime = "web" | "desktop" | "mobile_shell" | "daemon";

export type SemanticTelemetryScalar = string | number | boolean | null;

export type SemanticTelemetryProperties = Record<string, SemanticTelemetryScalar>;

export type SemanticTelemetryEvent = {
  event_id: string;
  event_name: string;
  event_version: number;
  occurred_at: string;
  plane: SemanticTelemetryPlane;
  delivery: SemanticTelemetryDelivery;
  origin_runtime: SemanticTelemetryOriginRuntime;
  origin_install_id: string;
  app_version: string;
  os: string;
  arch: string;
  surface?: "web" | "desktop" | "mobile_shell" | null;
  env_target?: "local" | "worktree" | "remote" | null;
  source?: string | null;
  properties?: SemanticTelemetryProperties;
};

export type SemanticTelemetryBatch = {
  events: SemanticTelemetryEvent[];
};

export type MobileConnectionProfile = {
  id: string;
  label: string;
  base_url: string;
  token_prefix: string;
  scopes: string[];
  created_at: string;
  last_used_at?: string | null;
};

export type MobileDeviceRegistration = {
  id: string;
  profile_id: string;
  device_label?: string | null;
  platform?: string | null;
  push_token?: string | null;
  push_provider?: string | null;
  public_key?: string | null;
  app_version?: string | null;
  created_at: string;
  last_seen_at: string;
};
