import assert from "node:assert/strict";
import childProcess from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { materializeReleaseTestSources, SOURCE_COMMIT, RELEASE_NAME, RUNTIME_TRANSPORTS,
  writeRuntimeHandoff, syntheticLoaded } from "./current_runtime_fixture.mjs";

const testSourceUrl = materializeReleaseTestSources(import.meta.url);
const {
  loadHostedManagedPairPublication,
  loadRuntimeTransportHandoff,
  renderHostedManagedPairMetadata,
  validateHostedManagedPairMetadata,
} = await import(new URL("../hosted-managed-pair-release.mjs", testSourceUrl));
const {
  HOSTED_CANDIDATE_MANIFESTS,
  HOSTED_SEMANTIC_ARTIFACTS,
  loadHostedSemanticAssetCatalog,
  renderHostedSemanticMetadataExtension,
  snapshotHostedSemanticAsset,
} = await import(new URL("../hosted-semantic-release.mjs", testSourceUrl));
const {
  compareStableVersions,
  immutableArtifactObjects,
  run: runHostedPublisher,
  strongConditionalEtag,
} = await import(new URL("../publish-hosted-managed-pair-stable.mjs", testSourceUrl));
const { stageRuntimeTransportFiles } = await import(new URL("../stage-runtime-transport-handoff.mjs", testSourceUrl));
await import(new URL("./frozen_bridge_publication_test.mjs", testSourceUrl));

function writeSemanticReleaseInputs(root) {
  const semanticDir = path.join(root, "semantic");
  const candidateDir = path.join(root, "candidates");
  fs.mkdirSync(semanticDir);
  fs.mkdirSync(candidateDir);
  const assets = Object.fromEntries(HOSTED_SEMANTIC_ARTIFACTS.map((artifact, index) => [
    `asset_${index}`,
    {
      archive_sha256: crypto.createHash("sha256").update(artifact.name).digest("hex"),
      artifact: artifact.name,
    },
  ]));
  for (const artifact of HOSTED_SEMANTIC_ARTIFACTS) {
    fs.writeFileSync(path.join(semanticDir, artifact.name), artifact.name, { mode: 0o600 });
  }
  const catalog = Buffer.from(JSON.stringify({ schema_version: 1, assets })).toString("base64");
  const authority = Buffer.from("{}", "utf8").toString("base64");
  fs.writeFileSync(
    path.join(semanticDir, "semantic-release.env"),
    [
      "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION=1",
      `CTX_RELEASE_SEMANTIC_ASSETS=${catalog}`,
      `CTX_RELEASE_SEMANTIC_AUTHORITY_apple_silicon_coreml=${authority}`,
      `CTX_RELEASE_SEMANTIC_AUTHORITY_windows_windows_ml=${authority}`,
      `CTX_RELEASE_SEMANTIC_AUTHORITY_linux_nvidia_ort_cuda=${authority}`,
      `CTX_RELEASE_SEMANTIC_AUTHORITY_universal_ort_cpu=${authority}`,
      "",
    ].join("\n"),
    { mode: 0o600 },
  );
  for (const candidate of HOSTED_CANDIDATE_MANIFESTS) {
    fs.writeFileSync(
      path.join(candidateDir, candidate.name),
      `${JSON.stringify({ candidate: candidate.key })}\n`,
      { mode: 0o600 },
    );
  }
  const handoff = Buffer.from("{}\n", "utf8");
  fs.writeFileSync(
    path.join(candidateDir, "ctx-core-github-handoff.json"),
    handoff,
    { mode: 0o600 },
  );
  return {
    candidateDir,
    handoffSha256: crypto.createHash("sha256").update(handoff).digest("hex"),
    semanticDir,
  };
}

function run(command, args, options = {}) {
  const result = childProcess.spawnSync(command, args, {
    encoding: "utf8",
    env: { PATH: process.env.PATH, HOME: os.tmpdir(), GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null" },
    ...options,
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return result.stdout.trim();
}

function fakePublicCheckout(root, { rejectPlatform = null } = {}) {
  assert.ok(rejectPlatform == null || /^[a-z0-9-]+$/u.test(rejectPlatform));
  const repo = path.join(root, "public-ctx");
  const log = path.join(root, "runtime-verifier.log");
  const contractLog = path.join(root, "runtime-contract.log");
  fs.mkdirSync(path.join(repo, "scripts"), { recursive: true });
  fs.writeFileSync(
    path.join(repo, "scripts", "verify-macos-release-attestation.sh"),
    `#!/usr/bin/env bash
set -euo pipefail
test "$#" -eq 6
test "$1" = --runtime-archive
case "$2" in macos-x64|macos-arm64) ;; *) exit 91 ;; esac
test -s "$3" && test -s "$4" && test -s "$5" && test -s "$6"
test -n "\${CTX_MACOS_RELEASE_SOURCE_COMMIT:-}"
test -z "\${CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM:-}"
test -z "\${CTX_CLI_METADATA_SIGNING_PRIVATE_KEY:-}"
printf '%s\n' "$2" >>${JSON.stringify(log)}
`,
    { mode: 0o755 },
  );
  for (const name of [
    "macos-release-publisher-policy.sh",
    "apple-developer-id-g2-ca.pem",
    "macos-release-signing-evidence.py",
    "onnxruntime-sidecar/validate_sidecar.sh",
    "onnxruntime-sidecar/release_manifest.sh",
    "onnxruntime-sidecar/source_inputs.sh",
    "onnxruntime-sidecar/archive_tool.py",
    "onnxruntime-sidecar/validate_runtime.py",
  ]) {
    fs.mkdirSync(path.dirname(path.join(repo, "scripts", name)), { recursive: true });
    fs.writeFileSync(path.join(repo, "scripts", name), `${name} fixture\n`);
  }
  fs.writeFileSync(
    path.join(repo, "scripts", "build-onnxruntime-sidecar.sh"),
    `#!/usr/bin/env bash
set -euo pipefail
test "$#" -eq 3
test "$1" = --validate
case "$2" in linux-x64|linux-aarch64|windows-x64|macos-x64|macos-arm64) ;; *) exit 92 ;; esac
test -s "$3"
case "$2" in windows-x64) test "\${3##*.}" = zip ;; *) test "\${3##*.}" = zst ;; esac
printf '%s\n' "$2" >>${JSON.stringify(contractLog)}
if test "$2" = ${JSON.stringify(rejectPlatform ?? "never-reject")}; then exit 93; fi
`,
    { mode: 0o755 },
  );
  run("git", ["-C", repo, "init", "-q"]);
  run("git", ["-C", repo, "config", "user.email", "runtime@example.test"]);
  run("git", ["-C", repo, "config", "user.name", "runtime fixture"]);
  run("git", ["-C", repo, "add", "."]);
  run("git", ["-C", repo, "commit", "-qm", "runtime verifier fixture"]);
  return {
    commit: run("git", ["-C", repo, "rev-parse", "HEAD"]),
    contractLog,
    log,
    repo,
  };
}

function writeRuntimeTransportInputs(root, embeddedVersion = "1.27.0") {
  const bodies = new Map();
  for (const [metadata, name, sourceName] of RUNTIME_TRANSPORTS) {
    const sourceDestination = path.join(root, sourceName);
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-fixture."));
    try {
      const tarArchive = path.join(temporary, "runtime.tar");
      const canonicalGzip = path.join(temporary, "runtime.tar.gz");
      const program = String.raw`
import gzip
import io
import tarfile
import sys
import zipfile

destination, tar_archive, canonical_gzip, kind, library, version = sys.argv[1:]
files = {
    "VERSION_NUMBER": (version + "\n").encode(),
    library: b"current runtime library\n",
}
if kind == "zip":
    with zipfile.ZipFile(destination, "w") as bundle:
        for name, body in files.items():
            member = zipfile.ZipInfo(name)
            member.external_attr = (0o100755 if name.startswith("lib/") else 0o100644) << 16
            bundle.writestr(member, body)
else:
    with tarfile.open(tar_archive, "w") as bundle:
        for name, body in files.items():
            member = tarfile.TarInfo(name)
            member.mode = 0o755 if name.startswith("lib/") else 0o644
            member.mtime = 0
            member.size = len(body)
            bundle.addfile(member, io.BytesIO(body))
    with open(tar_archive, "rb") as source, open(canonical_gzip, "xb") as raw_output:
        with gzip.GzipFile(
            filename="", mode="wb", fileobj=raw_output, compresslevel=9, mtime=0
        ) as output:
            while chunk := source.read(1024 * 1024):
                output.write(chunk)
`;
      const zip = metadata === "windows_x64";
      const library = zip
        ? "lib/onnxruntime.dll"
        : metadata.startsWith("macos_")
          ? "lib/libonnxruntime.dylib"
          : "lib/libonnxruntime.so";
      run("python3", [
        "-c",
        program,
        sourceDestination,
        tarArchive,
        canonicalGzip,
        zip ? "zip" : "tar.zst",
        library,
        embeddedVersion,
      ]);
      if (!zip) {
        run("zstd", ["-q", "-f", "-T1", tarArchive, "-o", sourceDestination]);
      }
      const source = fs.readFileSync(sourceDestination);
      bodies.set(metadata, {
        output: zip ? source : fs.readFileSync(canonicalGzip),
        source,
      });
    } finally {
      fs.rmSync(temporary, { recursive: true });
    }
  }
  for (const platform of ["macos-x64", "macos-arm64"]) {
    for (const suffix of [
      "release-attestation.json",
      "release-attestation.cms",
      "notary-submit.json",
    ]) {
      fs.writeFileSync(
        path.join(root, `ctx-onnxruntime-${platform}.${suffix}`),
        `${platform}-${suffix}\n`,
        { mode: 0o600 },
      );
    }
  }
  return bodies;
}

test("stable pair metadata requires the complete runtime transport handoff", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-handoff-test."));
  try {
    const loaded = syntheticLoaded();
    assert.throws(
      () => renderHostedManagedPairMetadata(loaded, "2026-08-24T00:00:00.000Z"),
      /requires the complete runtime transport handoff/u,
    );

    const runtimeHandoff = loadRuntimeTransportHandoff(writeRuntimeHandoff(root), loaded);
    const metadata = renderHostedManagedPairMetadata(
      loaded,
      "2026-08-24T00:00:00.000Z",
      runtimeHandoff,
    );
    validateHostedManagedPairMetadata(metadata, loaded, runtimeHandoff);
    const changed = Buffer.from(metadata.toString().replace(
      /CTX_RELEASE_SHA256_linux_x64=[0-9a-f]{64}/u,
      `CTX_RELEASE_SHA256_linux_x64=${"0".repeat(64)}`,
    ));
    assert.throws(() => validateHostedManagedPairMetadata(changed, loaded, runtimeHandoff), /differs from exact publication authority/u);
    const text = metadata.toString("utf8");
    assert.match(text, /^CTX_RELEASE_ONNXRUNTIME_VERSION=1\.27\.0$/mu);
    assert.equal((text.match(/CTX_RELEASE_ONNXRUNTIME_ARTIFACT_/gu) ?? []).length, 5);
    assert.equal((text.match(/CTX_RELEASE_ONNXRUNTIME_SHA256_/gu) ?? []).length, 5);
    assert.throws(
      () => validateHostedManagedPairMetadata(metadata, loaded, null),
      /requires the complete runtime transport handoff/u,
    );
  } finally {
    fs.rmSync(root, { recursive: true });
  }
});

test("hosted publisher requires the complete release handoff set", async () => {
  const base = [
    "prepare",
    "--publication", "/not-read/publication.json",
    "--public-ctx-repo", "/not-read/public",
    "--metadata-out", "/not-read/metadata.env",
    "--published-at", "2026-08-24T00:00:00.000Z",
  ];
  await assert.rejects(
    runHostedPublisher([
      ...base,
      "--runtime-handoff", "/not-read/runtime.json",
    ]),
    /requires the exact complete release handoff arguments/u,
  );
  await assert.rejects(
    runHostedPublisher([
      ...base,
      "--without-supplementary-assets", "false",
    ]),
    /requires the exact complete release handoff arguments/u,
  );
  await assert.rejects(
    runHostedPublisher([
      ...base,
      "--without-supplementary-assets", "true",
      "--runtime-handoff", "/not-read/runtime.json",
    ]),
    /requires the exact complete release handoff arguments/u,
  );
});

test("runtime staging snapshots producer archives and canonically transcodes Unix transports", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-stage-test."));
  try {
    const input = path.join(root, "input");
    const output = path.join(root, "output");
    fs.mkdirSync(input);
    const bodies = writeRuntimeTransportInputs(input, "1.28.0");
    const checkout = fakePublicCheckout(root);
    const previousPem = process.env.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM;
    const previousKey = process.env.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY;
    process.env.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM = "must-not-leak";
    process.env.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY = "must-not-leak";
    let staged;
    try {
      staged = stageRuntimeTransportFiles({
        inputDirectoryPath: input,
        loaded: syntheticLoaded(checkout.commit),
        outputDirectoryPath: output,
        publicRepo: checkout.repo,
        runtimeVersion: "1.28.0",
      });
    } finally {
      if (previousPem === undefined) delete process.env.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM;
      else process.env.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM = previousPem;
      if (previousKey === undefined) delete process.env.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY;
      else process.env.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY = previousKey;
    }
    assert.equal(staged.handoff.publicCommit, checkout.commit);
    assert.equal(staged.handoff.version, "1.28.0");
    assert.deepEqual(
      fs.readFileSync(checkout.log, "utf8").trim().split("\n").sort(),
      ["macos-arm64", "macos-x64"],
    );
    assert.deepEqual(
      fs.readFileSync(checkout.contractLog, "utf8").trim().split("\n"),
      RUNTIME_TRANSPORTS.map(([metadata]) => metadata.replaceAll("_", "-")),
    );
    const handoffValue = JSON.parse(fs.readFileSync(staged.handoffPath, "utf8"));
    assert.equal(handoffValue.artifacts.length, 5);
    for (const [index, [metadata, name, sourceName]] of RUNTIME_TRANSPORTS.entries()) {
      const expected = bodies.get(metadata);
      const record = handoffValue.artifacts[index];
      assert.deepEqual(Object.keys(record), [
        "metadata",
        "name",
        "path",
        "sha256",
        "size_bytes",
        "source_name",
        "source_path",
        "source_sha256",
        "source_size_bytes",
      ]);
      assert.equal(record.metadata, metadata);
      assert.equal(record.name, name);
      assert.equal(record.path, name);
      assert.equal(record.source_name, sourceName);
      assert.equal(record.source_path, sourceName);
      assert.deepEqual(fs.readFileSync(path.join(output, sourceName)), expected.source);
      assert.deepEqual(fs.readFileSync(path.join(output, name)), expected.output);
      assert.equal(record.source_sha256, crypto.createHash("sha256").update(expected.source).digest("hex"));
      assert.equal(record.source_size_bytes, expected.source.length);
      assert.equal(record.sha256, crypto.createHash("sha256").update(expected.output).digest("hex"));
      assert.equal(record.size_bytes, expected.output.length);
    }
    fs.appendFileSync(path.join(input, RUNTIME_TRANSPORTS[0][2]), "later mutation");
    assert.notDeepEqual(
      fs.readFileSync(path.join(output, RUNTIME_TRANSPORTS[0][2])),
      fs.readFileSync(path.join(input, RUNTIME_TRANSPORTS[0][2])),
    );
  } finally {
    fs.rmSync(root, { recursive: true });
  }
});

test("runtime staging fails closed on dirty, symlinked, or replacement-ref authority", () => {
  for (const kind of ["dirty", "symlink", "replacement-ref"]) {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-stage-test."));
    try {
      const input = path.join(root, "input");
      const output = path.join(root, "output");
      fs.mkdirSync(input);
      writeRuntimeTransportInputs(input);
      const checkout = fakePublicCheckout(root);
      let publicRepo = checkout.repo;
      if (kind === "dirty") fs.writeFileSync(path.join(checkout.repo, "untracked"), "dirty\n");
      if (kind === "symlink") {
        publicRepo = path.join(root, "public-link");
        fs.symlinkSync(checkout.repo, publicRepo);
      }
      if (kind === "replacement-ref") {
        const replacement = run(
          "git",
          [
            "-C", checkout.repo, "commit-tree", `${checkout.commit}^{tree}`,
            "-p", checkout.commit, "-m", "replacement verifier",
          ],
        );
        run("git", ["-C", checkout.repo, "replace", checkout.commit, replacement]);
      }
      assert.throws(
        () => stageRuntimeTransportFiles({
          inputDirectoryPath: input,
          loaded: syntheticLoaded(checkout.commit),
          outputDirectoryPath: output,
          publicRepo,
          runtimeVersion: "1.27.0",
        }),
        /exact clean runtime handoff source commit|non-symlink directory/u,
      );
      assert.equal(fs.existsSync(output), false);
    } finally {
      fs.rmSync(root, { recursive: true });
    }
  }
});

test("runtime staging fails closed when the exact public validator rejects an archive", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-stage-test."));
  try {
    const input = path.join(root, "input");
    const output = path.join(root, "output");
    fs.mkdirSync(input);
    writeRuntimeTransportInputs(input);
    const checkout = fakePublicCheckout(root, { rejectPlatform: "linux-x64" });
    assert.throws(
      () => stageRuntimeTransportFiles({
        inputDirectoryPath: input,
        loaded: syntheticLoaded(checkout.commit),
        outputDirectoryPath: output,
        publicRepo: checkout.repo,
        runtimeVersion: "1.27.0",
      }),
      /linux_x64 exact public runtime contract validation failed/u,
    );
    assert.deepEqual(
      fs.readFileSync(checkout.contractLog, "utf8").trim().split("\n"),
      ["linux-x64"],
    );
    assert.equal(fs.existsSync(output), false);
  } finally {
    fs.rmSync(root, { recursive: true });
  }
});

test("runtime staging rejects a version label that differs from archive bytes", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-stage-test."));
  try {
    const input = path.join(root, "input");
    const output = path.join(root, "output");
    fs.mkdirSync(input);
    writeRuntimeTransportInputs(input, "1.27.1");
    const checkout = fakePublicCheckout(root);
    assert.throws(
      () => stageRuntimeTransportFiles({
        inputDirectoryPath: input,
        loaded: syntheticLoaded(checkout.commit),
        outputDirectoryPath: output,
        publicRepo: checkout.repo,
        runtimeVersion: "1.27.0",
      }),
      /VERSION_NUMBER does not match the handoff/u,
    );
    assert.equal(fs.existsSync(output), false);
  } finally {
    fs.rmSync(root, { recursive: true });
  }
});

for (const [label, mutate, expected] of [
  ["release", (value) => ({ ...value, release_name: "v1.0.2" }), /does not match/u],
  ["source", (value) => ({ ...value, public_source_commit: "b".repeat(40) }), /does not match/u],
  ["missing", (value) => ({ ...value, artifacts: value.artifacts.slice(0, -1) }), /does not match/u],
  ["extra", (value) => ({ ...value, artifacts: [...value.artifacts, value.artifacts[0]] }), /does not match/u],
  ["order", (value) => ({ ...value, artifacts: [value.artifacts[1], value.artifacts[0], ...value.artifacts.slice(2)] }), /record is invalid/u],
  ["hash", (value) => ({ ...value, artifacts: [{ ...value.artifacts[0], sha256: "c".repeat(64) }, ...value.artifacts.slice(1)] }), /differs from its handoff identity/u],
  ["size", (value) => ({ ...value, artifacts: [{ ...value.artifacts[0], size_bytes: value.artifacts[0].size_bytes + 1 }, ...value.artifacts.slice(1)] }), /differs from its handoff identity/u],
  ["path", (value) => ({ ...value, artifacts: [{ ...value.artifacts[0], path: "../runtime" }, ...value.artifacts.slice(1)] }), /record is invalid/u],
  ["source name", (value) => ({ ...value, artifacts: [{ ...value.artifacts[0], source_name: value.artifacts[0].name }, ...value.artifacts.slice(1)] }), /record is invalid/u],
  ["source path", (value) => ({ ...value, artifacts: [{ ...value.artifacts[0], source_path: "../runtime" }, ...value.artifacts.slice(1)] }), /record is invalid/u],
  ["source hash", (value) => ({ ...value, artifacts: [{ ...value.artifacts[0], source_sha256: "d".repeat(64) }, ...value.artifacts.slice(1)] }), /producer archive differs from its handoff identity/u],
  ["source size", (value) => ({ ...value, artifacts: [{ ...value.artifacts[0], source_size_bytes: value.artifacts[0].source_size_bytes + 1 }, ...value.artifacts.slice(1)] }), /producer archive differs from its handoff identity/u],
  ["source field order", (value) => {
    const first = value.artifacts[0];
    return {
      ...value,
      artifacts: [{
        metadata: first.metadata,
        name: first.name,
        path: first.path,
        sha256: first.sha256,
        size_bytes: first.size_bytes,
        source_path: first.source_path,
        source_name: first.source_name,
        source_sha256: first.source_sha256,
        source_size_bytes: first.source_size_bytes,
      }, ...value.artifacts.slice(1)],
    };
  }, /fields are not in canonical order/u],
]) {
  test(`runtime handoff rejects ${label} substitution`, () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-handoff-test."));
    try {
      const handoff = writeRuntimeHandoff(root, mutate);
      assert.throws(() => loadRuntimeTransportHandoff(handoff, syntheticLoaded()), expected);
    } finally {
      fs.rmSync(root, { recursive: true });
    }
  });
}

test("runtime handoff rejects changed files, links, and noncanonical JSON", () => {
  for (const kind of ["changed", "changed-source", "symlink", "hardlink", "noncanonical"]) {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-handoff-test."));
    try {
      const handoff = writeRuntimeHandoff(root);
      const artifact = path.join(root, RUNTIME_TRANSPORTS[0][1]);
      if (kind === "changed") fs.appendFileSync(artifact, "changed");
      if (kind === "changed-source") {
        fs.appendFileSync(path.join(root, RUNTIME_TRANSPORTS[0][2]), "changed");
      }
      if (kind === "symlink") {
        const target = path.join(root, "outside");
        fs.writeFileSync(target, "outside");
        fs.unlinkSync(artifact);
        fs.symlinkSync(target, artifact);
      }
      if (kind === "hardlink") fs.linkSync(artifact, path.join(root, "second-link"));
      if (kind === "noncanonical") {
        const value = JSON.parse(fs.readFileSync(handoff, "utf8"));
        fs.writeFileSync(handoff, `${JSON.stringify(value, null, 2)}\n`);
      }
      assert.throws(
        () => loadRuntimeTransportHandoff(handoff, syntheticLoaded()),
        /differs from its handoff identity|identity-safe bounded file|canonical compact JSON/u,
      );
    } finally {
      fs.rmSync(root, { recursive: true });
    }
  }
});

test("stable hosted metadata pointer advances only by canonical SemVer", () => {
  assert.equal(compareStableVersions("1.0.0", "1.0.1"), -1);
  assert.equal(compareStableVersions("1.0.1", "1.0.1"), 0);
  assert.equal(compareStableVersions("1.1.0", "1.0.1"), 1);
  assert.equal(
    compareStableVersions("9007199254740992.0.0", "9007199254740991.999.999"),
    1,
  );
  assert.throws(() => compareStableVersions("01.0.0", "1.0.1"), /canonical SemVer/u);
});

test("stable hosted metadata normalizes R2 weak ETags for conditional writes", () => {
  assert.equal(strongConditionalEtag('"abc123"'), '"abc123"');
  assert.equal(strongConditionalEtag('W/"abc123"'), '"abc123"');
  assert.throws(() => strongConditionalEtag("abc123"), /ETag is invalid/u);
});

test("hosted semantic handoffs extend managed metadata with exact release identities", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-hosted-semantic-test."));
  try {
    const loaded = syntheticLoaded();
    const runtimeHandoff = loadRuntimeTransportHandoff(writeRuntimeHandoff(root), loaded);
    const { candidateDir, handoffSha256, semanticDir } =
      writeSemanticReleaseInputs(root);
    const extension = renderHostedSemanticMetadataExtension(
      semanticDir,
      candidateDir,
      handoffSha256,
    );
    const metadata = Buffer.concat([
      renderHostedManagedPairMetadata(
        loaded,
        "2026-08-20T00:00:00.000Z",
        runtimeHandoff,
      ),
      extension,
    ]);
    validateHostedManagedPairMetadata(metadata, loaded, runtimeHandoff, extension);
    assert.throws(
      () => validateHostedManagedPairMetadata(metadata, loaded, runtimeHandoff),
      /differs from exact publication authority/u,
    );

    const catalog = loadHostedSemanticAssetCatalog(metadata);
    assert.deepEqual(
      catalog.map((asset) => asset.name),
      HOSTED_SEMANTIC_ARTIFACTS.map((asset) => asset.name),
    );
    for (const asset of catalog) {
      snapshotHostedSemanticAsset(semanticDir, asset);
    }
    assert.match(
      extension.toString("utf8"),
      /^CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_linux_x64=[0-9a-f]{64}$/mu,
    );
    assert.match(
      extension.toString("utf8"),
      new RegExp(`^CTX_RELEASE_CORE_GITHUB_HANDOFF_SHA256=${handoffSha256}$`, "mu"),
    );
    assert.throws(
      () => renderHostedSemanticMetadataExtension(
        semanticDir,
        candidateDir,
        "a".repeat(64),
      ),
      /differs from its independent expected digest/u,
    );

    fs.appendFileSync(path.join(candidateDir, "ctx.candidate.json"), "changed\n");
    assert.notDeepEqual(
      renderHostedSemanticMetadataExtension(
        semanticDir,
        candidateDir,
        handoffSha256,
      ),
      extension,
    );
    const cuda = catalog.find((asset) => asset.name.includes("cuda12"));
    fs.appendFileSync(path.join(semanticDir, cuda.name), "changed\n");
    assert.throws(
      () => snapshotHostedSemanticAsset(semanticDir, cuda),
      /differs from signed metadata/u,
    );
  } finally {
    fs.rmSync(root, { recursive: true });
  }
});

test("current metadata and objects contain only five runtimes without historical coupling", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-current-runtimes."));
  try {
    const loaded = syntheticLoaded();
    const handoff = loadRuntimeTransportHandoff(writeRuntimeHandoff(root, (value) => ({
      ...value, runtime_version: "1.28.0",
    })), loaded);
    assert.equal(handoff.artifacts.size, 5);
    assert.equal(handoff.version, "1.28.0");
    assert.equal(Object.hasOwn(handoff, "compatibility"), false);
    for (const target of loaded.targets.values()) {
      target.core.artifact.body = Buffer.from("Core fixture");
      target.companion.artifact.body = Buffer.from("companion fixture");
      target.manifest = { body: Buffer.from("envelope fixture") };
    }
    const objects = immutableArtifactObjects(loaded, handoff);
    assert.equal(objects.length, 30);
    assert.ok(objects.every((object) => !object.key.includes("freebsd")));
    const metadata = renderHostedManagedPairMetadata(loaded, "2026-09-05T00:00:00.000Z", handoff);
    assert.equal((metadata.toString().match(/CTX_RELEASE_ONNXRUNTIME_ARTIFACT_/gu) ?? []).length, 5);
    validateHostedManagedPairMetadata(metadata, loaded, handoff);
    for (const field of ["ONNXRUNTIME_ARTIFACT", "ONNXRUNTIME_SHA256", "ARTIFACT", "MANAGED_PAIR_ENVELOPE"]) {
      assert.throws(() => validateHostedManagedPairMetadata(Buffer.from(`${metadata}CTX_RELEASE_${field}_freebsd_x64=extra\n`), loaded, handoff), /exact publication authority/u);
    }
    assert.throws(() => renderHostedManagedPairMetadata({ ...loaded, version: "1.3.2" }, "2026-09-05T00:00:00.000Z", handoff), /use retained B source/u);
    assert.throws(() => stageRuntimeTransportFiles({ inputDirectoryPath: root, loaded: { ...loaded, version: "1.3.2" },
      outputDirectoryPath: path.join(root, "output"), publicRepo: "unused", runtimeVersion: "1.28.0" }), /use retained B source/u);
    assert.equal(fs.existsSync(path.join(root, "output")), false);
  } finally { fs.rmSync(root, { recursive: true }); }
});

test("publisher keeps current handoff checks ahead of credentials", () => {
  const result = childProcess.spawnSync((process.env.JS_BINARY__NODE_BINARY || process.execPath), ["--experimental-test-module-mocks",
    new URL("./current_runtime_boundary_test.mjs", testSourceUrl).pathname], { encoding: "utf8", env: process.env });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
});

test("standalone signer permits current preparation and rejects bridge construction before key use", () => {
  const result = childProcess.spawnSync((process.env.JS_BINARY__NODE_BINARY || process.execPath), [new URL("./current_signer_boundary_test.mjs", testSourceUrl).pathname],
    { encoding: "utf8", env: process.env });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
});
