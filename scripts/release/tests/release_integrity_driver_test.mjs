import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const source = new URL("../release-contract.sh", import.meta.url);

// Only the shell's delegation/exit boundary is doubled here. The enclosing
// smoke harness runs the real cjs signature/source/artifact readback checks.
function run(status) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-release-integrity-driver-"));
  try {
    const release = path.join(root, "scripts/release");
    fs.mkdirSync(release, { recursive: true });
    fs.copyFileSync(source, path.join(release, "release-contract.sh"));
    const evidence = path.join(root, "evidence.json");
    fs.writeFileSync(path.join(release, "release-contract.cjs"), `
const assert = require("node:assert/strict");
const fs = require("node:fs");
assert.equal(process.cwd(), ${JSON.stringify(root)});
assert.equal(process.env.CTX_PUBLIC_RELEASE_SOURCE_COMMIT, "a".repeat(40));
assert.equal(process.env.CTX_PUBLIC_RELEASE_VERSION, "1.5.0");
assert.equal(process.env.CTX_PUBLIC_RELEASE_EVIDENCE_PATH, ${JSON.stringify(evidence)});
fs.writeFileSync("metadata-called", "1");
if (${status}) { console.error("authored integrity rejection"); process.exit(${status}); }
fs.writeFileSync(process.env.CTX_PUBLIC_RELEASE_EVIDENCE_PATH, "authored publication evidence\\n");
`);
    const install = path.join(root, "services/install-site/tests");
    fs.mkdirSync(install, { recursive: true });
    fs.writeFileSync(path.join(install, "install_live_smoke.sh"),
      '#!/bin/sh\ntouch native-called\nexit 91\n', { mode: 0o755 });
    const result = spawnSync("bash", [path.join(release, "release-contract.sh")], {
      encoding: "utf8", timeout: 10_000, env: {
        PATH: process.env.PATH, HOME: root, TMPDIR: root,
        XDG_CONFIG_HOME: root, XDG_DATA_HOME: root, XDG_STATE_HOME: root,
        XDG_CACHE_HOME: root, XDG_RUNTIME_DIR: root, CTX_DATA_ROOT: root,
        CTX_PUBLIC_RELEASE_SOURCE_COMMIT: "a".repeat(40),
        CTX_PUBLIC_RELEASE_VERSION: "1.5.0", CTX_PUBLIC_RELEASE_EVIDENCE_PATH: evidence,
      },
    });
    return { ...result, called: fs.existsSync(path.join(root, "metadata-called")),
      nativeCalled: fs.existsSync(path.join(root, "native-called")),
      evidence: fs.existsSync(evidence) ? fs.readFileSync(evidence, "utf8") : null };
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
}

test("publication driver invokes its integrity owner without a native campaign", () => {
  const result = run(0);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.called, true);
  assert.equal(result.nativeCalled, false);
  assert.equal(result.evidence, "authored publication evidence\n");
});

test("publication driver preserves the integrity owner's exact failure status", () => {
  const result = run(19);
  assert.equal(result.status, 19, result.stderr);
  assert.equal(result.called, true);
  assert.equal(result.nativeCalled, false);
  assert.equal(result.evidence, null);
  assert.match(result.stderr, /authored integrity rejection/);
});
