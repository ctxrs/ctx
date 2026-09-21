#!/usr/bin/env node
// Test inputs only: this packet never changes candidate or release authority.
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { renderCliInstallPowerShellScript } from "../src/cli-install-powershell-script.js";
import { CLI_INSTALL_POWERSHELL_NATIVE_PROCESS } from "../src/cli-install-powershell-native-process.js";
import { CLI_INSTALL_POWERSHELL_PROCESS_HELPERS } from "../src/cli-install-powershell-process.js";
import { renderCliInstallPowerShellManagedPairPublication } from "../src/cli-install-powershell-managed-pair.js";

const repository = fileURLToPath(new URL("../../../", import.meta.url));
export const POWERSHELL = Object.freeze({
  version: "7.6.5",
  file: "PowerShell-7.6.5-win-x64.zip",
  url: "https://github.com/PowerShell/PowerShell/releases/download/v7.6.5/PowerShell-7.6.5-win-x64.zip",
  sha256: "32eb8f6cdce08f86e987d625a2733e54ac3e289ae7e1621b14c0b5bcec2434ea",
  size_bytes: 106319290,
});
export const ARCHIVE_NAME = "windows-installer-fixtures.tar";
export const MANIFEST_NAME = "windows-installer-fixtures.json";
const COPIED_FIXTURES = [
  "native-process-fixture.cs",
  "native-process-fixture.ps1",
  "released-pair-fixture.ps1",
  "hosted-windows-install-fixture.ps1",
];
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");

export function renderFixtureFiles() {
  const files = new Map([
    ["install.ps1", renderCliInstallPowerShellScript({ installAttemptId: "ia_windows_pipeline_fixture" })],
    ["native-process.ps1", CLI_INSTALL_POWERSHELL_NATIVE_PROCESS],
    ["pair-helpers.ps1", CLI_INSTALL_POWERSHELL_PROCESS_HELPERS + "\n" + renderCliInstallPowerShellManagedPairPublication()],
    ["compile-fixture.ps1", "param([string]$Source, [string]$Output)\n$ErrorActionPreference = 'Stop'\nAdd-Type -Path $Source -OutputAssembly $Output -OutputType ConsoleApplication\n"],
  ]);
  for (const name of COPIED_FIXTURES) {
    const source = new URL(`../src/test/${name}`, import.meta.url);
    if (!fs.lstatSync(source).isFile()) throw new Error(`fixture must be a regular source file: ${name}`);
    files.set(name, fs.readFileSync(source));
  }
  return new Map([...files].sort(([left], [right]) => left.localeCompare(right))
    .map(([name, bytes]) => [name, Buffer.from(bytes)]));
}

function validateIdentity(publicCommit, producerJobId) {
  if (!/^[0-9a-f]{40}$/.test(publicCommit ?? "")) throw new Error("one exact public commit is required");
  if (!/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(producerJobId ?? "")) {
    throw new Error("one exact producer job UUID is required");
  }
}

export function createFixturePacket({ publicCommit, producerJobId, outputDir }) {
  validateIdentity(publicCommit, producerJobId);
  if (typeof outputDir !== "string" || outputDir.length === 0) throw new Error("new output directory is required");
  const output = path.resolve(outputDir);
  const files = renderFixtureFiles();
  // Exclusive creation prevents reuse of another source/job's packet.
  fs.mkdirSync(output, { mode: 0o700 });
  const payload = path.join(output, "files");
  fs.mkdirSync(payload, { mode: 0o700 });
  const inventory = [];
  for (const [name, bytes] of files) {
    fs.writeFileSync(path.join(payload, name), bytes, { flag: "wx", mode: 0o600 });
    inventory.push({ path: name, size_bytes: bytes.length, sha256: digest(bytes) });
  }
  const archive = path.join(output, ARCHIVE_NAME);
  const packed = spawnSync("/usr/bin/tar", [
    "--format=ustar", "--owner=0", "--group=0", "--numeric-owner", "--mtime=@0", "--mode=600",
    "-cf", archive, "-C", payload, "--", ...files.keys(),
  ], { encoding: "utf8", timeout: 30_000, maxBuffer: 1024 * 1024 });
  if (packed.error || packed.status !== 0) throw new Error("fixture archive construction failed");
  const archiveBytes = fs.readFileSync(archive);
  const manifest = {
    schema_version: 1,
    kind: "ctx-windows-installer-fixtures",
    release_authority: false,
    public_commit: publicCommit,
    producer_job_id: producerJobId,
    powershell: POWERSHELL,
    archive: { file: ARCHIVE_NAME, sha256: digest(archiveBytes), size_bytes: archiveBytes.length },
    files: inventory,
  };
  fs.writeFileSync(path.join(output, MANIFEST_NAME), JSON.stringify(manifest, null, 2) + "\n",
    { flag: "wx", mode: 0o400 });
  fs.chmodSync(archive, 0o400);
  return manifest;
}

function assertSource(publicCommit) {
  const git = (...args) => {
    const result = spawnSync("git", ["-C", repository, ...args], { encoding: "utf8", timeout: 10_000 });
    if (result.status !== 0) throw new Error("cannot verify fixture source checkout");
    return result.stdout.trim();
  };
  if (git("rev-parse", "--verify", "HEAD^{commit}") !== publicCommit ||
      git("status", "--porcelain=v1", "--untracked-files=all") !== "") {
    throw new Error("fixture source must be clean at the exact public commit");
  }
}

export function main(args) {
  const { values } = parseArgs({ args, options: {
    "public-commit": { type: "string" }, "producer-job-id": { type: "string" }, "output-dir": { type: "string" },
  } });
  validateIdentity(values["public-commit"], values["producer-job-id"]);
  assertSource(values["public-commit"]);
  const result = createFixturePacket({ publicCommit: values["public-commit"],
    producerJobId: values["producer-job-id"], outputDir: values["output-dir"] });
  assertSource(values["public-commit"]);
  return result;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try { console.log(JSON.stringify(main(process.argv.slice(2)))); }
  catch (error) { console.error(`Windows installer fixture packet: ${error.message}`); process.exitCode = 1; }
}
