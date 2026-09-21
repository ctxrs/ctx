#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

import {
  HOSTED_RUNTIME_TRANSPORTS,
  loadHostedManagedPairPublication,
  loadRuntimeTransportHandoff,
  sha256,
} from "./hosted-managed-pair-release.mjs";
import { assertCurrentReleaseVersion } from "./frozen-cli-bridge.cjs";
import { transcodeRuntimeTarZstd } from "./runtime-transport-transcode.mjs";
import { verifyRuntimeTransportHandoff } from "./verify-runtime-transport-handoff.mjs";

const HANDOFF_NAME = "ctx-runtime-transport-handoff-v1.json";
const RUNTIME_VERSION = /^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)$/u;
const MACOS_EVIDENCE = Object.freeze([
  "ctx-onnxruntime-macos-x64.release-attestation.json",
  "ctx-onnxruntime-macos-x64.release-attestation.cms",
  "ctx-onnxruntime-macos-x64.notary-submit.json",
  "ctx-onnxruntime-macos-arm64.release-attestation.json",
  "ctx-onnxruntime-macos-arm64.release-attestation.cms",
  "ctx-onnxruntime-macos-arm64.notary-submit.json",
]);

function fail(message) { throw new Error(message); }

function parseArgs(argv) {
  const names = [
    "--output-dir",
    "--public-ctx-repo",
    "--publication",
    "--runtime-artifact-dir",
    "--runtime-version",
  ];
  if (argv.length !== names.length * 2) {
    fail(`usage: stage-runtime-transport-handoff.mjs ${names.map((name) => `${name} PATH`).join(" ")}`);
  }
  const args = new Map();
  for (let index = 0; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!names.includes(name) || args.has(name) || value === "") {
      fail("runtime transport staging arguments are incomplete or duplicated");
    }
    args.set(name, value);
  }
  return args;
}

function inputDirectory(directory) {
  const requested = path.resolve(directory);
  const stat = fs.lstatSync(requested);
  if (!stat.isDirectory() || stat.isSymbolicLink()) {
    fail("runtime artifact input must be a non-symlink directory");
  }
  return fs.realpathSync(requested);
}

function readStableFile(file, label, maximumBytes) {
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
      fail(`${label} changed while being staged`);
    }
    return Object.freeze({ body, sha256: sha256(body), size: body.length });
  } finally {
    fs.closeSync(descriptor);
  }
}

function writeExclusive(file, body) {
  const descriptor = fs.openSync(file, "wx", 0o600);
  try {
    fs.writeFileSync(descriptor, body);
    fs.fchmodSync(descriptor, 0o644);
    fs.fsyncSync(descriptor);
  } finally {
    fs.closeSync(descriptor);
  }
}

export function stageRuntimeTransportFiles({
  inputDirectoryPath,
  loaded,
  outputDirectoryPath,
  publicRepo,
  runtimeVersion,
}) {
  if (!RUNTIME_VERSION.test(runtimeVersion)) {
    fail("runtime version must be canonical SemVer core syntax");
  }
  const input = inputDirectory(inputDirectoryPath);
  assertCurrentReleaseVersion(loaded.version);
  const output = path.resolve(outputDirectoryPath);
  if (fs.existsSync(output)) fail("runtime transport handoff output must not exist");
  fs.mkdirSync(output, { mode: 0o700 });
  let complete = false;
  try {
    const artifacts = [];
    for (const expected of HOSTED_RUNTIME_TRANSPORTS) {
      const sourceName = expected.metadata === "windows_x64"
        ? expected.name
        : expected.name.replace(/\.tar\.gz$/u, ".tar.zst");
      const source = readStableFile(
        path.join(input, sourceName),
        `${expected.metadata} runtime producer output`,
        1024 * 1024 * 1024,
      );
      const body = expected.metadata === "windows_x64"
        ? source.body
        : transcodeRuntimeTarZstd(source.body, `${expected.metadata} runtime`);
      writeExclusive(path.join(output, expected.name), body);
      if (sourceName !== expected.name) {
        writeExclusive(path.join(output, sourceName), source.body);
      }
      artifacts.push({
        metadata: expected.metadata,
        name: expected.name,
        path: expected.name,
        sha256: sha256(body),
        size_bytes: body.length,
        source_name: sourceName,
        source_path: sourceName,
        source_sha256: source.sha256,
        source_size_bytes: source.size,
      });
    }
    for (const name of MACOS_EVIDENCE) {
      const maximumBytes = name.endsWith(".notary-submit.json")
        ? 2 * 1024 * 1024
        : 1024 * 1024;
      writeExclusive(
        path.join(output, name),
        readStableFile(
          path.join(input, name),
          `macOS runtime evidence ${name}`,
          maximumBytes,
        ).body,
      );
    }
    const handoffValue = {
      contract: "ctx-runtime-transport-handoff",
      schema_version: 1,
      release_name: loaded.publication.release_name,
      public_source_commit: loaded.publicCommit,
      runtime_version: runtimeVersion,
      artifacts,
    };
    const handoffPath = path.join(output, HANDOFF_NAME);
    writeExclusive(handoffPath, Buffer.from(`${JSON.stringify(handoffValue)}\n`, "utf8"));
    const handoff = loadRuntimeTransportHandoff(handoffPath, loaded);
    verifyRuntimeTransportHandoff(publicRepo, handoff);
    const directoryDescriptor = fs.openSync(output, fs.constants.O_RDONLY | (fs.constants.O_DIRECTORY || 0));
    try { fs.fsyncSync(directoryDescriptor); } finally { fs.closeSync(directoryDescriptor); }
    complete = true;
    return Object.freeze({ handoff, handoffPath, output });
  } finally {
    if (!complete) fs.rmSync(output, { recursive: true });
  }
}

export function stageRuntimeTransportHandoff(argv) {
  const args = parseArgs(argv);
  return stageRuntimeTransportFiles({
    inputDirectoryPath: args.get("--runtime-artifact-dir"),
    loaded: loadHostedManagedPairPublication(args.get("--publication")),
    outputDirectoryPath: args.get("--output-dir"),
    publicRepo: args.get("--public-ctx-repo"),
    runtimeVersion: args.get("--runtime-version"),
  });
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const staged = stageRuntimeTransportHandoff(process.argv.slice(2));
    process.stdout.write(`${JSON.stringify({
      handoff: staged.handoffPath,
      public_source_commit: staged.handoff.publicCommit,
      release_name: staged.handoff.releaseName,
      runtime_handoff_sha256: staged.handoff.snapshot.sha256,
      status: "verified",
    })}\n`);
  } catch (error) {
    process.stderr.write(`runtime transport handoff staging failed: ${error.message}\n`);
    process.exitCode = 1;
  }
}
