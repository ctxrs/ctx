import {
  assert, mkdtempSync, path, powerShellCommand, rmSync, spawnSync, test,
  tmpdir, writeFileSync,
} from "./cli-install-test-helpers.mjs";
import { CLI_INSTALL_SHELL_VERSION_COMPARE } from "../cli-install-bridge.js";
import { invalidReleaseVersions, powerShellBridgeFixture, releaseVersionVectors } from "./cli-install-bridge-test-helpers.mjs";

test("POSIX release ordering rejects malformed versions and preserves SemVer precedence", () => {
  const commands = releaseVersionVectors.map(([a, b]) => `compare_release_versions '${a}' '${b}'`);
  commands.push(...invalidReleaseVersions.map((v) => `if compare_release_versions '${v}' '1.3.2'; then exit 1; else printf 'rejected\\n'; fi`));
  const result = spawnSync("sh", ["-c", `${CLI_INSTALL_SHELL_VERSION_COMPARE}\n${commands.join("\n")}`], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.stdout.trim().split("\n"), [
    ...releaseVersionVectors.map(([, , n]) => String(n)),
    ...invalidReleaseVersions.map(() => "rejected"),
  ]);
});

test("PowerShell bridge uses bounded legacy receipts and preserves telemetry transport", { skip: !powerShellCommand }, () => {
  const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-bridge-powershell-"));
  try {
    const script = path.join(sandbox, "bridge.ps1");
    writeFileSync(script, powerShellBridgeFixture());
    const result = spawnSync(powerShellCommand, ["-NoProfile", "-NonInteractive", "-File", script], { encoding: "utf8" });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.match(result.stdout, /PASS: PowerShell parsing/);
  } finally { rmSync(sandbox, { recursive: true, force: true }); }
});

test("PowerShell current recovery receipts wait for identity and reject malformed variants", { skip: !powerShellCommand }, async () => {
  const { powerShellRecoveryFixture } = await import("./cli-install-powershell-recovery-test-helpers.mjs");
  const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-recovery-powershell-"));
  try {
    const script = path.join(sandbox, "recovery.ps1");
    writeFileSync(script, powerShellRecoveryFixture());
    const result = spawnSync(powerShellCommand, ["-NoProfile", "-NonInteractive", "-File", script], { encoding: "utf8" });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.match(result.stdout, /PASS: 3 recovered schedules awaited identity; wrong identity timed out; 3 ordinary outcomes; 53 malformed receipts rejected before wait/);
  } finally { rmSync(sandbox, { recursive: true, force: true }); }
});
