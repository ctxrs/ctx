import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { renderCliInstallShellManagedPairApply } from "../cli-install-shell-managed-pair.js";
import { renderCliInstallPowerShellManagedPairPublication } from "../cli-install-powershell-managed-pair.js";
import { CLI_INSTALL_POWERSHELL_PROCESS_HELPERS } from "../cli-install-powershell-process.js";
import { authenticateReleased131Core } from "./released-command-compatibility.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
// Independent wire expectation: do not import expected argv/receipts from the
// renderer's contract. These bytes also match Rust's SUCCESS_RECEIPT.
const success = '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}';

test("shell receipt warnings escape the AWK regex delimiter for Apple awk", () => {
  assert.ok(
    renderCliInstallShellManagedPairApply().includes(String.raw`warning !~ /^[A-Za-z0-9 .,:;()\/_-]+$/`),
    "a slash inside the warning character class must still escape the AWK delimiter",
  );
});

test("shell apply preserves every argument through native quoting", () => {
  const root = mkdtempSync(path.join(tmpdir(), "ctx-pair-contract-"));
  try {
    const candidate = path.join(root, 'candidate $ctx "quoted"');
    const installRoot = path.join(root, "install root 'literal' $value");
    const envelope = path.join(root, "signed envelope.json");
    const companion = path.join(root, "companion & file");
    const marker = path.join(root, "marker ; file");
    writeFileSync(candidate, '#!/bin/sh\nprintf "%s\\0" "$@" > "$ARGV_LOG"\nprintf "%s\\n" "$PAIR_RECEIPT"\n', { mode: 0o700 });
    const script = `${renderCliInstallShellManagedPairApply()}
fail() { printf '%s\\n' "$*" >&2; exit 1; }
log() { printf '%s\\n' "$*" >&2; }
path_size_bytes() { wc -c < "$1"; }
receipt_warning() { printf '%s\\n' "$*" >&2; }
apply_managed_pair_candidate "$MARKER" 1
`;
    const result = spawnSync("sh", ["-eu", "-c", script], { encoding: "utf8", timeout: 10_000,
      env: { PATH: process.env.PATH, HOME: root, tmp_dir: root, version: "1.3.2",
        bin_dir: path.join(installRoot, "bin"), artifact_path: candidate,
        pair_envelope_path: envelope, companion_artifact_path: companion,
        MARKER: marker, ARGV_LOG: path.join(root, "argv"), PAIR_RECEIPT: success } });
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(readFileSync(path.join(root, "argv"), "utf8").split("\0"), [
      "--ctx-core-managed-pair-apply-v1", installRoot, "-", envelope, candidate,
      companion, marker, "",
    ]);
    assert.equal(result.stdout, "");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("released regression authenticates metadata before trusting fake or source-built capabilities", () => {
  const metadata = readFileSync(path.join(here, "released-command-compatibility-v1.3.1.env"));
  const signature = readFileSync(path.join(here, "released-command-compatibility-v1.3.1.sig"));
  for (const executable of [
    '#!/bin/sh\necho "ctx 1.3.1"\n',
    `#!/bin/sh\nprintf '%s\\n' '${success}'\n`,
    "source build implementing --ctx-core-managed-pair-apply-v1",
  ]) {
    assert.throws(() => authenticateReleased131Core(Buffer.from(executable), metadata, signature),
      /requires genuine released 1\.3\.1 bytes/);
  }
  const changed = Buffer.from(metadata.toString().replace("1.3.1", "1.3.2"));
  assert.throws(() => authenticateReleased131Core(Buffer.from("fake"), changed, signature),
    /released metadata signature is invalid/);
  assert.throws(() => authenticateReleased131Core(Buffer.from("fake"), metadata, Buffer.from("invalid")),
    /released metadata signature is invalid/);
});

const powershell = ["pwsh", "powershell"].find((command) =>
  spawnSync(command, ["-NoProfile", "-NonInteractive", "-Command", "exit 0"]).status === 0);
test("PowerShell apply validates independent argv and receipts and relays required errors", {
  skip: powershell ? false : "PowerShell is not installed; native editions remain qualification work",
}, () => {
  const root = mkdtempSync(path.join(tmpdir(), "ctx-pair-ps-contract-"));
  try {
    const helper = path.join(root, "helpers.ps1");
    writeFileSync(helper, CLI_INSTALL_POWERSHELL_PROCESS_HELPERS + "\n" +
      renderCliInstallPowerShellManagedPairPublication());
    const result = spawnSync(powershell, ["-NoProfile", "-NonInteractive", "-File",
      path.join(here, "cli-install-managed-pair-contract-fixture.ps1"),
      "-HelperPath", helper, "-WorkRoot", root], { encoding: "utf8", timeout: 30_000 });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.equal(JSON.parse(result.stdout).passed, 9);
    assert.match(result.stderr, /underlying pair error/);
    assert.match(result.stderr, /Bearer <redacted>/);
    assert.doesNotMatch(result.stderr, /private-token/);
    assert.match(result.stderr, /ctx child output truncated/);
    assert.ok(result.stderr.length < 20_000, "underlying errors must remain bounded");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
