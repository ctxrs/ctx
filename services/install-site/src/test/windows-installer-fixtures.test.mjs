import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { ARCHIVE_NAME, MANIFEST_NAME, POWERSHELL, createFixturePacket, main, renderFixtureFiles }
  from "../../tests/build-windows-installer-fixtures.mjs";
import { CLI_INSTALL_POWERSHELL_NATIVE_PROCESS }
  from "../cli-install-powershell-native-process.js";

const identity = { publicCommit: "a".repeat(40), producerJobId: "12345678-1234-1234-1234-123456789abc" };
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
function isolated(body) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-windows-fixture-packet-"));
  try { body(root); } finally { fs.rmSync(root, { recursive: true, force: true }); }
}

test("packet contains exact rendered production capture and checked-in Windows regressions", () => isolated((root) => {
  const outputDir = path.join(root, "packet");
  const value = createFixturePacket({ ...identity, outputDir });
  assert.equal(value.kind, "ctx-windows-installer-fixtures");
  assert.equal(value.release_authority, false);
  assert.equal(value.public_commit, identity.publicCommit);
  assert.equal(value.producer_job_id, identity.producerJobId);
  assert.equal(value.files.length, 8);
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(outputDir, MANIFEST_NAME))), value);
  const archive = fs.readFileSync(path.join(outputDir, ARCHIVE_NAME));
  assert.equal(hash(archive), value.archive.sha256);
  assert.equal(archive.length, value.archive.size_bytes);
  const listed = spawnSync("/usr/bin/tar", ["-tf", path.join(outputDir, ARCHIVE_NAME)], { encoding: "utf8" });
  assert.equal(listed.status, 0, listed.stderr);
  assert.deepEqual(listed.stdout.trim().split("\n"), value.files.map((file) => file.path));
  const extracted = path.join(root, "extracted"); fs.mkdirSync(extracted);
  const unpacked = spawnSync("/usr/bin/tar", ["-xf", path.join(outputDir, ARCHIVE_NAME), "-C", extracted]);
  assert.equal(unpacked.status, 0);
  for (const file of value.files) {
    const bytes = fs.readFileSync(path.join(extracted, file.path));
    assert.equal(bytes.length, file.size_bytes); assert.equal(hash(bytes), file.sha256);
    assert.deepEqual(bytes, renderFixtureFiles().get(file.path));
  }
  assert.equal(fs.readFileSync(path.join(extracted, "native-process.ps1"), "utf8"), CLI_INSTALL_POWERSHELL_NATIVE_PROCESS);
  assert.match(fs.readFileSync(path.join(extracted, "install.ps1"), "utf8"), /Invoke-ReleasedManagedPairInstall/);
}));

test("PowerShell archive is an explicitly pinned separate producer artifact", () => {
  assert.equal(POWERSHELL.version, "7.6.5");
  assert.equal(POWERSHELL.sha256, "32eb8f6cdce08f86e987d625a2733e54ac3e289ae7e1621b14c0b5bcec2434ea");
  assert.equal(POWERSHELL.size_bytes, 106319290);
  assert.equal(POWERSHELL.url, `https://github.com/PowerShell/PowerShell/releases/download/v${POWERSHELL.version}/${POWERSHELL.file}`);
  assert.ok(!renderFixtureFiles().has(POWERSHELL.file));
});

test("install-only fixture isolates inherited feature selections and restores them", () => {
  const fixture = renderFixtureFiles().get("hosted-windows-install-fixture.ps1").toString("utf8");
  for (const [name, value] of Object.entries({
    CTX_INSTALL_SEMANTIC: "'0'", CTX_SEARCH_SEMANTIC: "'false'",
    CTX_INSTALL_PRO_TRIAL: "'0'", CTX_INSTALL_NO_PRO_TRIAL: "'1'",
    CTX_INSTALL_SKILL_AGENTS: "$null", CTX_INSTALL_ALL_SKILL_AGENTS: "'0'",
    CTX_RELEASE_METADATA_URL: "$null", CTX_RELEASE_METADATA_SIGNATURE_URL: "$null",
    CTX_UPGRADE_FUNCTIONS_BASE: "$null", CTX_UPGRADE_CHANNEL: "$null",
    CTX_ALLOW_CUSTOM_RELEASE_BASE_URL: "$null",
  })) {
    assert.ok(fixture.includes(`${name} = ${value}`), `${name} must be explicit`);
  }
  assert.match(fixture, /CTX_DATA_ROOT = \(Join-Path \$tempRoot 'data'\)/);
  assert.ok(fixture.includes("'-NoSetup', '-NoDaemon', '-NoSkill', '-NoProTrial', '-NoModifyPath'"));
  assert.ok(fixture.includes("$prior[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')"));
  assert.ok(fixture.includes("[Environment]::SetEnvironmentVariable($name, $prior[$name], 'Process')"));
  assert.doesNotMatch(fixture, /MetadataPath|['"]-Metadata['"]/);
  assert.ok(fixture.includes("Test-LoadedUserProfile"));
  assert.ok(fixture.includes("[Console]::OpenStandardInput().ReadByte() -ne -1"));
  assert.ok(fixture.includes("@('fresh', 'managed-reinstall')"));
});

test("packet retains old helper authority and version-aware current-feed checks", () => {
  const files = renderFixtureFiles();
  const legacy = files.get("released-pair-fixture.ps1").toString("utf8");
  assert.match(legacy, /--ctx-core-hosted-pair-install-v1/);
  assert.match(legacy, /uncertified installed Core was executed/);
  assert.match(legacy, /native failure exit was lost/);
  const live = files.get("hosted-windows-install-fixture.ps1").toString("utf8");
  assert.ok(live.includes('[version]$ExpectedVersion -ge [version]"1.5.0"'));
  assert.match(live, /1\.5 installed a legacy companion/);
  assert.match(live, /companion digest differs/);
  assert.match(live, /single_binary = \$singleBinary/);
  assert.match(live, /unexpectedly retained a legacy pair receipt/);
  assert.match(live, /fresh.*managed-reinstall/);
  // These are packet-content assertions; execution remains a native guest check.
});

test("packet archive is deterministic for the same source inputs", () => isolated((root) => {
  const first = createFixturePacket({ ...identity, outputDir: path.join(root, "one") });
  const second = createFixturePacket({ ...identity, outputDir: path.join(root, "two") });
  assert.deepEqual(first, second);
}));

test("existing output is never replaced", () => isolated((root) => {
  const outputDir = path.join(root, "packet"); fs.mkdirSync(outputDir);
  fs.writeFileSync(path.join(outputDir, "retained"), "proof");
  assert.throws(() => createFixturePacket({ ...identity, outputDir }), /EEXIST/);
  assert.equal(fs.readFileSync(path.join(outputDir, "retained"), "utf8"), "proof");
}));

test("source and producer identities reject omissions and malformed selectors before writing", () => isolated((root) => {
  for (const replacement of [{ publicCommit: "main" }, { publicCommit: undefined },
    { producerJobId: "paired-linux-factory" }, { producerJobId: undefined }]) {
    const outputDir = path.join(root, "not-created");
    assert.throws(() => createFixturePacket({ ...identity, ...replacement, outputDir }), /exact/);
    assert.equal(fs.existsSync(outputDir), false);
  }
}));

test("CLI rejects wrong source before producing a packet and accepts no implicit selectors", () => isolated((root) => {
  const outputDir = path.join(root, "not-created");
  assert.throws(() => main(["--public-commit", identity.publicCommit,
    "--producer-job-id", identity.producerJobId, "--output-dir", outputDir]), /source/);
  assert.equal(fs.existsSync(outputDir), false);
  assert.throws(() => main([]), /exact public/);
}));
