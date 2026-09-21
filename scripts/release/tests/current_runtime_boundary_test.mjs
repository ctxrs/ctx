// The pair/candidate/Semantic authorities are prebound test doubles here. The
// five-current runtime handoff and release-version validation are real.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { mock } from "node:test";
import { syntheticLoaded, writeRuntimeHandoff } from "./current_runtime_fixture.mjs";
import * as hosted from "../hosted-managed-pair-release.mjs";

const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-current-boundary."));
try {
  const handoffPath = writeRuntimeHandoff(root);
  const loaded = syntheticLoaded();
  mock.module(new URL("../hosted-managed-pair-release.mjs", import.meta.url).href, {
    namedExports: { ...hosted, loadHostedManagedPairPublication: () => loaded },
  });
  mock.module(new URL("../verify-runtime-transport-handoff.mjs", import.meta.url).href, {
    namedExports: { verifyRuntimeTransportHandoff: () => {} },
  });
  mock.module(new URL("../hosted-semantic-release.mjs", import.meta.url).href, {
    namedExports: { renderHostedSemanticMetadataExtension: () => Buffer.alloc(0),
      loadHostedSemanticAssetCatalog: () => [], snapshotHostedSemanticAsset: () => { throw new Error("unexpected asset read"); } },
  });
  mock.module(new URL("../release-candidate-manifest-contract.cjs", import.meta.url).href, {
    defaultExport: { verifyCandidateManifestHandoff: () => {}, parseEnvMetadata: () => ({}) },
  });
  const { run } = await import("../publish-hosted-managed-pair-stable.mjs");
  const common = ["--publication", "prebound-pair", "--runtime-handoff", handoffPath,
    "--public-ctx-repo", "prebound-current-source", "--semantic-artifact-dir", "prebound-semantic",
    "--candidate-manifest-handoff", "prebound-candidates", "--candidate-handoff-sha256", "a".repeat(64)];
  const credentials = [];
  const environment = new Proxy({}, { get: (_, key) => { credentials.push(key); throw new Error(`credential access ${String(key)}`); } });
  const fetch = () => { throw new Error("network must not run"); };
  const output = path.join(root, "metadata.env");
  assert.equal((await run(["prepare", ...common, "--metadata-out", output,
    "--published-at", "2026-09-05T00:00:00.000Z"], environment, fetch)).status, "prepared");
  assert.equal(credentials.length, 0);
  const originalHandoff = fs.readFileSync(handoffPath);
  for (const mutation of ["current-source", "current-slot", "current-digest", "extra-slot"]) {
    const value = JSON.parse(originalHandoff);
    if (mutation === "current-source") value.public_source_commit = "b".repeat(40);
    if (mutation === "current-slot") value.artifacts.pop();
    if (mutation === "current-digest") value.artifacts[0].sha256 = "b".repeat(64);
    if (mutation === "extra-slot") value.artifacts.push({ ...value.artifacts[0], metadata: "arbitrary_x64" });
    fs.writeFileSync(handoffPath, `${JSON.stringify(value)}\n`);
    try {
      for (const command of ["prepare", "publish"]) {
        const operation = command === "prepare" ? ["--metadata-out", path.join(root, "must-not-write"), "--published-at", "2026-09-05T00:00:00.000Z"]
          : ["--metadata", output, "--signature", "must-not-read-signature", "--evidence-out", "must-not-write-evidence"];
        await assert.rejects(run([command, ...common, ...operation], environment, fetch), /runtime transport|runtime .*handoff/u, mutation);
        assert.equal(credentials.length, 0, mutation);
      }
    } finally { fs.writeFileSync(handoffPath, originalHandoff); }
  }
  loaded.version = "1.3.2";
  await assert.rejects(run(["prepare", ...common, "--metadata-out", path.join(root, "bridge-output"),
    "--published-at", "2026-09-05T00:00:00.000Z"], environment, fetch), /use retained B source/u);
  assert.equal(credentials.length, 0);
  process.stdout.write("prebound other authorities: prepare/publish reject current handoff mutations before credentials; real five-current handoff owner\n");
} finally { mock.restoreAll(); fs.rmSync(root, { recursive: true }); }
