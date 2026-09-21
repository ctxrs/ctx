import { assert, fileURLToPath, gzipSync, mkdtempSync, path, powerShellCommand,
  rmSync, spawnSync, test, tmpdir, writeFileSync } from "./cli-install-test-helpers.mjs";
import { powerShellAcquisitionFixture } from "./cli-install-resource-fixture.mjs";

test("hosted PowerShell bounds reads/writes and gzip and releases failed file handles", {
  skip: powerShellCommand ? false : "PowerShell is not installed",
}, () => {
  const root = mkdtempSync(path.join(tmpdir(), "ctx-installer-ps-bounds-"));
  try {
    const functions = path.join(root, "functions.ps1");
    writeFileSync(functions, powerShellAcquisitionFixture());
    writeFileSync(path.join(root, "exact.gz"), gzipSync(Buffer.alloc(65536)));
    writeFileSync(path.join(root, "bomb.gz"), gzipSync(Buffer.alloc(8 * 1024 * 1024)));
    writeFileSync(path.join(root, "corrupt.gz"), "not gzip");
    const result = spawnSync(powerShellCommand, ["-NoProfile", "-NonInteractive", "-File",
      fileURLToPath(new URL("./cli-install-powershell-resource-fixture.ps1", import.meta.url)),
      "-FunctionsPath", functions, "-WorkRoot", root], {
      encoding: "utf8", timeout: 30_000,
      env: { ...process.env, CTX_UPGRADE_AUTO: "off" },
    });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.equal(JSON.parse(result.stdout).cases.length, 8);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
