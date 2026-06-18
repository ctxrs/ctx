export type PluginManifest = {
  schema_version?: number;
  id: string;
  name: string;
  version: string;
  description?: string | null;
  entrypoints?: PluginEntrypoint[];
  contributes?: PluginContributions;
  compatibility?: PluginCompatibility;
};

export type PluginEntrypointKind = "process" | "worker" | "webview";

export type PluginEntrypoint = {
  id: string;
  kind?: PluginEntrypointKind;
  command: string;
  args?: string[];
  cwd?: string | null;
  environment?: Record<string, string>;
};

export type PluginContributions = {
  providers?: PluginProviderContribution[];
  runtimes?: PluginRuntimeContribution[];
  commands?: PluginCommandContribution[];
  collectors?: PluginCollectorContribution[];
  observers?: PluginObserverContribution[];
  ui_surfaces?: PluginUiSurfaceContribution[];
};

export type PluginProviderContribution = {
  id: string;
  name: string;
  description?: string | null;
  entrypoint?: string | null;
  capabilities?: string[];
};

export type PluginRuntimeContribution = {
  id: string;
  name: string;
  description?: string | null;
  entrypoint?: string | null;
  capabilities?: string[];
};

export type PluginCommandContribution = {
  id: string;
  title: string;
  description?: string | null;
  category?: string | null;
  entrypoint?: string | null;
};

export type PluginCollectorContribution = {
  id: string;
  name: string;
  description?: string | null;
  entrypoint?: string | null;
  events?: string[];
};

export type PluginObserverContribution = {
  id: string;
  name: string;
  description?: string | null;
  entrypoint?: string | null;
  events?: string[];
};

export type PluginUiSurfaceKind =
  | "panel"
  | "sidebar"
  | "status_bar"
  | "command_palette"
  | "settings";

export type PluginUiSurfaceContribution = {
  id: string;
  name: string;
  surface: PluginUiSurfaceKind;
  description?: string | null;
  entrypoint?: string | null;
  contexts?: string[];
};

export type PluginCompatibility = {
  min_ctx_version?: string | null;
  capabilities?: string[];
};

export type PluginEnablement = "enabled" | "disabled";

export type PluginLoadStatus = "not_loaded" | "loaded" | "error";

export type PluginDiagnosticSeverity = "info" | "warning" | "error";

export type PluginDiagnostic = {
  severity: PluginDiagnosticSeverity;
  message: string;
  code?: string | null;
};

export type PluginInventoryItem = {
  id: string;
  name: string;
  version: string;
  enabled: PluginEnablement;
  status: PluginLoadStatus;
  path: string;
  diagnostics?: PluginDiagnostic[];
  last_loaded_at?: string | null;
  revision?: string | null;
  manifest?: PluginManifest | null;
};

export type PluginContributionRegistration<TContribution> = {
  plugin_id: string;
  plugin_name: string;
  plugin_version: string;
  plugin_path: string;
  plugin_revision?: string | null;
  contribution: TContribution;
};

export type PluginExtensionRegistry = {
  revision: number;
  providers?: PluginContributionRegistration<PluginProviderContribution>[];
  runtimes?: PluginContributionRegistration<PluginRuntimeContribution>[];
  commands?: PluginContributionRegistration<PluginCommandContribution>[];
  collectors?: PluginContributionRegistration<PluginCollectorContribution>[];
  observers?: PluginContributionRegistration<PluginObserverContribution>[];
  ui_surfaces?: PluginContributionRegistration<PluginUiSurfaceContribution>[];
};

export type PluginCommandExecutionRequest = {
  plugin_id: string;
  command_id: string;
  input?: string | null;
  workspace_id?: string | null;
  task_id?: string | null;
  session_id?: string | null;
};

export type PluginCommandExecutionStatus = "completed" | "failed";

export type PluginCommandExecutionResponse = {
  plugin_id: string;
  command_id: string;
  status: PluginCommandExecutionStatus;
  message?: string | null;
  error?: string | null;
  stdout?: string;
  stderr?: string;
  exit_code?: number | null;
};
