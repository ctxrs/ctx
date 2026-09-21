// Test preload: prebind unrelated release authorities, preserving actual signer,
// managed metadata validator, current handoff parser and current version boundary.
import fs from "node:fs";
import crypto from "node:crypto";
import childProcess from "node:child_process";
import { mock } from "node:test";
import { syntheticLoaded } from "./current_runtime_fixture.mjs";
import * as hosted from "../hosted-managed-pair-release.mjs";

mock.module(new URL("../hosted-managed-pair-release.mjs", import.meta.url).href, {
  namedExports: { ...hosted, loadHostedManagedPairPublication: () => syntheticLoaded() },
});
mock.module(new URL("../verify-runtime-transport-handoff.mjs", import.meta.url).href, {
  namedExports: { verifyRuntimeTransportHandoff: () => {} },
});
mock.module(new URL("../hosted-semantic-release.mjs", import.meta.url).href, {
  namedExports: { renderHostedSemanticMetadataExtension: () => fs.readFileSync(process.env.CTX_TEST_SEMANTIC_EXTENSION),
    loadHostedSemanticAssetCatalog: () => [], snapshotHostedSemanticAsset: () => { throw new Error("unexpected semantic read"); } },
});
mock.module(new URL("../release-candidate-manifest-contract.cjs", import.meta.url).href, {
  defaultExport: { CORE_GITHUB_HANDOFF_METADATA_KEY: "CTX_RELEASE_CORE_GITHUB_HANDOFF_SHA256",
    verifyCandidateManifestHandoff: () => {}, parseEnvMetadata: () => ({}) },
});
const spawn = childProcess.spawnSync;
mock.method(childProcess, "spawnSync", (command, args, options) => {
  if (args?.some((arg) => String(arg).endsWith("semantic_runtime_metadata.py"))
      && args.includes("--allow-managed-pair-nonsemantic")) {
    // Semantic archive validation is outside this real current metadata boundary.
    return { status: 0, stdout: "", stderr: "" };
  }
  if (command === process.execPath) {
    return spawn(command, ["--experimental-test-module-mocks", `--import=${import.meta.url}`, ...args], options);
  }
  return spawn(command, args, options);
});
mock.method(crypto, "sign", () => {
  fs.appendFileSync(process.env.CTX_TEST_SIGNING_SENTINEL, "key-use\n");
  throw new Error("TEST_SIGNING_KEY_USE_SENTINEL");
});
