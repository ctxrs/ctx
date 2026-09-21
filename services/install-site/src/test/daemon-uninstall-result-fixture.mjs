export const VERIFIED_DAEMON_UNINSTALL_RESULT = Object.freeze({
  schema_version: 1,
  command: "daemon_prepare_uninstall",
  ok: true,
  scope: "installation",
  requested_data_root: "/ctx/test/requested-root",
  canonical_data_root: "/ctx/test/canonical-root",
  quiesced_roots: [
    "/ctx/test/canonical-root",
    "/ctx/test/requested-root",
  ],
  quiesced_root_count: 2,
  installation_quiescent: true,
  daemon_enabled: false,
  daemon_running: false,
  owner_lock_released: true,
  endpoint_released: true,
  supervisor_removed: true,
  coordination_state_removed: true,
  binary_retained: true,
  retry_safe: true,
  local_only: true,
});

export function daemonUninstallResult(overrides = {}, {
  requestedDataRoot = VERIFIED_DAEMON_UNINSTALL_RESULT.requested_data_root,
  canonicalDataRoot = VERIFIED_DAEMON_UNINSTALL_RESULT.canonical_data_root,
} = {}) {
  const quiescedRoots = canonicalDataRoot === requestedDataRoot
    ? [requestedDataRoot]
    : [canonicalDataRoot, requestedDataRoot];
  return `${JSON.stringify({
    ...VERIFIED_DAEMON_UNINSTALL_RESULT,
    requested_data_root: requestedDataRoot,
    canonical_data_root: canonicalDataRoot,
    quiesced_roots: quiescedRoots,
    quiesced_root_count: quiescedRoots.length,
    ...overrides,
  }, null, 2)}\n`;
}
