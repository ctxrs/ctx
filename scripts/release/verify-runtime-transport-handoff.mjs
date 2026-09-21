#!/usr/bin/env node

import childProcess from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

import {
  loadHostedManagedPairPublication,
  loadRuntimeTransportHandoff,
} from "./hosted-managed-pair-release.mjs";
import { transcodeRuntimeTarZstd } from "./runtime-transport-transcode.mjs";

function fail(message) { throw new Error(message); }

function identitySafeFile(file, label, maximumBytes) {
  const absolute = path.resolve(file);
  const stat = fs.lstatSync(absolute, { bigint: true });
  if (!stat.isFile() || stat.isSymbolicLink() || stat.nlink !== 1n
      || stat.size < 1n || stat.size > BigInt(maximumBytes)) {
    fail(`${label} is not an identity-safe bounded file`);
  }
  return absolute;
}

function stableFileSnapshot(file, label, maximumBytes) {
  const absolute = path.resolve(file);
  let descriptor;
  try {
    descriptor = fs.openSync(
      absolute,
      fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0),
    );
  } catch {
    fail(`${label} is not an identity-safe bounded file`);
  }
  try {
    const before = fs.fstatSync(descriptor, { bigint: true });
    if (!before.isFile() || before.nlink !== 1n
        || before.size < 1n || before.size > BigInt(maximumBytes)) {
      fail(`${label} is not an identity-safe bounded file`);
    }
    const body = fs.readFileSync(descriptor);
    const after = fs.fstatSync(descriptor, { bigint: true });
    const current = fs.lstatSync(absolute, { bigint: true });
    if (!current.isFile() || current.isSymbolicLink() || current.nlink !== 1n
        || before.dev !== after.dev || before.ino !== after.ino
        || before.size !== after.size || before.mtimeNs !== after.mtimeNs
        || before.ctimeNs !== after.ctimeNs || BigInt(body.length) !== before.size
        || current.dev !== after.dev || current.ino !== after.ino
        || current.size !== after.size || current.mtimeNs !== after.mtimeNs
        || current.ctimeNs !== after.ctimeNs) {
      fail(`${label} changed while being snapshotted`);
    }
    return Object.freeze({ absolute, body });
  } finally {
    fs.closeSync(descriptor);
  }
}

function writeSnapshot(file, body) {
  const descriptor = fs.openSync(file, "wx", 0o600);
  try {
    fs.writeFileSync(descriptor, body);
    fs.fsyncSync(descriptor);
  } finally {
    fs.closeSync(descriptor);
  }
}

function command(argv, options = {}) {
  const { capture = false, label = argv[0], ...spawnOptions } = options;
  const result = childProcess.spawnSync(argv[0], argv.slice(1), {
    encoding: "utf8",
    stdio: capture ? ["ignore", "pipe", "pipe"] : "inherit",
    timeout: 60_000,
    ...spawnOptions,
  });
  if (result.error != null || result.status !== 0) {
    const detail = capture ? `: ${(result.stderr ?? "").trim()}` : "";
    fail(`${label} failed${detail}`);
  }
  return capture ? result.stdout.trim() : "";
}

function publicCheckout(publicRepo, expectedCommit) {
  const requested = path.resolve(publicRepo);
  const requestedStat = fs.lstatSync(requested);
  if (!requestedStat.isDirectory() || requestedStat.isSymbolicLink()) {
    fail("public ctx checkout must be a non-symlink directory");
  }
  const absolute = fs.realpathSync(requested);
  const git = ["git", "--no-replace-objects", "-C", absolute];
  const top = command([...git, "rev-parse", "--show-toplevel"], {
    capture: true,
    env: verificationEnvironment(expectedCommit),
    label: "public ctx root resolution",
  });
  const head = command([...git, "rev-parse", "--verify", "HEAD^{commit}"], {
    capture: true,
    env: verificationEnvironment(expectedCommit),
    label: "public ctx commit resolution",
  });
  const dirty = command([...git, "status", "--porcelain=v1", "--untracked-files=all"], {
    capture: true,
    env: verificationEnvironment(expectedCommit),
    label: "public ctx clean-tree check",
  });
  const replacementRefs = command([
    ...git, "for-each-ref", "--format=%(refname)", "refs/replace",
  ], {
    capture: true,
    env: verificationEnvironment(expectedCommit),
    label: "public ctx replacement-ref check",
  });
  if (fs.realpathSync(top) !== absolute || head !== expectedCommit || dirty !== ""
      || replacementRefs !== "") {
    fail("public ctx checkout is not the exact clean runtime handoff source commit");
  }
  identitySafeFile(
    path.join(absolute, "scripts/verify-macos-release-attestation.sh"),
    "public macOS runtime attestation verifier",
    1024 * 1024,
  );
  return absolute;
}

export function verifyExactPublicCheckout(publicRepo, expectedCommit) {
  return publicCheckout(publicRepo, expectedCommit);
}

function snapshotPublicVerifier(checkout, sourceCommit, destination) {
  for (const [relative, maximumBytes] of [
    ["scripts/verify-macos-release-attestation.sh", 1024 * 1024],
    ["scripts/macos-release-publisher-policy.sh", 256 * 1024],
    ["scripts/apple-developer-id-g2-ca.pem", 256 * 1024],
    ["scripts/macos-release-signing-evidence.py", 2 * 1024 * 1024],
    ["scripts/build-onnxruntime-sidecar.sh", 1024 * 1024],
    ["scripts/onnxruntime-sidecar/validate_sidecar.sh", 1024 * 1024],
    ["scripts/onnxruntime-sidecar/release_manifest.sh", 1024 * 1024],
    ["scripts/onnxruntime-sidecar/source_inputs.sh", 2 * 1024 * 1024],
    ["scripts/onnxruntime-sidecar/archive_tool.py", 2 * 1024 * 1024],
    ["scripts/onnxruntime-sidecar/validate_runtime.py", 2 * 1024 * 1024],
  ]) {
    const result = childProcess.spawnSync(
      "git",
      ["--no-replace-objects", "-C", checkout, "show", `${sourceCommit}:${relative}`],
      {
        encoding: null,
        env: verificationEnvironment(sourceCommit),
        maxBuffer: maximumBytes + 1,
        timeout: 60_000,
      },
    );
    if (result.error != null || result.status !== 0
        || !Buffer.isBuffer(result.stdout) || result.stdout.length < 1
        || result.stdout.length > maximumBytes) {
      fail(`could not snapshot exact public verifier input ${relative}`);
    }
    const output = path.join(destination, relative);
    fs.mkdirSync(path.dirname(output), { recursive: true, mode: 0o700 });
    writeSnapshot(output, result.stdout);
  }
  return path.join(destination, "scripts/verify-macos-release-attestation.sh");
}

function verifyWithPublicRuntimeContract(verifierRoot, selected, environment) {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-contract."));
  try {
    const sourceArchive = path.join(temporary, selected.source_name);
    writeSnapshot(sourceArchive, selected.source.body);
    if (selected.metadata === "windows_x64") {
      if (!selected.source.body.equals(selected.artifact.body)) {
        fail("windows_x64 runtime producer archive differs from its final transport");
      }
    } else {
      const canonical = transcodeRuntimeTarZstd(
        selected.source.body,
        `${selected.metadata} runtime verification`,
      );
      if (!canonical.equals(selected.artifact.body)) {
        fail(`${selected.metadata} final runtime transport is not the canonical producer transcode`);
      }
    }
    command([
      "bash",
      path.join(verifierRoot, "scripts/build-onnxruntime-sidecar.sh"),
      "--validate",
      selected.metadata.replaceAll("_", "-"),
      sourceArchive,
    ], {
      env: environment,
      label: `${selected.metadata} exact public runtime contract validation`,
    });
  } finally {
    fs.rmSync(temporary, { recursive: true });
  }
}

function verificationEnvironment(sourceCommit) {
  const environment = {
    HOME: "/var/empty",
    LANG: process.env.LANG ?? "C",
    LC_ALL: process.env.LC_ALL ?? "C",
    PATH: process.env.PATH ?? "/usr/bin:/bin",
    TMPDIR: process.env.TMPDIR ?? os.tmpdir(),
    CTX_MACOS_RELEASE_SOURCE_COMMIT: sourceCommit,
    GIT_NO_REPLACE_OBJECTS: "1",
  };
  return environment;
}

function extractNestedRuntime(archive, output, environment) {
  const program = String.raw`
import shutil
import sys
import tarfile

archive, output = sys.argv[1:]
expected = "lib/libonnxruntime.dylib"
with tarfile.open(archive, "r:gz") as bundle:
    matches = [member for member in bundle.getmembers() if member.name == expected]
    if len(matches) != 1 or not matches[0].isfile() or matches[0].issym() or matches[0].islnk():
        raise SystemExit("runtime archive must contain one regular lib/libonnxruntime.dylib")
    source = bundle.extractfile(matches[0])
    if source is None:
        raise SystemExit("could not read lib/libonnxruntime.dylib")
    with source, open(output, "xb") as destination:
        shutil.copyfileobj(source, destination)
`;
  command(["python3", "-c", program, archive, output], {
    env: environment,
    label: "macOS runtime archive extraction",
  });
}

function verifyRuntimeArchiveVersion(selected, expectedVersion, environment) {
  const program = String.raw`
import pathlib
import stat
import sys
import tarfile
import zipfile

archive, expected, kind = sys.argv[1:]

def safe_name(name):
    path = pathlib.PurePosixPath(name)
    return (
        name != ""
        and "\\" not in name
        and not path.is_absolute()
        and all(part not in ("", ".", "..") for part in path.parts)
    )

version = None
names = set()
total = 0
if kind == "zip":
    with zipfile.ZipFile(archive) as bundle:
        members = bundle.infolist()
        for member in members:
            if not safe_name(member.filename) or member.filename in names:
                raise SystemExit("runtime ZIP contains an unsafe or duplicate path")
            names.add(member.filename)
            total += member.file_size
            mode = (member.external_attr >> 16) & 0xFFFF
            if stat.S_ISLNK(mode):
                raise SystemExit("runtime ZIP contains a symbolic link")
        matches = [member for member in members if member.filename == "VERSION_NUMBER"]
        if len(matches) == 1 and not matches[0].is_dir() and matches[0].file_size <= 64:
            version = bundle.read(matches[0])
else:
    with tarfile.open(archive, "r:gz") as bundle:
        members = bundle.getmembers()
        for member in members:
            if not safe_name(member.name) or member.name in names:
                raise SystemExit("runtime tarball contains an unsafe or duplicate path")
            names.add(member.name)
            total += member.size
            if not member.isfile() and not member.isdir():
                raise SystemExit("runtime tarball contains a non-file member")
        matches = [member for member in members if member.name == "VERSION_NUMBER"]
        if len(matches) == 1 and matches[0].isfile() and matches[0].size <= 64:
            stream = bundle.extractfile(matches[0])
            version = None if stream is None else stream.read()
if len(members) > 128 or total > 2 * 1024 * 1024 * 1024:
    raise SystemExit("runtime archive exceeds structural limits")
if version != (expected + "\n").encode("ascii"):
    raise SystemExit("runtime archive VERSION_NUMBER does not match the handoff")
`;
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-version."));
  try {
    const archive = path.join(temporary, selected.name);
    writeSnapshot(archive, selected.artifact.body);
    command([
      "python3", "-c", program, archive, expectedVersion,
      selected.metadata === "windows_x64" ? "zip" : "tar.gz",
    ], {
      capture: true,
      env: environment,
      label: `${selected.metadata} runtime archive version validation`,
    });
  } finally {
    fs.rmSync(temporary, { recursive: true });
  }
}

export function verifyRuntimeTransportHandoff(publicRepo, runtimeHandoff) {
  const environment = verificationEnvironment(runtimeHandoff.publicCommit);
  const checkout = publicCheckout(publicRepo, runtimeHandoff.publicCommit);
  const evidenceRoot = path.dirname(runtimeHandoff.snapshot.absolute);
  const verifierRoot = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-verifier."));
  try {
    const verifier = snapshotPublicVerifier(
      checkout,
      runtimeHandoff.publicCommit,
      verifierRoot,
    );
    for (const selected of runtimeHandoff.artifacts.values()) {
      verifyWithPublicRuntimeContract(verifierRoot, selected, environment);
      verifyRuntimeArchiveVersion(selected, runtimeHandoff.version, environment);
    }
    for (const [metadata, platform] of [
      ["macos_x64", "macos-x64"],
      ["macos_arm64", "macos-arm64"],
    ]) {
      const selected = runtimeHandoff.artifacts.get(metadata);
      if (selected == null) fail(`runtime handoff is missing ${metadata}`);
      const statement = stableFileSnapshot(
        path.join(evidenceRoot, `ctx-onnxruntime-${platform}.release-attestation.json`),
        `${platform} runtime archive attestation`,
        256 * 1024,
      );
      const cms = stableFileSnapshot(
        path.join(evidenceRoot, `ctx-onnxruntime-${platform}.release-attestation.cms`),
        `${platform} runtime archive attestation signature`,
        1024 * 1024,
      );
      const notary = stableFileSnapshot(
        path.join(evidenceRoot, `ctx-onnxruntime-${platform}.notary-submit.json`),
        `${platform} notarization response`,
        2 * 1024 * 1024,
      );
      const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-runtime-attestation."));
      try {
        const archive = path.join(temporary, selected.name);
        const nested = path.join(temporary, "libonnxruntime.dylib");
        const statementPath = path.join(temporary, path.basename(statement.absolute));
        const cmsPath = path.join(temporary, path.basename(cms.absolute));
        const notaryPath = path.join(temporary, path.basename(notary.absolute));
        writeSnapshot(archive, selected.artifact.body);
        writeSnapshot(statementPath, statement.body);
        writeSnapshot(cmsPath, cms.body);
        writeSnapshot(notaryPath, notary.body);
        extractNestedRuntime(archive, nested, environment);
        command([
          "bash", verifier, "--runtime-archive", platform,
          archive, nested, statementPath, cmsPath,
        ], {
          env: environment,
          label: `${platform} runtime archive attestation verification`,
        });
      } finally {
        fs.rmSync(temporary, { recursive: true });
      }
    }
  } finally {
    fs.rmSync(verifierRoot, { recursive: true });
  }
}

export function run(argv) {
  if (argv.length !== 6
      || argv[0] !== "--publication"
      || argv[2] !== "--runtime-handoff"
      || argv[4] !== "--public-ctx-repo") {
    fail("usage: verify-runtime-transport-handoff.mjs --publication PATH --runtime-handoff PATH --public-ctx-repo PATH");
  }
  const loaded = loadHostedManagedPairPublication(argv[1]);
  const handoff = loadRuntimeTransportHandoff(argv[3], loaded);
  verifyRuntimeTransportHandoff(argv[5], handoff);
  return handoff;
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const handoff = run(process.argv.slice(2));
    process.stdout.write(`${JSON.stringify({
      public_source_commit: handoff.publicCommit,
      release_name: handoff.releaseName,
      runtime_handoff_sha256: handoff.snapshot.sha256,
      status: "verified",
    })}\n`);
  } catch (error) {
    process.stderr.write(`runtime transport handoff verification failed: ${error.message}\n`);
    process.exitCode = 1;
  }
}
