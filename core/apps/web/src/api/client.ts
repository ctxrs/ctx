export * from "./clientTypes";
export {
  primeDaemonConnection,
  authToken,
  getDaemonClientConfig,
  subscribeDaemonConfig,
  setDaemonBaseUrl,
  setDaemonAuthToken,
  applyDaemonDesktopConnection,
  syncDesktopDaemonConnectionFromBridge,
  resetDaemonConnection,
  daemonFetchRaw,
  idToString,
  recordClientCounterMetric,
  recordClientGaugeMetric,
  recordClientHistogramMetric,
  recordSemanticTelemetryEvent,
  setSemanticTelemetryRemoteEnabled,
} from "./clientBase";
export type {
  DaemonRawResponse,
  DaemonClientConfig,
  DesktopDaemonConnectionSyncResult,
} from "./clientBase";
export {
  getDaemonConnection,
  getDaemonConnectionReadiness,
  hasReadyDaemonConnection,
  subscribeDaemonConnection,
  setDaemonConnection,
  clearDaemonConnection,
  normalizeDaemonBaseUrl,
  normalizeDaemonWsBaseUrl,
  deriveDaemonWsBaseUrl,
  getDaemonWsUrl,
  getDaemonHttpUrl,
} from "./daemonConnection";
export type {
  DaemonConnection,
  DaemonConnectionReadiness,
  DaemonConnectionUpdate,
  SetDaemonConnectionOptions,
} from "./daemonConnection";
export * from "./clientWorkspaces";
export * from "./clientSessions";
export * from "./clientProviders";
export * from "./clientSystem";
export * from "./clientMobile";
export * from "./clientRepo";
