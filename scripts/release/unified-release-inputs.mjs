import childProcess from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  CORE_CAPABILITY_PROTOCOL_FINGERPRINT, MAX_AGGREGATE_COMPONENT_BYTES,
  TARGET_IDS, COMPONENT_KINDS, canonicalJsonBytes, contractError, hashStableFile,
  readStableRegularFile, parseJsonStrict, sha256,
} from "./managed-pair-release-contract.mjs";

import { compareReleaseVersions } from "./release-version.cjs";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const MAX_INCOMING_BINARY_BYTES = 128 * 1024 * 1024;
const SOURCE_NAMES = Object.freeze({ "linux-arm64": "ctx-linux-aarch64", "linux-x64": "ctx",
  "macos-arm64": "ctx-macos-arm64", "macos-x64": "ctx-macos-x64", "windows-x64": "ctx.exe" });

// A projection has two historical roles, never two source builds. This pure
// constructor is also used by authored fixture tests; production admission is
// the exact public factory/handoff verifier in loadUnifiedReleaseInputs below.
export function projectUnifiedReleaseInputs({ candidate, matrix, sourceCommit, artifacts,
  handoffDigest, validationDigest, validationPolicy }) {
  if (!/^[0-9a-f]{40}$/u.test(sourceCommit) || /^0+$/u.test(sourceCommit)
      || !/^[0-9a-f]{64}$/u.test(handoffDigest) || !/^[0-9a-f]{64}$/u.test(validationDigest)
      || !["native-receipts-required-v1", "factory-only-human-override-v1"].includes(validationPolicy)
      || artifacts.size !== TARGET_IDS.length) contractError("unified public source binding is incomplete");
  const targets = [];
  let aggregateBytes = 0;
  for (const targetId of TARGET_IDS) {
    const target = matrix.targets.get(targetId);
    const selected = artifacts.get(targetId);
    if (target == null || selected == null || !/^[0-9a-f]{64}$/u.test(selected.buildFingerprint)) {
      contractError("unified public target is missing");
    }
    const artifact = hashStableFile(selected.path, targetId, MAX_INCOMING_BINARY_BYTES);
    if (artifact.digest !== selected.sha256 || artifact.sizeBytes !== selected.sizeBytes) {
      contractError("unified executable differs from its accepted public candidate");
    }
    aggregateBytes += artifact.sizeBytes * 2;
    if (aggregateBytes > MAX_AGGREGATE_COMPONENT_BYTES) contractError("legacy projection exceeds aggregate bound");
    const components = Object.fromEntries(COMPONENT_KINDS.map((kind) => {
      const artifactName = kind === "core" ? target.public_artifact : target.helper_artifact;
      return [kind, Object.freeze({ artifactName, artifactPath: artifact.path,
        artifactSourceIdentity: artifact.identity, digest: artifact.digest, sizeBytes: artifact.sizeBytes,
        objectKey: `sha256/${artifact.digest}/${artifactName}`,
        slot: target[`managed_pair_${kind}_slot`], buildIdentity: Object.freeze({
          component: kind, rust_target: target.public_rust_target, source_revision: sourceCommit,
          build_fingerprint: selected.buildFingerprint,
        }),
      })];
    }));
    targets.push(Object.freeze({ targetId, target, components: Object.freeze(components) }));
  }
  const acceptedPair = Object.freeze({ acceptance_receipt_sha256: validationDigest,
    candidate_digest: handoffDigest, protocol_fingerprint: handoffDigest,
    public_source_commit: sourceCommit, private_source_commit: sourceCommit });
  return Object.freeze({ targets: Object.freeze(targets), aggregateBytes, acceptedPair, validationPolicy,
    compatibility: Object.freeze({ invocation_fingerprint: matrix.digest,
      core_capability_fingerprint: CORE_CAPABILITY_PROTOCOL_FINGERPRINT }),
    snapshot: Object.freeze({ contract: "ctx-managed-pair-snapshot-v1", fingerprint: sha256(canonicalJsonBytes({
      kind: "ctx-unified-release-legacy-projection", source_commit: sourceCommit,
      candidate: handoffDigest, validation: validationDigest, release: candidate.release_name,
      target_matrix_sha256: matrix.digest,
      targets: targets.map(({ targetId, components }) => ({ id: targetId,
        sha256: components.core.digest, size_bytes: components.core.sizeBytes,
        build_fingerprint: components.core.buildIdentity.build_fingerprint })),
    })) }),
  });
}

export function loadUnifiedReleaseInputs({ factoryDir, handoffDir, handoffDigest, candidate, matrix }) {
  const root = path.resolve(factoryDir);
  const handoff = path.resolve(handoffDir);
  const sourceCommit = childProcess.execFileSync("git", ["-C", ROOT, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
  if (childProcess.execFileSync("git", ["-C", ROOT, "status", "--porcelain=v1", "--untracked-files=all"],
    { encoding: "utf8" }).trim() !== "") contractError("public release source checkout is dirty");
  const run = (script, args) => childProcess.execFileSync("python3", ["-I", "-B",
    path.join(ROOT, "scripts", script), ...args], { encoding: "utf8" }).trim();
  run("release/released-source-continuity.py", ["--public-repo", ROOT, "--source-commit", sourceCommit]);
  run("release/seal-linux-factory-candidate.py", ["--verify", "--candidate-dir", root, "--source-commit", sourceCommit]);
  const observed = run("release-sbom.py", ["verify-release", "--handoff-dir", handoff,
    "--expected-handoff-sha256", handoffDigest]);
  if (observed !== handoffDigest) contractError("public handoff authority differs from expected digest");
  const factoryBytes = readStableRegularFile(path.join(root, "ctx-release-factory.json"), "public factory", 16 * 1024 * 1024);
  const factory = parseJsonStrict(factoryBytes, "public factory");
  if (factory.source_commit !== sourceCommit || `v${factory.version}` !== candidate.release_name
      || compareReleaseVersions(factory.version, "1.5.0") < 0) {
    contractError("projection requires an exact unified 1.5-or-newer public factory");
  }
  const authority = parseJsonStrict(readStableRegularFile(path.join(handoff, "ctx-core-github-handoff.json"),
    "public handoff", 16 * 1024 * 1024), "public handoff");
  if (authority.source_commit !== sourceCommit || authority.factory_manifest.sha256 !== sha256(factoryBytes)) {
    contractError("factory and staged public handoff differ");
  }
  const validationBytes = readStableRegularFile(path.join(handoff, "release-validation.json"), "validation", 1024 * 1024);
  const validation = parseJsonStrict(validationBytes, "validation");
  if (sha256(validationBytes) !== authority.validation.sha256) contractError("validation identity differs");
  const files = new Map(factory.files.map((record) => [record.file, record]));
  const artifacts = new Map(TARGET_IDS.map((id) => {
    const name = SOURCE_NAMES[id];
    const file = files.get(name);
    const manifest = files.get(`${name}.candidate.json`);
    if (file == null || manifest == null) contractError("public factory target is incomplete");
    return [id, { path: path.join(root, name), sha256: file.sha256, sizeBytes: file.size_bytes,
      buildFingerprint: manifest.sha256 }];
  }));
  return projectUnifiedReleaseInputs({ candidate, matrix, sourceCommit, artifacts, handoffDigest,
    validationDigest: sha256(validationBytes), validationPolicy: validation.validation_policy });
}
