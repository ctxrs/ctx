// Compile-only assertions. tsc fails when an expected error stops being an
// error, so weakening the input or tuple types cannot silently turn this green.
import { managedPairApplyArguments, MANAGED_PAIR_APPLY_OPERATION,
  MANAGED_PAIR_APPLY_RECEIPT } from "../cli-install-managed-pair-contract.js";

const paths = { installRoot: "root", envelope: "envelope", core: "core",
  companion: "companion", marker: "marker" };
const args = managedPairApplyArguments(paths);

/** @type {7} */
const arity = args.length;
/** @type {"--ctx-core-managed-pair-apply-v1"} */
const operation = MANAGED_PAIR_APPLY_OPERATION;
/** @type {{readonly schema_version: 1, readonly command: "managed_pair_apply",
 * readonly ok: true, readonly status: "committed"}} */
const receipt = MANAGED_PAIR_APPLY_RECEIPT;
void [arity, operation, receipt];

// @ts-expect-error Missing the marker operand must not build.
managedPairApplyArguments({ installRoot: "r", envelope: "e", core: "c", companion: "p" });
// @ts-expect-error A misspelled named input must not build.
managedPairApplyArguments({ ...paths, installMarker: "m" });
// @ts-expect-error An operand must be a native expression string.
managedPairApplyArguments({ ...paths, marker: 1 });
// @ts-expect-error Literal rendering must produce a string.
managedPairApplyArguments(paths, () => false);
// @ts-expect-error An extra operation operand must not build.
managedPairApplyArguments(paths, (value) => value, "extra");

/** @type {ReturnType<typeof managedPairApplyArguments>} */
// @ts-expect-error Missing an argv position must not build.
const short = ["operation", "root", "-", "envelope", "core", "companion"];
/** @type {ReturnType<typeof managedPairApplyArguments>} */
// @ts-expect-error Adding an argv position must not build.
const long = ["operation", "root", "-", "envelope", "core", "companion", "marker", "extra"];
// @ts-expect-error Callers cannot change the fixed positional vector's arity.
args.push("extra");
// @ts-expect-error The receipt definition is immutable.
MANAGED_PAIR_APPLY_RECEIPT.command = "another_operation";
void [short, long];
