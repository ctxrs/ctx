#!/usr/bin/env bash
set -euo pipefail

if [[ $# -eq 0 && -z "${TEST_SRCDIR:-}${TEST_TARGET:-}${RUNFILES_DIR:-}" ]]; then
  set -- "$(dirname "${BASH_SOURCE[0]}")/../../.." "$(command -v node)"
fi
[[ $# -eq 2 ]] || { echo "usage: $0 SOURCE_ROOT NODE_BINARY" >&2; exit 64; }
# Materialized declared data keeps real no-follow release checks meaningful.
node_bin="$(cd "$(dirname "$2")" && pwd -P)/$(basename "$2")"
[[ -f "$node_bin" && -x "$node_bin" ]] || { echo "declared Node is unavailable" >&2; exit 69; }
ROOT="$(cd "$1" && pwd -P)"
[[ -f "$ROOT/scripts/release/release-contract.cjs" ]] || { echo "declared release source is missing" >&2; exit 66; }
cd "$ROOT"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: missing required command: $1" >&2
    exit 1
  }
}

tmp="$(mktemp -d "${TEST_TMPDIR:-${TMPDIR:-/tmp}}"/ctx-public-release-contract.XXXXXX)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin"
ln -s "$node_bin" "$tmp/bin/node"
export PATH="$tmp/bin:$PATH"

need_cmd git
need_cmd node
need_cmd python3
need_cmd zstd

# All fixtures, subprocess state and disposable signing keys stay in this test root.
for variable in "${!CTX_@}" "${!GIT_@}"; do
  [[ -z "$variable" ]] || unset "$variable"
done
unset NODE_OPTIONS NODE_PATH PYTHONPATH PYTHONHOME BASH_ENV ENV
export HOME="$tmp/home" XDG_CONFIG_HOME="$tmp/config" XDG_DATA_HOME="$tmp/data"
export XDG_STATE_HOME="$tmp/state" XDG_CACHE_HOME="$tmp/cache" XDG_RUNTIME_DIR="$tmp/runtime"
export TMPDIR="$tmp/tmp" CTX_DATA_ROOT="$tmp/ctx" CODEX_HOME="$tmp/codex"
export CLAUDE_CONFIG_DIR="$tmp/claude" GNUPGHOME="$tmp/gnupg"
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0
export PYTHONDONTWRITEBYTECODE=1
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" \
  "$XDG_CACHE_HOME" "$XDG_RUNTIME_DIR" "$TMPDIR" "$CTX_DATA_ROOT" \
  "$CODEX_HOME" "$CLAUDE_CONFIG_DIR" "$GNUPGHOME"

contract_root="$tmp/contract-root"
repo="$tmp/ctx"
artifacts="$tmp/artifacts"
release_dir="$tmp/releases/stable/1.5.0"
stable_metadata="$tmp/functions/v2/releases/stable/ctx-release-metadata.env"
original_metadata="$tmp/functions/v1/releases/stable/ctx-release-metadata.env"
versioned_metadata="$release_dir/ctx-release-metadata.env"
private_key="$tmp/metadata-signing-private.pem"
public_key="$tmp/metadata-signing-public.pem"
evidence="$tmp/public-cli-release-contract.json"
candidate_artifacts="$tmp/github-release-assets"
candidate_manifest="$candidate_artifacts/SHA256SUMS"
candidate_authority="$tmp/github-release-authority"
semantic_metadata="$tmp/semantic-runtime-metadata.env"
semantic_archive_backup="$tmp/semantic-archive-backup"
authority_git_bin="$tmp/authority-git-bin"
real_git="$(command -v git)"
mkdir -p \
  "$contract_root/scripts/release" \
  "$contract_root/services/install-site/src" \
  "$repo/crates/ctx-cli/src" \
  "$repo/crates/ctx-upgrade-engine/src/upgrade" \
  "$repo/scripts" \
  "$artifacts" \
  "$candidate_artifacts" \
  "$candidate_authority" \
  "$release_dir" \
  "$(dirname "$stable_metadata")" \
  "$(dirname "$original_metadata")" \
  "$authority_git_bin"
cat >"$authority_git_bin/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ -n "${CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM:-}" || \
  -n "${CTX_CLI_METADATA_SIGNING_PRIVATE_KEY:-}" ]]; then
  echo "git inherited a metadata signing key" >&2
  exit 97
fi
if [[ "${1:-}" == "-C" && "${3:-}" == "merge-base" && \
  "${4:-}" == "--is-ancestor" && \
  "${5:-}" == "4eb7234af45b568a4200e7331570d9056a1c5cdd" ]]; then
  [[ "${CTX_TEST_REJECT_PUBLIC_AUTHORITY_DESCENDANT:-0}" != "1" ]]
  exit
fi
# Only the remote discovery boundary is replaced; the real continuity checker
# resolves annotated objects and checks patches in this isolated Git history.
if [[ "${1:-}" == "-C" && "${3:-}" == "ls-remote" &&
  "${4:-}" == "--tags" && "${5:-}" == "https://github.com/ctxrs/ctx.git" ]]; then
  "${CTX_TEST_REAL_GIT:?}" -C "$2" show-ref --tags --dereference | tr ' ' '\t'
  exit
fi
exec "${CTX_TEST_REAL_GIT:?}" "$@"
SH
chmod 755 "$authority_git_bin/git"
cp scripts/release/frozen-cli-bridge.cjs "$contract_root/scripts/release/"
cp scripts/release/managed-pair-release-io.mjs "$contract_root/scripts/release/"
cp scripts/release/release-version.cjs "$contract_root/scripts/release/"
cp scripts/release/release-contract.cjs "$contract_root/scripts/release/"
cp scripts/release/release-contract-managed-pair.cjs "$contract_root/scripts/release/"
cp scripts/release/release-candidate-manifest-contract.cjs \
  "$contract_root/scripts/release/"
cp scripts/release/released-source-continuity.py "$contract_root/scripts/release/"
# Fixture policy applies only to the authored temporary history.
printf '%s\n' '{"minimum_release":"0.0.0","required_patches":[],"dispositions":{}}' \
  >"$contract_root/scripts/release/released-source-continuity.json"
cp scripts/release/semantic_runtime_metadata.py "$contract_root/scripts/release/"
cp scripts/release/semantic_runtime_archive.py "$contract_root/scripts/release/"
cp scripts/release/release-contract-semantic.cjs "$contract_root/scripts/release/"
cp scripts/release/semantic-runtime-layout-v1.json "$contract_root/scripts/release/"
cp services/install-site/package.json "$contract_root/services/install-site/"
cp services/install-site/src/cli-install-script.js "$contract_root/services/install-site/src/"
cp services/install-site/src/cli-install-powershell-script.js "$contract_root/services/install-site/src/"
# This private working tree receives authored keys, dispositions, and Semantic pins.
find -P "$contract_root" -type f -exec chmod u+w {} +
if grep -F "CTX_PUBLIC_RELEASE_METADATA_PUBLIC_KEY_PEM" \
  "$contract_root/scripts/release/release-contract.cjs" >/dev/null; then
  echo "release contract unexpectedly permits an ambient metadata verification key" >&2
  exit 1
fi

cat >"$repo/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/ctx-cli"]

[workspace.package]
version = "1.5.0"
EOF

cat >"$repo/crates/ctx-cli/Cargo.toml" <<'EOF'
[package]
name = "ctx"
version.workspace = true
edition = "2021"
EOF

cat >"$repo/scripts/release-sbom.py" <<'PY'
#!/usr/bin/env python3
import hashlib
import os
from pathlib import Path
import sys

expected_names = {
    "SHA256SUMS",
    "ctx.candidate.json",
    "ctx.candidate.json.sha256",
    "ctx-core-github-handoff.json",
    "ctx-core-github-handoff.json.sha256",
    "ctx-core.release-complete.json",
    "ctx-linux-aarch64.candidate.json",
    "ctx-linux-aarch64.candidate.json.sha256",
    "ctx-macos-arm64.candidate.json",
    "ctx-macos-arm64.candidate.json.sha256",
    "ctx-macos-x64.candidate.json",
    "ctx-macos-x64.candidate.json.sha256",
    "ctx.exe",
    "ctx.exe.build-info.json",
    "ctx.exe.candidate.json",
    "ctx.exe.candidate.json.sha256",
    "ctx.exe.cdx.json",
    "ctx.exe.size.json",
    "ctx.exe.third-party-notices.txt",
    "ctx-release-factory.json",
    "release-validation.json",
    "normal-ci.json",
    "windows-authenticode.json",
}
if os.environ.get("CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM") or os.environ.get(
    "CTX_CLI_METADATA_SIGNING_PRIVATE_KEY"
):
    raise SystemExit("release verifier inherited a metadata signing key")
if len(sys.argv) != 6 or sys.argv[1:3] != ["verify-release", "--handoff-dir"] or sys.argv[4] != "--expected-handoff-sha256":
    raise SystemExit("wrong release verifier interface")
handoff = Path(sys.argv[3])
expected = sys.argv[5]
if {entry.name for entry in handoff.iterdir()} != expected_names:
    raise SystemExit("release authority handoff does not have the exact production inventory")
actual = hashlib.sha256((handoff / "ctx-core-github-handoff.json").read_bytes()).hexdigest()
if actual != expected:
    raise SystemExit("Core GitHub handoff digest does not match expected digest")
document = __import__("json").loads(
    (handoff / "ctx-core-github-handoff.json").read_text(encoding="utf-8")
)
for record in document["candidate_manifests"]:
    digest = hashlib.sha256((handoff / record["file"]).read_bytes()).hexdigest()
    if digest != record["sha256"]:
        raise SystemExit("Core GitHub handoff does not bind exact candidate manifest")
print(actual)
PY

PRIVATE_KEY_PATH="$private_key" PUBLIC_KEY_PATH="$public_key" node - <<'NODE'
const crypto = require("node:crypto");
const fs = require("node:fs");

const { publicKey, privateKey } = crypto.generateKeyPairSync("rsa", {
  modulusLength: 2048,
  publicExponent: 0x10001,
});
fs.writeFileSync(
  process.env.PRIVATE_KEY_PATH,
  privateKey.export({ format: "pem", type: "pkcs8" }),
  { mode: 0o600 },
);
fs.writeFileSync(
  process.env.PUBLIC_KEY_PATH,
  publicKey.export({ format: "pem", type: "spki" }),
);
NODE

write_fixture_upgrade_key() {
  PUBLIC_KEY_PATH="$public_key" \
  INSTALLER_PATH="$contract_root/services/install-site/src/cli-install-script.js" \
  POWERSHELL_PATH="$contract_root/services/install-site/src/cli-install-powershell-script.js" \
  UPGRADE_RS_PATH="$repo/crates/ctx-upgrade-engine/src/upgrade/metadata.rs" node - <<'NODE'
const crypto = require("node:crypto");
const fs = require("node:fs");

const publicKey = fs.readFileSync(process.env.PUBLIC_KEY_PATH, "utf8").trim();
const key = crypto.createPublicKey(publicKey);
const jwk = key.export({ format: "jwk" });
const pkcs1 = key.export({ format: "pem", type: "pkcs1" }).trim();
fs.writeFileSync(
  process.env.INSTALLER_PATH,
  `const DEFAULT_METADATA_PUBLIC_KEY_PEM = \`${publicKey}\`;\nexport const CLI_METADATA_PUBLIC_KEY_PEM = DEFAULT_METADATA_PUBLIC_KEY_PEM;\n`,
);
fs.writeFileSync(
  process.env.POWERSHELL_PATH,
  `const DEFAULT_METADATA_PUBLIC_KEY_MODULUS_BASE64URL = "${jwk.n}";\n` +
    `const DEFAULT_METADATA_PUBLIC_KEY_EXPONENT_BASE64URL = "${jwk.e}";\n`,
);
fs.writeFileSync(
  process.env.UPGRADE_RS_PATH,
  `const RELEASE_METADATA_PUBLIC_KEY_PEM: &str = r#"${pkcs1}"#;\n`,
);
NODE
}

write_fixture_upgrade_key

git -C "$repo" init -q
git -C "$repo" checkout -q -b main
git -C "$repo" config user.email "ctx-release-contract@example.test"
git -C "$repo" config user.name "ctx release contract"
git -C "$repo" add \
  Cargo.toml \
  crates/ctx-cli/Cargo.toml \
  crates/ctx-upgrade-engine/src/upgrade/metadata.rs \
  scripts/release-sbom.py
git -C "$repo" commit -q -m "fixture public ctx"
source_commit="$(git -C "$repo" rev-parse HEAD)"
git -C "$repo" tag -a v1.3.2 -m 'fixture released source'

cat >"$artifacts/ctx" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
  echo "ctx 1.5.0"
  exit 0
fi
exit 0
EOF
chmod 755 "$artifacts/ctx"
cat >"$artifacts/ctx-linux-aarch64" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
  echo "ctx 1.5.0"
  exit 0
fi
exit 0
EOF
chmod 755 "$artifacts/ctx-linux-aarch64"
printf 'macos arm64 fixture\n' >"$artifacts/ctx-macos-arm64"
printf 'macos x64 fixture\n' >"$artifacts/ctx-macos-x64"
printf 'windows x64 fixture\n' >"$artifacts/ctx.exe"
signed_semantic_artifacts=(
  ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz
  ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz
  ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz
  ctx-windowsml-windows-x64.zip
  ctx-onnxruntime-linux-x64-cuda12.tar.zst
  ctx-onnxruntime-linux-x64.tar.zst
  ctx-onnxruntime-linux-aarch64.tar.zst
  ctx-onnxruntime-macos-arm64.tar.zst
  ctx-onnxruntime-macos-x64.tar.zst
)
candidate_runtime_artifacts=(
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-linux-aarch64.tar.gz
  ctx-onnxruntime-macos-arm64.tar.gz
  ctx-onnxruntime-macos-x64.tar.gz
  ctx-onnxruntime-windows-x64.zip
)
ROOT="$contract_root" ARTIFACT_DIR="$artifacts" python3 - <<'PY'
import gzip
import hashlib
import io
import json
import os
import subprocess
import tarfile
import zipfile
from pathlib import Path

root = Path(os.environ["ROOT"])
artifact_dir = Path(os.environ["ARTIFACT_DIR"])
layout_path = root / "scripts/release/semantic-runtime-layout-v1.json"
layout = json.loads(layout_path.read_text())
assets = {
    asset["artifact"]: asset
    for asset in layout["assets"].values()
}

for artifact, asset in assets.items():
    prefix = asset["path_prefix"]
    relative = list(asset["exact_paths"])
    relative.extend(f"{value}fixture.bin" for value in asset["tree_prefixes"])
    names = [f"{prefix}/{name}" if prefix else name for name in relative]
    destination = artifact_dir / artifact
    if asset["format"] == "zip":
        with zipfile.ZipFile(destination, "w", zipfile.ZIP_DEFLATED) as bundle:
            for name in names:
                bundle.writestr(name, f"fixture {artifact} {name}\n".encode())
    else:
        if asset["format"] == "tar.zst":
            tar_destination = destination.with_name(f"{destination.name}.tar")
            mode = "w"
        else:
            tar_destination = destination
            mode = "w:gz" if asset["format"] == "tar.gz" else "w:xz"
        with tarfile.open(tar_destination, mode) as bundle:
            for name in names:
                body = f"fixture {artifact} {name}\n".encode()
                member = tarfile.TarInfo(name)
                member.mode = 0o644
                member.size = len(body)
                bundle.addfile(member, io.BytesIO(body))
        if asset["format"] == "tar.zst":
            subprocess.run(
                ["zstd", "-q", "-f", "-T1", str(tar_destination), "-o", str(destination)],
                check=True,
            )
            tar_destination.unlink()

for asset in assets.values():
    if asset["format"] != "tar.zst" or asset["backend"] != "ort-cpu":
        continue
    source = artifact_dir / asset["artifact"]
    destination = artifact_dir / (
        asset["artifact"].removesuffix(".tar.zst") + ".tar.gz"
    )
    expanded = subprocess.run(
        ["zstd", "-q", "-d", "-c", str(source)],
        check=True,
        stdout=subprocess.PIPE,
    ).stdout
    with destination.open("wb") as raw_output:
        with gzip.GzipFile(
            filename="",
            mode="wb",
            fileobj=raw_output,
            compresslevel=9,
            mtime=0,
        ) as output:
            output.write(expanded)

pins = layout["publication_pins"]
for model_pin_name in ("model", "accelerator_model"):
    model_pin = pins[model_pin_name]
    model_asset = layout["assets"][model_pin["asset_id"]]
    for relative, pin in model_pin["runtime_files"].items():
        full_path = f"{model_asset['path_prefix']}/{relative}"
        body = f"fixture {model_asset['artifact']} {full_path}\n".encode()
        pin["size"] = len(body)
        pin["sha256"] = hashlib.sha256(body).hexdigest()

coreml_asset = layout["assets"][pins["coreml"]["asset_id"]]
coreml_archive = artifact_dir / coreml_asset["artifact"]
pins["coreml"]["archive_sha256"] = hashlib.sha256(
    coreml_archive.read_bytes()
).hexdigest()
manifest_path = f"{coreml_asset['path_prefix']}/{pins['coreml']['manifest_path']}"
manifest_body = f"fixture {coreml_asset['artifact']} {manifest_path}\n".encode()
pins["coreml"]["manifest_sha256"] = hashlib.sha256(manifest_body).hexdigest()
layout_path.write_text(json.dumps(layout, indent=2) + "\n")
PY
printf 'windows ONNX Runtime fixture\n' >"$artifacts/ctx-onnxruntime-windows-x64.zip"
mkdir -p "$semantic_archive_backup"
for semantic_artifact in "${signed_semantic_artifacts[@]}"; do
  cp "$artifacts/$semantic_artifact" "$semantic_archive_backup/$semantic_artifact"
done
node scripts/release/tests/release_integrity_gzip_fixture.mjs "$artifacts"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

managed_pair_metadata() {
  while read -r key platform core core_object companion; do
    local core_sha companion_sha
    core_sha="$(sha256_file "$artifacts/$core")"
    companion_sha="$core_sha"
    printf '%s\n' \
      "CTX_RELEASE_MANAGED_PAIR_ENVELOPE_${key}=ctx-managed-pair-${platform}.json" \
      "CTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_${key}=sha256/${core_sha}/${core_object}" \
      "CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_${key}=${core_sha}" \
      "CTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_${key}=sha256/${companion_sha}/${companion}" \
      "CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_${key}=${companion_sha}"
  done <<'EOF'
linux_x64 linux-x64 ctx ctx-linux-x64 ctx-pro-linux-x64
linux_aarch64 linux-arm64 ctx-linux-aarch64 ctx-linux-aarch64 ctx-pro-linux-arm64
macos_arm64 macos-arm64 ctx-macos-arm64 ctx-macos-arm64 ctx-pro-macos-arm64
macos_x64 macos-x64 ctx-macos-x64 ctx-macos-x64 ctx-pro-macos-x64
windows_x64 windows-x64 ctx.exe ctx-windows-x64.exe ctx-pro-windows-x64.exe
EOF
}

runtime_transport_metadata() {
  printf '%s\n' 'CTX_RELEASE_ONNXRUNTIME_VERSION=1.27.0'
  while read -r key artifact; do
    printf '%s\n' \
      "CTX_RELEASE_ONNXRUNTIME_ARTIFACT_${key}=${artifact}" \
      "CTX_RELEASE_ONNXRUNTIME_SHA256_${key}=$(sha256_file "$artifacts/$artifact")"
  done <<'EOF'
linux_x64 ctx-onnxruntime-linux-x64.tar.gz
linux_aarch64 ctx-onnxruntime-linux-aarch64.tar.gz
windows_x64 ctx-onnxruntime-windows-x64.zip
macos_x64 ctx-onnxruntime-macos-x64.tar.gz
macos_arm64 ctx-onnxruntime-macos-arm64.tar.gz
EOF
}

write_candidate_manifest() {
  : >"$candidate_manifest"
  while read -r source_name candidate_name; do
    cp "$artifacts/$source_name" "$candidate_artifacts/$candidate_name"
    printf 'CycloneDX fixture for %s\n' "$candidate_name" \
      >"$candidate_artifacts/$candidate_name.cdx.json"
    printf 'Third-party notices fixture for %s\n' "$candidate_name" \
      >"$candidate_artifacts/$candidate_name.third-party-notices.txt"
    for candidate_asset in \
      "$candidate_name" \
      "$candidate_name.cdx.json" \
      "$candidate_name.third-party-notices.txt"; do
      printf '%s  %s\n' \
        "$(sha256_file "$candidate_artifacts/$candidate_asset")" \
        "$candidate_asset" >>"$candidate_manifest"
    done
  done <<'EOF'
ctx ctx-linux-x64
ctx-linux-aarch64 ctx-linux-aarch64
ctx-macos-arm64 ctx-macos-arm64
ctx-macos-x64 ctx-macos-x64
ctx.exe ctx-windows-x64.exe
EOF
  for runtime_artifact in "${candidate_runtime_artifacts[@]}"; do
    cp "$artifacts/$runtime_artifact" "$candidate_artifacts/$runtime_artifact"
    printf '%s  %s\n' \
      "$(sha256_file "$candidate_artifacts/$runtime_artifact")" \
      "$runtime_artifact" >>"$candidate_manifest"
  done
}

write_candidate_manifest

write_candidate_authority() {
  rm -rf "$candidate_authority"
  mkdir "$candidate_authority"
  while read -r key id platform artifact construction_label rust_triple manifest; do
    KEY="$key" \
    ID="$id" \
    PLATFORM="$platform" \
    ARTIFACT="$artifact" \
    CONSTRUCTION_LABEL="$construction_label" \
    RUST_TRIPLE="$rust_triple" \
    ARTIFACT_SHA256="$(sha256_file "$artifacts/$artifact")" \
    MANIFEST="$candidate_authority/$manifest" \
    SOURCE_COMMIT="$source_commit" \
      node - <<'NODE'
const fs = require("node:fs");

function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

const candidate = {
  artifact: {
    file: process.env.ARTIFACT,
    sha256: process.env.ARTIFACT_SHA256,
    size_bytes: fs.statSync(`${process.env.MANIFEST}/../${process.env.ARTIFACT}`, { throwIfNoEntry: false })?.size || 1,
  },
  construction: {
    authority: "linux-cross-cargo-zigbuild-v1",
    label: process.env.CONSTRUCTION_LABEL,
  },
  evidence: {},
  kind: "ctx-public-cli-candidate",
  product: "core",
  schema_version: 1,
  source: { clean: true, commit: process.env.SOURCE_COMMIT },
  tantivy: {},
  target: {
    id: process.env.ID,
    platform: process.env.PLATFORM,
    rust_triple: process.env.RUST_TRIPLE,
  },
  version: "1.5.0",
};
fs.writeFileSync(process.env.MANIFEST, `${canonical(candidate)}\n`);
NODE
    printf '%s\n' "$(sha256_file "$candidate_authority/$manifest")" \
      >"$candidate_authority/$manifest.sha256"
  done <<'EOF'
linux_x64 linux-x64 linux-x64 ctx scripts/release/build-public-candidate-on-linux.sh x86_64-unknown-linux-gnu ctx.candidate.json
linux_aarch64 linux-arm64 linux-aarch64 ctx-linux-aarch64 scripts/release/build-public-candidate-on-linux.sh aarch64-unknown-linux-gnu ctx-linux-aarch64.candidate.json
macos_arm64 macos-arm64 macos-arm64 ctx-macos-arm64 scripts/release/build-public-candidate-on-linux.sh aarch64-apple-darwin ctx-macos-arm64.candidate.json
macos_x64 macos-x64 macos-x64 ctx-macos-x64 scripts/release/build-public-candidate-on-linux.sh x86_64-apple-darwin ctx-macos-x64.candidate.json
windows_x64 windows-x64 windows-x64 ctx.exe scripts/release/build-public-candidate-on-linux.sh x86_64-pc-windows-gnu ctx.exe.candidate.json
EOF
  cp "$artifacts/ctx.exe" "$candidate_authority/ctx.exe"
  printf '{}\n' >"$candidate_authority/ctx.exe.build-info.json"
  printf '{}\n' >"$candidate_authority/ctx.exe.cdx.json"
  printf '{}\n' >"$candidate_authority/ctx.exe.size.json"
  printf 'fixture notices\n' \
    >"$candidate_authority/ctx.exe.third-party-notices.txt"
  grep -v '  ctx-onnxruntime-' "$candidate_manifest" \
    > "$candidate_authority/SHA256SUMS"
  printf '{}\n' >"$candidate_authority/ctx-core.release-complete.json"
  printf '{}\n' >"$candidate_authority/ctx-release-factory.json"
  # These are authored boundary receipts, not CI or platform-signing evidence.
  AUTHORITY_DIR="$candidate_authority" SOURCE_COMMIT="$source_commit" node - <<'NODE'
const fs = require("node:fs");
const path = require("node:path");
const root = process.env.AUTHORITY_DIR;
fs.writeFileSync(path.join(root, "normal-ci.json"), JSON.stringify({
  kind: "ctx-normal-ci-result", schema_version: 1, mode: "ci",
  source_commit: process.env.SOURCE_COMMIT, status: "passed",
}));
fs.writeFileSync(path.join(root, "windows-authenticode.json"), '{"fixture":"not platform-signing evidence"}\n');
NODE
  python3 scripts/release/release-validation.py \
    --source-commit "$source_commit" --policy factory-only-human-override-v1 \
    --ci-receipt "$candidate_authority/normal-ci.json" \
    --native-proof-dir "$candidate_authority" \
    --output "$candidate_authority/release-validation.json"
  AUTHORITY_DIR="$candidate_authority" node - <<'NODE'
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const root = process.env.AUTHORITY_DIR;
const names = [
  "ctx-linux-aarch64.candidate.json",
  "ctx.candidate.json",
  "ctx-macos-arm64.candidate.json",
  "ctx-macos-x64.candidate.json",
  "ctx.exe.candidate.json",
];
const candidate_manifests = names.map((file) => {
  const body = fs.readFileSync(path.join(root, file));
  return {
    file,
    sha256: crypto.createHash("sha256").update(body).digest("hex"),
    size_bytes: body.length,
  };
});
const body = Buffer.from(`${JSON.stringify({ candidate_manifests })}\n`, "utf8");
fs.writeFileSync(path.join(root, "ctx-core-github-handoff.json"), body);
fs.writeFileSync(
  path.join(root, "ctx-core-github-handoff.json.sha256"),
  `${crypto.createHash("sha256").update(body).digest("hex")}\n`,
);
NODE
}

write_candidate_authority

artifact_base_url="$(node -e 'console.log(require("node:url").pathToFileURL(process.argv[1]).toString())' "$artifacts")"
stable_metadata_url="$(node -e 'console.log(require("node:url").pathToFileURL(process.argv[1]).toString())' "$stable_metadata")"
versioned_metadata_url="$(node -e 'console.log(require("node:url").pathToFileURL(process.argv[1]).toString())' "$versioned_metadata")"

sign_metadata() {
  local metadata_path="$1"
  local signature_path="$2"
  METADATA_PATH="$metadata_path" SIGNATURE_PATH="$signature_path" PRIVATE_KEY_PATH="$private_key" node - <<'NODE'
const crypto = require("node:crypto");
const fs = require("node:fs");

const metadata = fs.readFileSync(process.env.METADATA_PATH);
const privateKey = fs.readFileSync(process.env.PRIVATE_KEY_PATH, "utf8");
const signature = crypto.sign("RSA-SHA256", metadata, {
  key: privateKey,
  padding: crypto.constants.RSA_PKCS1_PADDING,
});
fs.writeFileSync(process.env.SIGNATURE_PATH, `${signature.toString("base64")}\n`);
NODE
}

write_metadata() {
  local extra="${1:-}"
  python3 "$contract_root/scripts/release/semantic_runtime_metadata.py" generate \
    --artifact-dir "$artifacts" \
    --output "$semantic_metadata"
  write_candidate_manifest
  write_candidate_authority
  cat >"$stable_metadata" <<EOF
CTX_RELEASE_SCHEMA_VERSION=1
CTX_RELEASE_CHANNEL=stable
CTX_RELEASE_VERSION=1.5.0
CTX_RELEASE_BASE_URL=$artifact_base_url
CTX_RELEASE_SOURCE_COMMIT=$source_commit
CTX_RELEASE_SELF_UPGRADE_ALLOWED=true
CTX_RELEASE_AUTO_UPGRADE_ALLOWED=true
CTX_RELEASE_ARTIFACT_linux_x64=ctx
CTX_RELEASE_SHA256_linux_x64=$(sha256_file "$artifacts/ctx")
CTX_RELEASE_ARTIFACT_linux_aarch64=ctx-linux-aarch64
CTX_RELEASE_SHA256_linux_aarch64=$(sha256_file "$artifacts/ctx-linux-aarch64")
CTX_RELEASE_ARTIFACT_macos_arm64=ctx-macos-arm64
CTX_RELEASE_SHA256_macos_arm64=$(sha256_file "$artifacts/ctx-macos-arm64")
CTX_RELEASE_ARTIFACT_macos_x64=ctx-macos-x64
CTX_RELEASE_SHA256_macos_x64=$(sha256_file "$artifacts/ctx-macos-x64")
CTX_RELEASE_ARTIFACT_windows_x64=ctx.exe
CTX_RELEASE_SHA256_windows_x64=$(sha256_file "$artifacts/ctx.exe")
$(managed_pair_metadata)
$(runtime_transport_metadata)
CTX_RELEASE_CORE_GITHUB_HANDOFF_SHA256=$(sha256_file "$candidate_authority/ctx-core-github-handoff.json")
CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_linux_x64=$(sha256_file "$candidate_authority/ctx.candidate.json")
CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_linux_aarch64=$(sha256_file "$candidate_authority/ctx-linux-aarch64.candidate.json")
CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_macos_arm64=$(sha256_file "$candidate_authority/ctx-macos-arm64.candidate.json")
CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_macos_x64=$(sha256_file "$candidate_authority/ctx-macos-x64.candidate.json")
CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_windows_x64=$(sha256_file "$candidate_authority/ctx.exe.candidate.json")
$(cat "$semantic_metadata")
$extra
EOF
  cp "$stable_metadata" "$versioned_metadata"
  sign_metadata "$stable_metadata" "$stable_metadata.sig"
  sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
  node scripts/release/tests/release_integrity_gzip_fixture.mjs "$artifacts" >/dev/null
}

write_metadata_without_supplementary() {
  cat >"$stable_metadata" <<EOF
CTX_RELEASE_SCHEMA_VERSION=1
CTX_RELEASE_CHANNEL=stable
CTX_RELEASE_VERSION=1.9.9
CTX_RELEASE_BASE_URL=$artifact_base_url
CTX_RELEASE_SOURCE_COMMIT=$source_commit
CTX_RELEASE_SELF_UPGRADE_ALLOWED=true
CTX_RELEASE_AUTO_UPGRADE_ALLOWED=true
CTX_RELEASE_ARTIFACT_linux_x64=ctx
CTX_RELEASE_SHA256_linux_x64=$(sha256_file "$artifacts/ctx")
CTX_RELEASE_ARTIFACT_linux_aarch64=ctx-linux-aarch64
CTX_RELEASE_SHA256_linux_aarch64=$(sha256_file "$artifacts/ctx-linux-aarch64")
CTX_RELEASE_ARTIFACT_macos_arm64=ctx-macos-arm64
CTX_RELEASE_SHA256_macos_arm64=$(sha256_file "$artifacts/ctx-macos-arm64")
CTX_RELEASE_ARTIFACT_macos_x64=ctx-macos-x64
CTX_RELEASE_SHA256_macos_x64=$(sha256_file "$artifacts/ctx-macos-x64")
CTX_RELEASE_ARTIFACT_windows_x64=ctx.exe
CTX_RELEASE_SHA256_windows_x64=$(sha256_file "$artifacts/ctx.exe")
$(managed_pair_metadata)
EOF
  cp "$stable_metadata" "$versioned_metadata"
  sign_metadata "$stable_metadata" "$stable_metadata.sig"
  sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
  node scripts/release/tests/release_integrity_gzip_fixture.mjs "$artifacts" >/dev/null
}

run_contract() {
  local expected_commit="${1:-$source_commit}"
  local expected_version="${2:-1.5.0}"
  CTX_PUBLIC_CTX_REPO="$repo" \
    CTX_TEST_REAL_GIT="$real_git" \
    CTX_PUBLIC_RELEASE_VERSION="$expected_version" \
    CTX_PUBLIC_RELEASE_SOURCE_COMMIT="$expected_commit" \
    CTX_PUBLIC_RELEASE_CANDIDATE_MANIFEST="$candidate_authority" \
    CTX_PUBLIC_RELEASE_SHA256SUMS="${CTX_TEST_RELEASE_SUMS:-$candidate_manifest}" \
    CTX_PUBLIC_RELEASE_STABLE_METADATA_URL="$stable_metadata_url" \
    CTX_PUBLIC_RELEASE_VERSIONED_METADATA_URL="$versioned_metadata_url" \
    CTX_PUBLIC_RELEASE_ALLOW_CUSTOM_BASE_URL=1 \
    CTX_PUBLIC_RELEASE_SKIP_REMOTE_CHECK=1 \
    CTX_PUBLIC_RELEASE_EVIDENCE_PATH="$evidence" \
    PATH="$authority_git_bin:$PATH" \
    node --import "$tmp/frozen-bridge-fetch.mjs" "$contract_root/scripts/release/release-contract.cjs"
}

expect_failure() {
  local label="$1"
  shift
  if "$@" >"$tmp/${label}.stdout" 2>"$tmp/${label}.stderr"; then
    echo "error: expected release contract failure for $label" >&2
    cat "$tmp/${label}.stdout" >&2
    exit 1
  fi
}

expect_failure_contains() {
  local label="$1"
  local needle="$2"
  shift 2
  expect_failure "$label" "$@"
  if ! grep -F "$needle" "$tmp/${label}.stderr" >/dev/null; then
    echo "error: expected release contract failure for $label to contain: $needle" >&2
    cat "$tmp/${label}.stderr" >&2
    exit 1
  fi
}

tamper_semantic_catalog() {
  local kind="$1"
  for metadata_path in "$stable_metadata" "$versioned_metadata"; do
    METADATA_PATH="$metadata_path" TAMPER_KIND="$kind" node - <<'NODE'
const fs = require("node:fs");
const path = process.env.METADATA_PATH;
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}
const text = fs.readFileSync(path, "utf8").replace(
  /^CTX_RELEASE_SEMANTIC_ASSETS=(.*)$/m,
  (_, encoded) => {
    const catalog = JSON.parse(Buffer.from(encoded, "base64").toString("utf8"));
    if (process.env.TAMPER_KIND === "model-runtime") {
      const tokenizer = catalog.assets.onnx_model.files.find(
        (record) => record.path === "tokenizer.json",
      );
      tokenizer.sha256 = "a".repeat(64);
    } else if (process.env.TAMPER_KIND.startsWith("missing-")) {
      delete catalog.assets[process.env.TAMPER_KIND.slice("missing-".length)];
    } else if (process.env.TAMPER_KIND === "coreml-archive") {
      catalog.assets.apple_coreml.archive_sha256 = "a".repeat(64);
    } else if (process.env.TAMPER_KIND === "coreml-manifest") {
      const manifest = catalog.assets.apple_coreml.files.find(
        (record) => record.path === "manifest.json",
      );
      manifest.sha256 = "a".repeat(64);
    } else {
      throw new Error(`unknown semantic catalog tamper kind: ${process.env.TAMPER_KIND}`);
    }
    return `CTX_RELEASE_SEMANTIC_ASSETS=${Buffer.from(canonical(catalog)).toString("base64")}`;
  },
);
fs.writeFileSync(path, text);
NODE
  done
  sign_metadata "$stable_metadata" "$stable_metadata.sig"
  sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
}

tamper_apple_authority() {
  local kind="$1"
  for metadata_path in "$stable_metadata" "$versioned_metadata"; do
    METADATA_PATH="$metadata_path" TAMPER_KIND="$kind" node - <<'NODE'
const fs = require("node:fs");
const path = process.env.METADATA_PATH;
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}
const text = fs.readFileSync(path, "utf8").replace(
  /^CTX_RELEASE_SEMANTIC_AUTHORITY_apple_silicon_coreml=(.*)$/m,
  (_, encoded) => {
    const authority = JSON.parse(Buffer.from(encoded, "base64").toString("utf8"));
    if (process.env.TAMPER_KIND === "incomplete") {
      authority.asset_ids = ["apple_coreml"];
    } else if (process.env.TAMPER_KIND === "misordered") {
      authority.asset_ids = ["onnx_model", "apple_coreml", "macos_arm64_cpu"];
    } else {
      throw new Error(`unknown Apple authority tamper kind: ${process.env.TAMPER_KIND}`);
    }
    return `CTX_RELEASE_SEMANTIC_AUTHORITY_apple_silicon_coreml=${Buffer.from(canonical(authority)).toString("base64")}`;
  },
);
fs.writeFileSync(path, text);
NODE
  done
  sign_metadata "$stable_metadata" "$stable_metadata.sig"
  sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
}

# This isolated source fixture binds a test-signed B identity.
# Production uses its reviewed operator disposition; no runtime key, identity or endpoint override.
write_bridge_disposition() {
  CONTRACT_ROOT="$contract_root" FIXTURE_ROOT="$tmp" ORIGINAL_METADATA="$original_metadata" \
  PRIVATE_KEY_PATH="$private_key" SOURCE_COMMIT="$source_commit" node - <<'NODE'
const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const metadata = Buffer.from(`CTX_RELEASE_VERSION=1.3.2\nCTX_RELEASE_CHANNEL=stable\nCTX_RELEASE_SOURCE_COMMIT=${process.env.SOURCE_COMMIT}\n`);
const signature = Buffer.from(`${crypto.sign("RSA-SHA256", metadata, fs.readFileSync(process.env.PRIVATE_KEY_PATH)).toString("base64")}\n`);
const digest = body => crypto.createHash("sha256").update(body).digest("hex");
const identity = { publicSourceCommit: process.env.SOURCE_COMMIT, privateSourceCommit: "d".repeat(40),
  metadataSha256: digest(metadata), signatureSha256: digest(signature), operatorDispositionSha256: "e".repeat(64) };
fs.writeFileSync(process.env.ORIGINAL_METADATA, metadata);
fs.writeFileSync(`${process.env.ORIGINAL_METADATA}.sig`, signature);
const pointerPath = path.join(process.env.FIXTURE_ROOT, "bridge-current.json");
const object = "releases/stable/1.3.2/ctx-release-metadata.env";
fs.writeFileSync(pointerPath, JSON.stringify({ channel: "stable", contract: "ctx-cli-release-pointer", schema_version: 1,
  version: "1.3.2", metadata_object: object, metadata_sha256: identity.metadataSha256,
  signature_object: `${object}.sig`, signature_sha256: identity.signatureSha256 }));
const sourcePath = path.join(process.env.CONTRACT_ROOT, "scripts/release/frozen-cli-bridge.cjs");
fs.writeFileSync(sourcePath, fs.readFileSync(sourcePath, "utf8").replace(/^const FROZEN_BRIDGE_IDENTITY = .*;$/mu, `const FROZEN_BRIDGE_IDENTITY = ${JSON.stringify(identity)};`));
const base = "https://cli.ctx.rs/functions/v1/releases/stable";
const files = [[`${base}/current.json`, pointerPath], [`${base}/1.3.2/ctx-release-metadata.env`, process.env.ORIGINAL_METADATA],
  [`${base}/1.3.2/ctx-release-metadata.env.sig`, `${process.env.ORIGINAL_METADATA}.sig`]];
fs.writeFileSync(path.join(process.env.FIXTURE_ROOT, "frozen-bridge-fetch.mjs"),
  `import fs from "node:fs";\nconst files = new Map(${JSON.stringify(files)});\n` +
  'globalThis.fetch = async (url) => { if (!files.has(url)) throw new Error(`unexpected network ${url}`); return new Response(fs.readFileSync(files.get(url))); };\n');
NODE
}
write_bridge_disposition
write_metadata
run_contract
test -s "$evidence"
# A newly discovered release-branch fix must block the otherwise valid handoff.
git -C "$repo" checkout -qb released-fix
printf 'released repair\n' >"$repo/released-fix"
git -C "$repo" add released-fix
git -C "$repo" commit -qm 'fixture released repair'
git -C "$repo" tag -a v1.5.0 -m 'fixture release with repair'
git -C "$repo" checkout -q main
expect_failure_contains omitted-released-fix \
  "released changes are absent without a reviewed disposition" run_contract
git -C "$repo" tag -d v1.5.0 >/dev/null
run_contract
node - "$evidence" "$stable_metadata" "$candidate_authority/release-validation.json" <<'NODE'
const assert = require("node:assert/strict");
const fs = require("node:fs");
const evidence = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const values = Object.fromEntries(fs.readFileSync(process.argv[3], "utf8").trim().split("\n").map(line => {
  const split = line.indexOf("="); return [line.slice(0, split), line.slice(split + 1)];
}));
const targets = { "linux-x64": "linux_x64", "linux-aarch64": "linux_aarch64",
  "macos-arm64": "macos_arm64", "macos-x64": "macos_x64", "windows-x64": "windows_x64" };
assert.deepEqual(Object.keys(evidence.metadata.managed_pair).sort(), Object.keys(targets).sort());
for (const [target, key] of Object.entries(targets)) {
  assert.deepEqual(evidence.metadata.managed_pair[target], {
    core_sha256: values[`CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_${key}`],
    pro_sha256: values[`CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_${key}`],
  });
  assert.equal(evidence.metadata.managed_pair[target].core_sha256,
    evidence.metadata.managed_pair[target].pro_sha256);
}
assert.deepEqual(evidence.validation, {
  construction: JSON.parse(fs.readFileSync(process.argv[4], "utf8")),
  publication_readback: "passed", installer_native_execution: "not_run",
  stock_1_4_upgrade: "not_run",
});
assert.equal(evidence.validation.construction.source_commit, values.CTX_RELEASE_SOURCE_COMMIT);
assert.equal(evidence.validation.construction.validation_policy, "factory-only-human-override-v1");
assert.equal(evidence.validation.construction.nightly, "not_run");
assert.equal(evidence.validation.construction.release_tier, "not_run");
assert.deepEqual(evidence.validation.construction.native_execution,
  Object.fromEntries(Object.keys(targets).map(target => [target, { status: "not_run" }])));
assert.equal(evidence.metadata.frozen_bridge.version, "1.3.2");
assert.equal(evidence.metadata.stable.signature_verified, true);
assert.equal(evidence.metadata.versioned.signature_verified, true);
assert.equal(evidence.hosted_matrix.filter(entry => entry.component === "cli").length, 5);
assert.equal(evidence.hosted_matrix.filter(entry => entry.component === "onnxruntime-compatibility").length, 5);
assert.equal(evidence.hosted_matrix.filter(entry => entry.component.startsWith("semantic-")).length, 9);
NODE
# A valid signature over changed B bytes cannot replace the reviewed identity.
printf "CTX_RELEASE_TEST_DIFFERENCE=original-only\n" >>"$original_metadata"
sign_metadata "$original_metadata" "$original_metadata.sig"
expect_failure_contains frozen-bridge-readback "frozen bridge signed byte identity differs from reviewed disposition" run_contract
write_bridge_disposition
sed -i 's/^const FROZEN_BRIDGE_IDENTITY = .*;$/const FROZEN_BRIDGE_IDENTITY = null;/' "$contract_root/scripts/release/frozen-cli-bridge.cjs"
expect_failure_contains missing-bridge-disposition "has no reviewed disposition; future stable promotion is blocked" run_contract
write_bridge_disposition

for field in ARTIFACT SHA256; do
  write_metadata
  for metadata_path in "$stable_metadata" "$versioned_metadata"; do
    sed -i "/^CTX_RELEASE_ONNXRUNTIME_${field}_linux_x64=/d" "$metadata_path"
    sign_metadata "$metadata_path" "$metadata_path.sig"
  done
  expect_failure_contains "current-partial-$field" "partial or unexpected ONNX Runtime transport matrix" run_contract
done
write_metadata
for metadata_path in "$stable_metadata" "$versioned_metadata"; do
  printf 'CTX_RELEASE_ONNXRUNTIME_ARTIFACT_freebsd_x64=extra.tar.gz\n' >>"$metadata_path"
  sign_metadata "$metadata_path" "$metadata_path.sig"
done
expect_failure_contains historical-field-rejected "partial or unexpected ONNX Runtime transport matrix" run_contract
write_metadata

grep -F '"status": "passed"' "$evidence" >/dev/null
grep -F "\"sha256\": \"$(sha256_file "$candidate_manifest")\"" "$evidence" >/dev/null
grep -F "\"windows_x64\": \"$(sha256_file "$candidate_authority/ctx.exe.candidate.json")\"" \
  "$evidence" >/dev/null
grep -F "\"core_github_handoff_sha256\": \"$(sha256_file "$candidate_authority/ctx-core-github-handoff.json")\"" \
  "$evidence" >/dev/null
grep -F '"gzip_transport"' "$evidence" >/dev/null
test "$(wc -l <"$candidate_manifest")" -eq 20

write_metadata_without_supplementary
CTX_PUBLIC_RELEASE_SKIP_EXECUTE_LINUX=1 \
CTX_TEST_RELEASE_SUMS="$candidate_manifest" expect_failure_contains \
  missing-supplementary-profile \
  "missing the required supplementary release profile" \
  run_contract "$source_commit" 1.9.9

for metadata_path in "$stable_metadata" "$versioned_metadata"; do
  printf '%s\n' 'CTX_RELEASE_ONNXRUNTIME_VERSION=1.27.0' >>"$metadata_path"
  sign_metadata "$metadata_path" "$metadata_path.sig"
done
CTX_TEST_RELEASE_SUMS="$candidate_manifest" expect_failure_contains \
  partial-supplementary-profile \
  "partial supplementary release profile" \
  run_contract "$source_commit" 1.9.9

write_metadata
CTX_TEST_REJECT_PUBLIC_AUTHORITY_DESCENDANT=1 expect_failure_contains \
  public-source-before-manifest-authority \
  "is not a descendant of manifest authority 4eb7234af45b568a4200e7331570d9056a1c5cdd" \
  run_contract

write_metadata
sed -i '/^CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_macos_x64=/d' \
  "$stable_metadata"
cp "$stable_metadata" "$versioned_metadata"
sign_metadata "$stable_metadata" "$stable_metadata.sig"
sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
expect_failure_contains \
  candidate-digest-matrix-incomplete \
  "missing macos_x64" \
  run_contract

write_metadata \
  "CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_linux_arm64=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
expect_failure_contains \
  candidate-digest-matrix-misnamed \
  "unexpected linux_arm64" \
  run_contract

write_metadata
sed -i \
  's/^CTX_RELEASE_CORE_GITHUB_HANDOFF_SHA256=.*/CTX_RELEASE_CORE_GITHUB_HANDOFF_SHA256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/' \
  "$versioned_metadata"
sign_metadata "$stable_metadata" "$stable_metadata.sig"
sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
expect_failure_contains \
  core-github-handoff-digest-disagreement \
  "stable metadata CTX_RELEASE_CORE_GITHUB_HANDOFF_SHA256 does not match versioned metadata" \
  run_contract

write_metadata
sed -i \
  's/^CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_macos_arm64=.*/CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_macos_arm64=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/' \
  "$versioned_metadata"
sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
expect_failure_contains \
  candidate-digest-matrix-disagreement \
  "stable metadata CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_macos_arm64 does not match versioned metadata" \
  run_contract

write_metadata
CANDIDATE_PATH="$candidate_authority/ctx-macos-x64.candidate.json" node - <<'NODE'
const fs = require("node:fs");
const file = process.env.CANDIDATE_PATH;
const candidate = JSON.parse(fs.readFileSync(file, "utf8"));
candidate.evidence = { substituted: true };
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}
fs.writeFileSync(file, `${canonical(candidate)}\n`);
NODE
printf '%s\n' "$(sha256_file "$candidate_authority/ctx-macos-x64.candidate.json")" \
  >"$candidate_authority/ctx-macos-x64.candidate.json.sha256"
expect_failure_contains \
  coordinated-manifest-sidecar-substitution \
  "digest does not match CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_macos_x64" \
  run_contract

CANDIDATE_PATH="$candidate_authority/ctx-macos-x64.candidate.json" node - <<'NODE'
const fs = require("node:fs");
const file = process.env.CANDIDATE_PATH;
const candidate = JSON.parse(fs.readFileSync(file, "utf8"));
candidate.source.commit = "b".repeat(40);
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}
fs.writeFileSync(file, `${canonical(candidate)}\n`);
NODE
printf '%s\n' "$(sha256_file "$candidate_authority/ctx-macos-x64.candidate.json")" \
  >"$candidate_authority/ctx-macos-x64.candidate.json.sha256"
replacement_digest="$(sha256_file "$candidate_authority/ctx-macos-x64.candidate.json")"
for metadata_path in "$stable_metadata" "$versioned_metadata"; do
  sed -i \
    "s/^CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_macos_x64=.*/CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_macos_x64=${replacement_digest}/" \
    "$metadata_path"
done
sign_metadata "$stable_metadata" "$stable_metadata.sig"
sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
expect_failure_contains \
  coordinated-signed-matrix-handoff-substitution \
  "does not bind the signed release version, source, platform, and artifact" \
  run_contract

write_metadata
printf 'unexpected\n' >"$candidate_authority/unexpected"
expect_failure_contains \
  candidate-handoff-unexpected-leaf \
  "exact production inventory" \
  run_contract
write_metadata

for required_receipt in normal-ci.json release-validation.json windows-authenticode.json; do
  mv "$candidate_authority/$required_receipt" "$tmp/receipt-backup"
  expect_failure_contains "missing-$required_receipt" "exact production inventory" run_contract
  mv "$tmp/receipt-backup" "$candidate_authority/$required_receipt"
done

mv \
  "$candidate_artifacts/ctx-linux-aarch64.cdx.json" \
  "$candidate_artifacts/ctx-linux-aarch64.cdx.json.missing"
expect_failure_contains \
  candidate-missing-file \
  "could not read candidate artifact" \
  run_contract
mv \
  "$candidate_artifacts/ctx-linux-aarch64.cdx.json.missing" \
  "$candidate_artifacts/ctx-linux-aarch64.cdx.json"

mv "$artifacts/ctx-macos-arm64.gz" "$artifacts/ctx-macos-arm64.gz.missing"
expect_failure_contains missing-gzip-transport "could not read file URL" run_contract
mv "$artifacts/ctx-macos-arm64.gz.missing" "$artifacts/ctx-macos-arm64.gz"

sed -i 's/^[0-9a-f]\{64\}  ctx-linux-x64$/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  ctx-linux-x64/' "$candidate_manifest"
expect_failure_contains candidate-hosted-digest-mismatch "hosted digest for linux-x64 does not match candidate manifest artifact ctx-linux-x64" run_contract
write_candidate_manifest

sed -i '/  ctx-macos-x64$/d' "$candidate_manifest"
expect_failure_contains candidate-missing-platform "candidate manifest artifacts must be exactly" run_contract
write_candidate_manifest

printf '%s  ctx-unexpected\n' "$(sha256_file "$artifacts/ctx")" >>"$candidate_manifest"
expect_failure_contains candidate-unexpected-platform "candidate manifest artifacts must be exactly" run_contract
write_candidate_manifest

sed -i '/  ctx-onnxruntime-windows-x64.zip$/d' "$candidate_manifest"
expect_failure_contains candidate-missing-runtime "candidate manifest artifacts must be exactly" run_contract
write_candidate_manifest

sed -i 's/  ctx-onnxruntime-windows-x64.zip$/  ctx-windowsml-windows-x64.zip/' "$candidate_manifest"
expect_failure_contains candidate-stale-windows-ml "candidate manifest artifacts must be exactly" run_contract
write_candidate_manifest

printf '%s  ctx-semantic-unexpected.tar.gz\n' "$(sha256_file "$artifacts/ctx")" >>"$candidate_manifest"
expect_failure_contains candidate-unexpected-runtime "candidate manifest artifacts must be exactly" run_contract
write_candidate_manifest

sed -i '/  ctx-macos-arm64.third-party-notices.txt$/d' "$candidate_manifest"
expect_failure_contains candidate-missing-notices "candidate manifest artifacts must be exactly" run_contract
write_candidate_manifest

sed -i 's/^[0-9a-f]\{64\}  ctx-windows-x64.exe.cdx.json$/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  ctx-windows-x64.exe.cdx.json/' "$candidate_manifest"
expect_failure_contains candidate-evidence-digest-mismatch "candidate artifact does not match manifest digest: ctx-windows-x64.exe.cdx.json" run_contract
write_candidate_manifest

while IFS=$' \t\n' read -r case_key case_platform case_runtime_artifact; do
  write_metadata
  printf 'substituted candidate runtime for %s\n' "$case_platform" \
    >"$candidate_artifacts/$case_runtime_artifact"
  replacement_digest="$(sha256_file "$candidate_artifacts/$case_runtime_artifact")"
  sed -i \
    "s/^[0-9a-f]\\{64\\}  ${case_runtime_artifact}$/${replacement_digest}  ${case_runtime_artifact}/" \
    "$candidate_manifest"
  grep -F "${replacement_digest}  ${case_runtime_artifact}" "$candidate_manifest" >/dev/null
  expect_failure_contains \
    "candidate-runtime-${case_key}-signed-digest-substitution" \
    "hosted ONNX Runtime digest for ${case_platform} does not match candidate manifest artifact ${case_runtime_artifact}" \
    run_contract
done <<'EOF'
linux_x64 linux-x64 ctx-onnxruntime-linux-x64.tar.gz
linux_aarch64 linux-aarch64 ctx-onnxruntime-linux-aarch64.tar.gz
macos_arm64 macos-arm64 ctx-onnxruntime-macos-arm64.tar.gz
macos_x64 macos-x64 ctx-onnxruntime-macos-x64.tar.gz
windows_x64 windows-x64 ctx-onnxruntime-windows-x64.zip
EOF
write_candidate_manifest

rm -f "$stable_metadata.sig"
expect_failure missing-signature run_contract

write_metadata
printf 'AAAA\n' >"$stable_metadata.sig"
expect_failure_contains invalid-signature "stable metadata signature verification failed" run_contract

write_metadata
printf 'CTX_RELEASE_TEST_UNSIGNED=changed\n' >>"$versioned_metadata"
expect_failure_contains unsigned-metadata-change "versioned metadata signature verification failed" run_contract

write_metadata
expect_failure wrong-source-commit run_contract "0000000000000000000000000000000000000000"

write_metadata
expect_failure_contains \
  wrong-release-version \
  "stable metadata version is 1.5.0, expected 9.9.8" \
  run_contract "$source_commit" 9.9.8

write_metadata
METADATA_PATH="$stable_metadata" node - <<'NODE'
const fs = require("node:fs");
const path = process.env.METADATA_PATH;
const text = fs.readFileSync(path, "utf8")
  .replace(/^CTX_RELEASE_ARTIFACT_macos_x64=.*\n/m, "")
  .replace(/^CTX_RELEASE_SHA256_macos_x64=.*\n/m, "");
fs.writeFileSync(path, text);
NODE
cp "$stable_metadata" "$versioned_metadata"
sign_metadata "$stable_metadata" "$stable_metadata.sig"
sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
expect_failure_contains missing-platform "missing macos-x64" run_contract

write_metadata
METADATA_PATH="$stable_metadata" node - <<'NODE'
const fs = require("node:fs");
const path = process.env.METADATA_PATH;
const text = fs.readFileSync(path, "utf8")
  .replace(/^CTX_RELEASE_SELF_UPGRADE_ALLOWED=.*\n/m, "");
fs.writeFileSync(path, text);
NODE
cp "$stable_metadata" "$versioned_metadata"
sign_metadata "$stable_metadata" "$stable_metadata.sig"
sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
expect_failure_contains missing-self-upgrade-flag "stable metadata missing CTX_RELEASE_SELF_UPGRADE_ALLOWED" run_contract

write_metadata
METADATA_PATH="$stable_metadata" node - <<'NODE'
const fs = require("node:fs");
const path = process.env.METADATA_PATH;
const text = fs.readFileSync(path, "utf8")
  .replace(/^CTX_RELEASE_AUTO_UPGRADE_ALLOWED=.*\n/m, "");
fs.writeFileSync(path, text);
NODE
cp "$stable_metadata" "$versioned_metadata"
sign_metadata "$stable_metadata" "$stable_metadata.sig"
sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
expect_failure_contains missing-auto-upgrade-flag "stable metadata missing CTX_RELEASE_AUTO_UPGRADE_ALLOWED" run_contract

write_metadata "CTX_RELEASE_ARTIFACT_linux_arm64=ctx-linux-arm64
CTX_RELEASE_SHA256_linux_arm64=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
expect_failure_contains unexpected-platform "unexpected linux_arm64" run_contract

write_metadata
METADATA_PATH="$stable_metadata" node - <<'NODE'
const fs = require("node:fs");
const path = process.env.METADATA_PATH;
const text = fs.readFileSync(path, "utf8")
  .replace(/^CTX_RELEASE_SHA256_macos_arm64=.*\n/m, "");
fs.writeFileSync(path, text);
NODE
cp "$stable_metadata" "$versioned_metadata"
sign_metadata "$stable_metadata" "$stable_metadata.sig"
sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
expect_failure_contains checksum-platform-drift "missing macos-arm64" run_contract

write_metadata
METADATA_PATH="$stable_metadata" node - <<'NODE'
const fs = require("node:fs");
const path = process.env.METADATA_PATH;
const text = fs.readFileSync(path, "utf8").replace(
  /^CTX_RELEASE_SEMANTIC_AUTHORITY_universal_ort_cpu=.*\n/m,
  "",
);
fs.writeFileSync(path, text);
NODE
cp "$stable_metadata" "$versioned_metadata"
sign_metadata "$stable_metadata" "$stable_metadata.sig"
sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
expect_failure_contains missing-semantic-authority "missing universal_ort_cpu" run_contract

write_metadata
sed -i '/^CTX_RELEASE_SEMANTIC_ASSETS=/d' "$stable_metadata"
cp "$stable_metadata" "$versioned_metadata"
sign_metadata "$stable_metadata" "$stable_metadata.sig"
sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
expect_failure_contains missing-semantic-assets "stable metadata missing CTX_RELEASE_SEMANTIC_ASSETS" run_contract

write_metadata
tamper_semantic_catalog model-runtime
expect_failure_contains \
  wrong-signed-model-pin \
  "immutable onnx_model publication pin mismatch for tokenizer.json sha256" \
  run_contract

write_metadata
tamper_semantic_catalog coreml-archive
expect_failure_contains \
  wrong-signed-coreml-pin \
  "immutable CoreML archive publication pin mismatch" \
  run_contract

write_metadata
tamper_semantic_catalog coreml-manifest
expect_failure_contains \
  wrong-signed-coreml-manifest-pin \
  "immutable CoreML manifest publication pin mismatch" \
  run_contract

for asset in linux_cuda12 windows_ml; do
  write_metadata
  tamper_semantic_catalog "missing-$asset"
  expect_failure_contains "missing-required-$asset" "assets must have exactly these fields" run_contract
done

write_metadata "CTX_RELEASE_SEMANTIC_AUTHORITY_windows_ort_directml=W10="
expect_failure_contains unexpected-semantic-authority "unexpected windows_ort_directml" run_contract

write_metadata
tamper_apple_authority incomplete
expect_failure_contains \
  incomplete-apple-semantic-authority \
  "apple_silicon_coreml has the wrong normalized asset composition" \
  run_contract

write_metadata
tamper_apple_authority misordered
expect_failure_contains \
  misordered-apple-semantic-authority \
  "apple_silicon_coreml has the wrong normalized asset composition" \
  run_contract

write_metadata
METADATA_PATH="$versioned_metadata" node - <<'NODE'
const fs = require("node:fs");
const path = process.env.METADATA_PATH;
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}
const text = fs.readFileSync(path, "utf8").replace(
  /^CTX_RELEASE_SEMANTIC_ASSETS=(.*)$/m,
  (_, encoded) => {
    const catalog = JSON.parse(Buffer.from(encoded, "base64").toString("utf8"));
    catalog.assets.windows_ml.archive_sha256 = "a".repeat(64);
    return `CTX_RELEASE_SEMANTIC_ASSETS=${Buffer.from(canonical(catalog)).toString("base64")}`;
  },
);
fs.writeFileSync(path, text);
NODE
sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
expect_failure_contains semantic-stable-versioned-disagreement "stable metadata CTX_RELEASE_SEMANTIC_ASSETS does not match versioned metadata" run_contract

write_metadata
for metadata_path in "$stable_metadata" "$versioned_metadata"; do
  sed -i '/^CTX_RELEASE_ONNXRUNTIME_SHA256_macos_arm64=/d' "$metadata_path"
  sign_metadata "$metadata_path" "$metadata_path.sig"
done
expect_failure_contains incomplete-runtime-transport-metadata \
  "partial or unexpected ONNX Runtime transport matrix" run_contract

write_metadata
METADATA_PATH="$stable_metadata" node - <<'NODE'
const fs = require("node:fs");
const path = process.env.METADATA_PATH;
const text = fs.readFileSync(path, "utf8").replace(
  "CTX_RELEASE_ARTIFACT_windows_x64=ctx.exe\n",
  "CTX_RELEASE_ARTIFACT_windows_x64=ctx.exe \n",
);
fs.writeFileSync(path, text);
NODE
cp "$stable_metadata" "$versioned_metadata"
sign_metadata "$stable_metadata" "$stable_metadata.sig"
sign_metadata "$versioned_metadata" "$versioned_metadata.sig"
expect_failure metadata-whitespace run_contract

write_metadata
printf 'corrupted artifact bytes\n' >"$artifacts/ctx-macos-x64"
expect_failure_contains checksum-mismatch "live cli artifact checksum mismatch for macos-x64" run_contract

write_metadata
printf 'corrupted runtime bytes\n' >"$artifacts/ctx-onnxruntime-macos-x64.tar.zst"
expect_failure_contains runtime-checksum-mismatch "live semantic-cpu-runtime artifact checksum mismatch for ctx-onnxruntime-macos-x64.tar.zst" run_contract
cp "$semantic_archive_backup/ctx-onnxruntime-macos-x64.tar.zst" "$artifacts/ctx-onnxruntime-macos-x64.tar.zst"

write_metadata
UPGRADE_RS_PATH="$repo/crates/ctx-upgrade-engine/src/upgrade/metadata.rs" node - <<'NODE'
const crypto = require("node:crypto");
const fs = require("node:fs");

const { publicKey } = crypto.generateKeyPairSync("rsa", {
  modulusLength: 2048,
  publicExponent: 0x10001,
});
const pkcs1 = publicKey.export({ format: "pem", type: "pkcs1" }).trim();
fs.writeFileSync(
  process.env.UPGRADE_RS_PATH,
  `const DEFAULT_METADATA_PUBLIC_KEY_PEM: &str = r#"${pkcs1}"#;\n`,
);
NODE
expect_failure_contains public-cli-key-drift "public CLI runtime metadata public key does not match hosted Unix installer metadata public key" run_contract

node --test scripts/release/tests/release_integrity_driver_test.mjs

echo "public ctx release contract smoke: OK"
