// Projection of ctx-cli/src/core_capability.rs and
// core_capability/managed_pair_apply.rs. Rust owns parsing, authentication and
// installation. This definition only fixes the two hosted wrappers' wire shape;
// source agreement does not prove a released executable supports the operation.
export const MANAGED_PAIR_APPLY_OPERATION = "--ctx-core-managed-pair-apply-v1";
export const MANAGED_PAIR_APPLY_RECEIPT = Object.freeze({
  schema_version: 1,
  command: "managed_pair_apply",
  ok: true,
  status: "committed",
});

/**
 * Arguments are already quoted native shell/PowerShell expressions, never user
 * values. Only the fixed operation and placeholder use the platform's literal
 * spelling; path expressions retain the quoting of their native wrapper.
 * @param {{installRoot: string, envelope: string, core: string,
 *   companion: string, marker: string}} paths
 * @param {(value: string) => string} literal
 * @returns {readonly [operation: string, installRoot: string, dataRoot: string,
 *   envelope: string, core: string, companion: string, marker: string]}
 */
export function managedPairApplyArguments(paths, literal = (value) => value) {
  return [
    literal(MANAGED_PAIR_APPLY_OPERATION),
    paths.installRoot,
    literal("-"),
    paths.envelope,
    paths.core,
    paths.companion,
    paths.marker,
  ];
}
