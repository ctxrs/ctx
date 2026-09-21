#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/release/ctx_cli_release_metadata_sign.sh --metadata PATH [--out PATH]
       [--semantic-artifact-dir PATH]
       [--candidate-manifest-handoff PATH --candidate-handoff-sha256 HEX
        --public-ctx-repo PATH]
       [--managed-pair-publication PATH --runtime-handoff PATH
        --semantic-artifact-dir PATH --candidate-manifest-handoff PATH
        --candidate-handoff-sha256 HEX --public-ctx-repo PATH]
       [--allow-legacy-pre-v0260-nonsemantic]

Signs standalone CLI release metadata with RSA-SHA256/PKCS#1 v1.5 and writes
the base64 signature expected by the hosted CLI installers. Metadata containing
semantic fields is regenerated from the final archives and compared byte-for-
byte with the metadata before signing.
Candidate-manifest digests are independently recomputed from the exact public
release-authority handoff and checked with the public release verifier before
the signing key is accessed.
Non-semantic metadata older than 0.26.0 requires the explicit legacy flag;
release 0.26.0 cannot bypass semantic artifact validation. Current stable (1.x
and later) signing requires the managed publication and complete runtime handoff,
with exactly five current runtime transports; bridge metadata requires retained B source.

Required environment:
  CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM
    or CTX_CLI_METADATA_SIGNING_PRIVATE_KEY
USAGE
}

metadata_path=""
signature_out=""
semantic_artifact_dir=""
candidate_manifest_handoff=""
candidate_handoff_sha256=""
managed_pair_publication=""
runtime_handoff=""
public_ctx_repo="${CTX_PUBLIC_CTX_REPO:-}"
allow_legacy_pre_v0260_nonsemantic=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --metadata)
      shift
      metadata_path="${1:-}"
      ;;
    --out)
      shift
      signature_out="${1:-}"
      ;;
    --semantic-artifact-dir)
      shift
      semantic_artifact_dir="${1:-}"
      ;;
    --candidate-manifest-handoff)
      shift
      candidate_manifest_handoff="${1:-}"
      ;;
    --candidate-handoff-sha256)
      shift
      candidate_handoff_sha256="${1:-}"
      ;;
    --managed-pair-publication)
      shift
      managed_pair_publication="${1:-}"
      ;;
    --runtime-handoff)
      shift
      runtime_handoff="${1:-}"
      ;;
    --public-ctx-repo)
      shift
      public_ctx_repo="${1:-}"
      ;;
    --allow-legacy-pre-v0260-nonsemantic)
      allow_legacy_pre_v0260_nonsemantic=1
      ;;
    -h|--help)
      usage
      exit 2
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
  shift
done

if [[ -z "$metadata_path" ]]; then
  echo "error: --metadata is required" >&2
  usage
  exit 2
fi
if [[ ! -f "$metadata_path" ]]; then
  echo "error: metadata file not found: $metadata_path" >&2
  exit 2
fi
if ! command -v node >/dev/null 2>&1; then
  echo "error: missing required command: node" >&2
  exit 2
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "error: missing required command: python3" >&2
  exit 2
fi
if ! command -v flock >/dev/null 2>&1; then
  echo "error: missing required command: flock" >&2
  exit 2
fi

metadata_abs="$(cd "$(dirname "$metadata_path")" && pwd)/$(basename "$metadata_path")"
signature_out="${signature_out:-${metadata_abs}.sig}"
signature_abs="$(mkdir -p "$(dirname "$signature_out")" && cd "$(dirname "$signature_out")" && pwd)/$(basename "$signature_out")"
signature_lock="$(dirname "$signature_abs")/.$(basename "$signature_abs").lock"
if [[ "$signature_abs" == "$metadata_abs" ]]; then
  echo "error: signature output must not overwrite release metadata" >&2
  exit 2
fi
semantic_artifact_abs=""
if [[ -n "$semantic_artifact_dir" ]]; then
  if [[ ! -d "$semantic_artifact_dir" ]]; then
    echo "error: semantic artifact directory not found: $semantic_artifact_dir" >&2
    exit 2
  fi
  semantic_artifact_abs="$(cd "$semantic_artifact_dir" && pwd)"
fi
candidate_manifest_handoff_abs=""
if [[ -n "$candidate_manifest_handoff" ]]; then
  if [[ -L "$candidate_manifest_handoff" || ! -d "$candidate_manifest_handoff" ]]; then
    echo "error: candidate manifest handoff must be a non-symlink directory: $candidate_manifest_handoff" >&2
    exit 2
  fi
  candidate_manifest_handoff_abs="$(cd "$candidate_manifest_handoff" && pwd)"
fi
if [[ -n "$candidate_manifest_handoff_abs" ]]; then
  if [[ ! "$candidate_handoff_sha256" =~ ^[0-9a-f]{64}$ || "$candidate_handoff_sha256" =~ ^0{64}$ ]]; then
    echo "error: --candidate-handoff-sha256 must be a nonzero lowercase SHA-256 digest" >&2
    exit 2
  fi
elif [[ -n "$candidate_handoff_sha256" ]]; then
  echo "error: --candidate-handoff-sha256 requires --candidate-manifest-handoff" >&2
  exit 2
fi
managed_pair_publication_abs=""
if [[ -n "$managed_pair_publication" ]]; then
  if [[ -L "$managed_pair_publication" || ! -f "$managed_pair_publication" ]]; then
    echo "error: managed-pair publication must be a non-symlink file: $managed_pair_publication" >&2
    exit 2
  fi
  managed_pair_publication_abs="$(cd "$(dirname "$managed_pair_publication")" && pwd)/$(basename "$managed_pair_publication")"
fi
runtime_handoff_abs=""
if [[ -n "$runtime_handoff" ]]; then
  if [[ -L "$runtime_handoff" || ! -f "$runtime_handoff" ]]; then
    echo "error: runtime handoff must be a non-symlink file: $runtime_handoff" >&2
    exit 2
  fi
  runtime_handoff_abs="$(cd "$(dirname "$runtime_handoff")" && pwd)/$(basename "$runtime_handoff")"
fi
if [[ -n "$managed_pair_publication_abs" && "$allow_legacy_pre_v0260_nonsemantic" == 1 ]]; then
  echo "error: managed-pair publication mode cannot be combined with legacy mode" >&2
  exit 2
fi
if [[ -n "$managed_pair_publication_abs" ]] &&
   [[ -z "$semantic_artifact_abs" || -z "$candidate_manifest_handoff_abs" ]] &&
   [[ -n "$semantic_artifact_abs" || -n "$candidate_manifest_handoff_abs" ]]; then
  echo "error: managed-pair semantic publication requires both --semantic-artifact-dir and --candidate-manifest-handoff" >&2
  exit 2
fi
public_ctx_repo_abs=""
if [[ -n "$public_ctx_repo" ]]; then
  if [[ ! -d "$public_ctx_repo" ]]; then
    echo "error: public ctx repository not found: $public_ctx_repo" >&2
    exit 2
  fi
  public_ctx_repo_abs="$(cd "$public_ctx_repo" && pwd)"
fi
if [[ -n "$managed_pair_publication_abs" ]]; then
  if [[ -z "$public_ctx_repo_abs" ]]; then
    echo "error: managed-pair publication requires --public-ctx-repo" >&2
    exit 2
  fi
  if [[ -z "$runtime_handoff_abs" || -z "$semantic_artifact_abs" || -z "$candidate_manifest_handoff_abs" ]]; then
    echo "error: managed-pair publication requires the complete runtime, Semantic, and candidate handoff set" >&2
    exit 2
  fi
elif [[ -n "$runtime_handoff_abs" ]]; then
  echo "error: runtime handoff requires --managed-pair-publication" >&2
  exit 2
fi

CTX_CLI_METADATA_PATH="$metadata_abs" \
CTX_CLI_METADATA_SIGNATURE_OUT="$signature_abs" \
CTX_CLI_METADATA_PYTHON="$(command -v python3)" \
CTX_CLI_METADATA_VALIDATOR="$ROOT/scripts/release/semantic_runtime_metadata.py" \
CTX_CLI_CANDIDATE_MANIFEST_CONTRACT="$ROOT/scripts/release/release-candidate-manifest-contract.cjs" \
CTX_CLI_SEMANTIC_ARTIFACT_DIR="$semantic_artifact_abs" \
CTX_CLI_CANDIDATE_MANIFEST_HANDOFF="$candidate_manifest_handoff_abs" \
CTX_CLI_CANDIDATE_HANDOFF_SHA256="$candidate_handoff_sha256" \
CTX_CLI_MANAGED_PAIR_PUBLICATION="$managed_pair_publication_abs" \
CTX_CLI_MANAGED_PAIR_VALIDATOR="$ROOT/scripts/release/validate-hosted-managed-pair-metadata.mjs" \
CTX_CLI_RUNTIME_HANDOFF="$runtime_handoff_abs" \
CTX_CLI_PUBLIC_CTX_REPO="$public_ctx_repo_abs" \
CTX_CLI_ALLOW_LEGACY_PRE_V0260_NONSEMANTIC="$allow_legacy_pre_v0260_nonsemantic" \
flock -x "$signature_lock" \
node - <<'NODE'
const childProcess = require("child_process");
const crypto = require("crypto");
const fs = require("fs");
const path = require("path");

const metadataPath = process.env.CTX_CLI_METADATA_PATH;
const signatureOut = process.env.CTX_CLI_METADATA_SIGNATURE_OUT;
const python = process.env.CTX_CLI_METADATA_PYTHON;
const validator = process.env.CTX_CLI_METADATA_VALIDATOR;
const candidateManifestContract = require(
  process.env.CTX_CLI_CANDIDATE_MANIFEST_CONTRACT,
);
const semanticArtifactDir = process.env.CTX_CLI_SEMANTIC_ARTIFACT_DIR;
const candidateManifestHandoff =
  process.env.CTX_CLI_CANDIDATE_MANIFEST_HANDOFF;
const candidateHandoffSha256 = process.env.CTX_CLI_CANDIDATE_HANDOFF_SHA256;
const managedPairPublication = process.env.CTX_CLI_MANAGED_PAIR_PUBLICATION;
const managedPairValidator = process.env.CTX_CLI_MANAGED_PAIR_VALIDATOR;
const runtimeHandoff = process.env.CTX_CLI_RUNTIME_HANDOFF;
const publicCtxRepo = process.env.CTX_CLI_PUBLIC_CTX_REPO;
const allowLegacyPreV0260Nonsemantic =
  process.env.CTX_CLI_ALLOW_LEGACY_PRE_V0260_NONSEMANTIC === "1";

function sameMetadataIdentity(expected, actual) {
  return [
    "dev",
    "ino",
    "mode",
    "nlink",
    "size",
    "mtimeNs",
    "ctimeNs",
  ].every((field) => expected[field] === actual[field]);
}

let metadataDescriptor;
let metadataIdentity;
let metadata;
try {
  metadataDescriptor = fs.openSync(
    metadataPath,
    fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0),
  );
  metadataIdentity = fs.fstatSync(metadataDescriptor, { bigint: true });
  if (!metadataIdentity.isFile()) {
    throw new Error("metadata input is not a regular file");
  }
  metadata = fs.readFileSync(metadataDescriptor);
  const afterRead = fs.fstatSync(metadataDescriptor, { bigint: true });
  const pathIdentity = fs.lstatSync(metadataPath, { bigint: true });
  if (
    BigInt(metadata.length) !== metadataIdentity.size ||
    !sameMetadataIdentity(metadataIdentity, afterRead) ||
    !sameMetadataIdentity(metadataIdentity, pathIdentity)
  ) {
    throw new Error("metadata input changed while its signing snapshot was read");
  }
} catch (err) {
  if (metadataDescriptor !== undefined) fs.closeSync(metadataDescriptor);
  console.error(`error: could not snapshot release metadata: ${err.message}`);
  process.exit(1);
}

function assertMetadataUnchanged(stage) {
  let descriptorIdentity;
  let pathIdentity;
  try {
    descriptorIdentity = fs.fstatSync(metadataDescriptor, { bigint: true });
    pathIdentity = fs.lstatSync(metadataPath, { bigint: true });
  } catch (err) {
    throw new Error(`metadata input changed ${stage}: ${err.message}`);
  }
  if (
    !sameMetadataIdentity(metadataIdentity, descriptorIdentity) ||
    !sameMetadataIdentity(metadataIdentity, pathIdentity)
  ) {
    throw new Error(`metadata input changed ${stage}`);
  }
}

const validationArgs = [
  validator,
  "validate-signing",
  "--metadata-stdin",
  "--metadata-label",
  metadataPath,
];
if (semanticArtifactDir) {
  validationArgs.push("--semantic-artifact-dir", semanticArtifactDir);
}
if (allowLegacyPreV0260Nonsemantic) {
  validationArgs.push("--allow-legacy-pre-v0260-nonsemantic");
}
if (managedPairPublication) {
  validationArgs.push("--allow-managed-pair-nonsemantic");
}
const validationEnvironment = { ...process.env };
delete validationEnvironment.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM;
delete validationEnvironment.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY;
const validation = childProcess.spawnSync(python, validationArgs, {
  env: validationEnvironment,
  input: metadata,
  stdio: ["pipe", "inherit", "inherit"],
});
if (validation.error) {
  fs.closeSync(metadataDescriptor);
  console.error(`error: semantic metadata validation failed: ${validation.error.message}`);
  process.exit(1);
}
if (validation.status !== 0) {
  fs.closeSync(metadataDescriptor);
  process.exit(validation.status ?? 1);
}
try {
  assertMetadataUnchanged("during semantic validation");
} catch (err) {
  fs.closeSync(metadataDescriptor);
  console.error(`error: ${err.message}`);
  process.exit(1);
}

// The exact current release/version and five runtime slots are validated before private-key access.
if (managedPairPublication) {
  const managedPairEnvironment = { ...process.env };
  delete managedPairEnvironment.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM;
  delete managedPairEnvironment.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY;
  const managedPairValidationArgs = [
    managedPairValidator,
    "--publication",
    managedPairPublication,
    "--runtime-handoff",
    runtimeHandoff,
    "--public-ctx-repo",
    publicCtxRepo,
    "--semantic-artifact-dir",
    semanticArtifactDir,
    "--candidate-manifest-handoff",
    candidateManifestHandoff,
    "--candidate-handoff-sha256",
    candidateHandoffSha256,
  ];
  const managedPairValidation = childProcess.spawnSync(
    process.execPath,
    managedPairValidationArgs,
    {
      env: managedPairEnvironment,
      input: metadata,
      stdio: ["pipe", "inherit", "inherit"],
    },
  );
  if (managedPairValidation.error) {
    fs.closeSync(metadataDescriptor);
    console.error(
      `error: managed-pair metadata validation failed: ${managedPairValidation.error.message}`,
    );
    process.exit(1);
  }
  if (managedPairValidation.status !== 0) {
    fs.closeSync(metadataDescriptor);
    process.exit(managedPairValidation.status ?? 1);
  }
  try {
    assertMetadataUnchanged("during managed-pair publication validation");
  } catch (err) {
    fs.closeSync(metadataDescriptor);
    console.error(`error: ${err.message}`);
    process.exit(1);
  }
}

let metadataValues;
try {
  metadataValues = {};
  for (const [index, rawLine] of metadata.toString("utf8").split(/\r?\n/).entries()) {
    if (rawLine === "" || /^[ \t]*#/.test(rawLine)) continue;
    const equals = rawLine.indexOf("=");
    if (equals <= 0) {
      throw new Error(`metadata line ${index + 1} is not KEY=value`);
    }
    const key = rawLine.slice(0, equals);
    const value = rawLine.slice(equals + 1);
    if (Object.hasOwn(metadataValues, key)) {
      throw new Error(`metadata repeats key ${key}`);
    }
    metadataValues[key] = value;
  }
  const hasSemanticMetadata = Object.keys(metadataValues).some((key) =>
    key.startsWith("CTX_RELEASE_SEMANTIC_"),
  );
  const hasCandidateManifestMetadata = Object.keys(metadataValues).some((key) =>
    key.startsWith("CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_"),
  );
  if (hasSemanticMetadata && !hasCandidateManifestMetadata) {
    throw new Error(
      "semantic release metadata requires the complete candidate manifest digest matrix",
    );
  }
  if (hasCandidateManifestMetadata) {
    if (!candidateManifestHandoff) {
      throw new Error(
        "semantic release metadata requires --candidate-manifest-handoff",
      );
    }
    if (!publicCtxRepo) {
      throw new Error("semantic release metadata requires --public-ctx-repo");
    }
    const handoffKey =
      candidateManifestContract.CORE_GITHUB_HANDOFF_METADATA_KEY;
    if (metadataValues[handoffKey] !== candidateHandoffSha256) {
      throw new Error(
        `release metadata ${handoffKey} does not match --candidate-handoff-sha256`,
      );
    }
    candidateManifestContract.verifyCandidateManifestHandoff({
      values: metadataValues,
      label: metadataPath,
      handoffDir: candidateManifestHandoff,
      publicRepo: publicCtxRepo,
      python,
      environment: process.env,
    });
  } else if (candidateManifestHandoff) {
    throw new Error(
      "candidate manifest verification inputs require current semantic release metadata",
    );
  }
  assertMetadataUnchanged("during candidate manifest validation");
} catch (err) {
  fs.closeSync(metadataDescriptor);
  console.error(`error: candidate manifest validation failed: ${err.message}`);
  process.exit(1);
}

let privateKey =
  process.env.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM ||
  process.env.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY ||
  "";
if (!privateKey) {
  fs.closeSync(metadataDescriptor);
  console.error(
    "error: missing required env var: " +
      "CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM or " +
      "CTX_CLI_METADATA_SIGNING_PRIVATE_KEY",
  );
  process.exit(2);
}

if (privateKey.includes("\\n") && !privateKey.includes("\n")) {
  privateKey = privateKey.replace(/\\n/g, "\n");
}

let signature;
try {
  signature = crypto.sign("RSA-SHA256", metadata, {
    key: privateKey,
    padding: crypto.constants.RSA_PKCS1_PADDING,
  });
} catch (err) {
  console.error(`error: failed to sign metadata: ${err.message}`);
  process.exit(1);
}

const temporary =
  `${signatureOut}.${process.pid}.` +
  `${crypto.randomBytes(12).toString("hex")}.tmp`;
const signatureDirectory = path.dirname(signatureOut);
let descriptor;
let directoryDescriptor;
let published = false;
try {
  descriptor = fs.openSync(temporary, "wx", 0o600);
  fs.writeFileSync(descriptor, `${signature.toString("base64")}\n`, {
    encoding: "utf8",
  });
  fs.fchmodSync(descriptor, 0o644);
  fs.fsyncSync(descriptor);
  fs.closeSync(descriptor);
  descriptor = undefined;
  assertMetadataUnchanged("before signature publication");
  directoryDescriptor = fs.openSync(
    signatureDirectory,
    fs.constants.O_RDONLY | (fs.constants.O_DIRECTORY || 0),
  );
  fs.renameSync(temporary, signatureOut);
  published = true;
  fs.fsyncSync(directoryDescriptor);
  assertMetadataUnchanged("during signature publication");
  fs.closeSync(directoryDescriptor);
  directoryDescriptor = undefined;
} catch (err) {
  if (descriptor !== undefined) fs.closeSync(descriptor);
  if (published) {
    try {
      fs.unlinkSync(signatureOut);
      if (directoryDescriptor === undefined) {
        directoryDescriptor = fs.openSync(
          signatureDirectory,
          fs.constants.O_RDONLY | (fs.constants.O_DIRECTORY || 0),
        );
      }
      fs.fsyncSync(directoryDescriptor);
    } catch (unlinkError) {
      if (unlinkError.code !== "ENOENT") {
        console.error(`error: could not retract invalid signature: ${unlinkError.message}`);
      }
    }
  }
  try {
    fs.unlinkSync(temporary);
  } catch (unlinkError) {
    if (unlinkError.code !== "ENOENT") {
      console.error(`error: could not clean temporary signature: ${unlinkError.message}`);
    }
  }
  if (directoryDescriptor !== undefined) fs.closeSync(directoryDescriptor);
  console.error(`error: failed to publish metadata signature atomically: ${err.message}`);
  fs.closeSync(metadataDescriptor);
  process.exit(1);
}
fs.closeSync(metadataDescriptor);
NODE

printf 'signed %s -> %s\n' "$metadata_abs" "$signature_abs"
