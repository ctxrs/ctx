import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

export function materializeReleaseTestSources(entryUrl) {
  if (!process.env.TEST_SRCDIR && !process.env.TEST_TARGET && !process.env.RUNFILES_DIR) {
    return new URL(entryUrl);
  }
  const sourceRoot = fileURLToPath(new URL("../../../", entryUrl));
  if (path.basename(sourceRoot) !== "release_test_source_tree") {
    throw new Error("release tests require the declared release_test_source_tree");
  }
  const root = fs.mkdtempSync(path.join(process.env.TEST_TMPDIR ?? os.tmpdir(), "ctx-release-test-sources-"));
  const cleanup = () => fs.rmSync(root, { recursive: true, force: true });
  try {
    // Bazel expands even TreeArtifact inputs into sandbox leaf symlinks.
    // Existing destination directories keep cleanup writable; file modes stay immutable.
    for (const relative of fs.readdirSync(sourceRoot, { recursive: true })) {
      if (fs.statSync(path.join(sourceRoot, relative)).isDirectory()) {
        fs.mkdirSync(path.join(root, relative), { recursive: true, mode: 0o700 });
      }
    }
    fs.cpSync(sourceRoot, root, { recursive: true, dereference: true });
  } catch (error) {
    cleanup();
    throw error;
  }
  after(cleanup);
  return pathToFileURL(path.join(root, path.relative(sourceRoot, fileURLToPath(entryUrl))));
}

export const SOURCE_COMMIT = "a".repeat(40);
export const RELEASE_NAME = "v1.3.3";
export const RUNTIME_TRANSPORTS = [
  ["linux_x64", "ctx-onnxruntime-linux-x64.tar.gz", "ctx-onnxruntime-linux-x64.tar.zst"],
  ["linux_aarch64", "ctx-onnxruntime-linux-aarch64.tar.gz", "ctx-onnxruntime-linux-aarch64.tar.zst"],
  ["windows_x64", "ctx-onnxruntime-windows-x64.zip", "ctx-onnxruntime-windows-x64.zip"],
  ["macos_x64", "ctx-onnxruntime-macos-x64.tar.gz", "ctx-onnxruntime-macos-x64.tar.zst"],
  ["macos_arm64", "ctx-onnxruntime-macos-arm64.tar.gz", "ctx-onnxruntime-macos-arm64.tar.zst"],
];
export function writeRuntimeHandoff(
  root,
  mutate = (value) => value,
  sourceCommit = SOURCE_COMMIT,
) {
  const artifacts = RUNTIME_TRANSPORTS.map(([metadata, name, sourceName], index) => {
    const sourceBody = Buffer.from(`producer-${index}-${metadata}\n`, "utf8");
    const body = sourceName === name
      ? sourceBody
      : Buffer.from(`transport-${index}-${metadata}\n`, "utf8");
    fs.writeFileSync(path.join(root, sourceName), sourceBody, { mode: 0o600 });
    fs.writeFileSync(path.join(root, name), body, { mode: 0o600 });
    return {
      metadata,
      name,
      path: name,
      sha256: crypto.createHash("sha256").update(body).digest("hex"),
      size_bytes: body.length,
      source_name: sourceName,
      source_path: sourceName,
      source_sha256: crypto.createHash("sha256").update(sourceBody).digest("hex"),
      source_size_bytes: sourceBody.length,
    };
  });
  const value = mutate({
    contract: "ctx-runtime-transport-handoff",
    schema_version: 1,
    release_name: RELEASE_NAME,
    public_source_commit: sourceCommit,
    runtime_version: "1.27.0",
    artifacts,
  });
  const handoff = path.join(root, "ctx-runtime-transport-handoff-v1.json");
  fs.writeFileSync(handoff, `${JSON.stringify(value)}\n`, { mode: 0o600 });
  return handoff;
}

export function syntheticLoaded(sourceCommit = SOURCE_COMMIT) {
  const targets = new Map();
  for (const [id, metadata] of [
    ["linux-arm64", "linux_aarch64"],
    ["linux-x64", "linux_x64"],
    ["macos-arm64", "macos_arm64"],
    ["macos-x64", "macos_x64"],
    ["windows-x64", "windows_x64"],
  ]) {
    const coreSha = crypto.createHash("sha256").update(`core-${id}`).digest("hex");
    const companionSha = crypto.createHash("sha256").update(`companion-${id}`).digest("hex");
    targets.set(id, {
      core: {
        artifact: { sha256: coreSha },
        identity: { object_key: `sha256/${coreSha}/ctx-${id}` },
      },
      companion: {
        artifact: { sha256: companionSha },
        identity: { object_key: `sha256/${companionSha}/ctx-pro-${id}` },
      },
      manifestRecord: { name: `ctx-managed-pair-${id}.json` },
    });
  }
  return {
    baseUrl: "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/stable/1.3.3",
    publicCommit: sourceCommit,
    publication: { release_name: RELEASE_NAME },
    targets,
    version: "1.3.3",
  };
}
