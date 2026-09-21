#!/usr/bin/env bash
set -euo pipefail

if [[ $# -eq 0 && -z "${TEST_SRCDIR:-}${TEST_TARGET:-}${RUNFILES_DIR:-}" ]]; then
  set -- "$(dirname "${BASH_SOURCE[0]}")/../../.." "$(command -v node)"
fi
[[ $# -eq 2 ]] || { echo "usage: $0 SOURCE_ROOT NODE_BINARY" >&2; exit 64; }
# Bazel supplies its pinned Node and declared sources; never select host Node.
node_bin="$(cd "$(dirname "$2")" && pwd -P)/$(basename "$2")"
[[ -f "$node_bin" && -x "$node_bin" ]] || { echo "declared Node is unavailable" >&2; exit 69; }
ROOT="$(cd "$1" && pwd -P)"
[[ -f "$ROOT/scripts/release/ctx_cli_release_metadata_sign.sh" ]] || { echo "declared signer source is missing" >&2; exit 66; }
cd "$ROOT"

tmp="$(mktemp -d "${TEST_TMPDIR:-${TMPDIR:-/tmp}}"/ctx-cli-metadata-sign-contract.XXXXXX)"
cleanup() {
  if [[ -n "${concurrent_release_first:-}" ]]; then
    touch "$concurrent_release_first" 2>/dev/null || true
  fi
  if [[ -n "${concurrent_first_pid:-}" ]]; then
    kill "$concurrent_first_pid" 2>/dev/null || true
    wait "$concurrent_first_pid" 2>/dev/null || true
  fi
  if [[ -n "${concurrent_second_pid:-}" ]]; then
    kill "$concurrent_second_pid" 2>/dev/null || true
    wait "$concurrent_second_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp"
}
trap cleanup EXIT
mkdir -p "$tmp/bin"
ln -s "$node_bin" "$tmp/bin/node"
export PATH="$tmp/bin:$PATH"

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

wait_for_file() {
  local path="$1"
  local attempts=0
  while [[ ! -e "$path" && "$attempts" -lt 200 ]]; do
    sleep 0.05
    attempts=$((attempts + 1))
  done
  if [[ ! -e "$path" ]]; then
    echo "timed out waiting for test synchronization file: $path" >&2
    return 1
  fi
}

metadata="$tmp/ctx-release-metadata.env"
signature="$tmp/ctx-release-metadata.env.sig"
second_signature="$tmp/ctx-release-metadata.second.sig"
private_key="$tmp/metadata-signing-private.pem"
public_key="$tmp/metadata-signing-public.pem"

cat >"$metadata" <<'EOF'
CTX_RELEASE_SCHEMA_VERSION=1
CTX_RELEASE_VERSION=0.25.0
CTX_RELEASE_CHANNEL=stable
CTX_RELEASE_BASE_URL=https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/stable/0.25.0
CTX_RELEASE_ARTIFACT_linux_x64=ctx
CTX_RELEASE_SHA256_linux_x64=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
EOF

PRIVATE_KEY_PATH="$private_key" PUBLIC_KEY_PATH="$public_key" node - <<'NODE'
const crypto = require("crypto");
const fs = require("fs");

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

signer_source="$(<scripts/release/ctx_cli_release_metadata_sign.sh)"
for required in \
  --managed-pair-publication \
  --runtime-handoff \
  --public-ctx-repo \
  --semantic-artifact-dir \
  --candidate-manifest-handoff \
  --candidate-handoff-sha256; do
  [[ "$signer_source" == *"$required"* ]] || {
    echo "signer omits current runtime handoff argument: $required" >&2
    exit 1
  }
done
for retired in \
  --legacy-v025-runtime-dir \
  --allow-v025-upgrade-bridge \
  --without-supplementary-assets; do
  [[ "$signer_source" != *"$retired"* ]] || {
    echo "signer retains retired runtime authority: $retired" >&2
    exit 1
  }
done

printf '{}\n' >"$tmp/managed-publication.json"
printf '{}\n' >"$tmp/runtime-handoff.json"
mkdir "$tmp/public-repo" "$tmp/managed-semantic" "$tmp/managed-candidates"
if scripts/release/ctx_cli_release_metadata_sign.sh \
  --metadata "$metadata" \
  --managed-pair-publication "$tmp/managed-publication.json" \
  >"$tmp/missing-runtime.stdout" 2>"$tmp/missing-runtime.stderr"; then
  echo "signer accepted managed publication without current runtime authority" >&2
  exit 1
fi
grep -F \
  "managed-pair publication requires --public-ctx-repo" \
  "$tmp/missing-runtime.stderr" >/dev/null
if scripts/release/ctx_cli_release_metadata_sign.sh \
  --metadata "$metadata" \
  --runtime-handoff "$tmp/runtime-handoff.json" \
  --public-ctx-repo "$tmp/public-repo" \
  >"$tmp/orphan-runtime.stdout" 2>"$tmp/orphan-runtime.stderr"; then
  echo "signer accepted a runtime handoff outside managed publication mode" >&2
  exit 1
fi
grep -F "runtime handoff requires --managed-pair-publication" \
  "$tmp/orphan-runtime.stderr" >/dev/null

if scripts/release/ctx_cli_release_metadata_sign.sh \
  --metadata "$metadata" \
  --managed-pair-publication "$tmp/managed-publication.json" \
  --runtime-handoff "$tmp/runtime-handoff.json" \
  --public-ctx-repo "$tmp/public-repo" \
  >"$tmp/runtime-only.stdout" 2>"$tmp/runtime-only.stderr"; then
  echo "signer accepted a managed publication with only a runtime handoff" >&2
  exit 1
fi
grep -F \
  "requires the complete runtime, Semantic, and candidate handoff set" \
  "$tmp/runtime-only.stderr" >/dev/null

if scripts/release/ctx_cli_release_metadata_sign.sh \
  --metadata "$metadata" \
  --managed-pair-publication "$tmp/managed-publication.json" \
  --runtime-handoff "$tmp/runtime-handoff.json" \
  --public-ctx-repo "$tmp/public-repo" \
  --semantic-artifact-dir "$tmp/managed-semantic" \
  >"$tmp/unpaired-semantic.stdout" 2>"$tmp/unpaired-semantic.stderr"; then
  echo "signer accepted managed Semantic artifacts without candidate authority" >&2
  exit 1
fi
grep -F \
  "requires both --semantic-artifact-dir and --candidate-manifest-handoff" \
  "$tmp/unpaired-semantic.stderr" >/dev/null

if CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  scripts/release/ctx_cli_release_metadata_sign.sh \
    --metadata "$metadata" \
    --out "$signature" \
    >"$tmp/default-legacy.stdout" 2>"$tmp/default-legacy.stderr"; then
  echo "signer unexpectedly signed non-semantic metadata without explicit legacy mode" >&2
  exit 1
fi
grep -F -- "--allow-legacy-pre-v0260-nonsemantic" \
  "$tmp/default-legacy.stderr" >/dev/null
test ! -e "$signature"

CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  scripts/release/ctx_cli_release_metadata_sign.sh \
    --metadata "$metadata" \
    --out "$signature" \
    --allow-legacy-pre-v0260-nonsemantic >/dev/null
CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  scripts/release/ctx_cli_release_metadata_sign.sh \
    --metadata "$metadata" \
    --out "$second_signature" \
    --allow-legacy-pre-v0260-nonsemantic >/dev/null
cmp "$signature" "$second_signature"

METADATA_PATH="$metadata" SIGNATURE_PATH="$signature" PUBLIC_KEY_PATH="$public_key" node - <<'NODE'
const crypto = require("crypto");
const fs = require("fs");

const metadata = fs.readFileSync(process.env.METADATA_PATH);
const signature = Buffer.from(fs.readFileSync(process.env.SIGNATURE_PATH, "utf8").trim(), "base64");
const publicKey = fs.readFileSync(process.env.PUBLIC_KEY_PATH, "utf8");
const ok = crypto.verify(
  "RSA-SHA256",
  metadata,
  { key: publicKey, padding: crypto.constants.RSA_PKCS1_PADDING },
  signature,
);

if (!ok) {
  console.error("signature did not verify");
  process.exit(1);
}
NODE

v026_nonsemantic="$tmp/v026-nonsemantic.env"
sed 's/CTX_RELEASE_VERSION=0.25.0/CTX_RELEASE_VERSION=0.26.0/' \
  "$metadata" >"$v026_nonsemantic"
if CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  scripts/release/ctx_cli_release_metadata_sign.sh \
    --metadata "$v026_nonsemantic" \
    --out "$v026_nonsemantic.sig" \
    --allow-legacy-pre-v0260-nonsemantic \
    >"$tmp/v026-nonsemantic.stdout" 2>"$tmp/v026-nonsemantic.stderr"; then
  echo "signer unexpectedly signed release 0.26.0 without semantic metadata" >&2
  exit 1
fi
grep -F \
  "release 0.26.0 metadata requires the complete semantic field set" \
  "$tmp/v026-nonsemantic.stderr" >/dev/null
test ! -e "$v026_nonsemantic.sig"

race_metadata="$tmp/race-metadata.env"
race_replacement="$tmp/race-metadata.replacement.env"
race_signature="$tmp/race-metadata.env.sig"
cp "$metadata" "$race_metadata"
sed 's/CTX_RELEASE_VERSION=0.25.0/CTX_RELEASE_VERSION=0.24.0/' \
  "$metadata" >"$race_replacement"
mkdir "$tmp/race-bin"
real_python3="$(command -v python3)"
cat >"$tmp/race-bin/python3" <<'EOF'
#!/bin/sh
"$CTX_TEST_REAL_PYTHON3" "$@"
status=$?
if [ "$status" -eq 0 ]; then
  cp "$CTX_TEST_RACE_REPLACEMENT" "$CTX_TEST_RACE_METADATA"
fi
exit "$status"
EOF
chmod 0755 "$tmp/race-bin/python3"
if PATH="$tmp/race-bin:$PATH" \
  CTX_TEST_REAL_PYTHON3="$real_python3" \
  CTX_TEST_RACE_METADATA="$race_metadata" \
  CTX_TEST_RACE_REPLACEMENT="$race_replacement" \
  CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  scripts/release/ctx_cli_release_metadata_sign.sh \
    --metadata "$race_metadata" \
    --out "$race_signature" \
    --allow-legacy-pre-v0260-nonsemantic \
    >"$tmp/race.stdout" 2>"$tmp/race.stderr"; then
  echo "signer unexpectedly published a signature after metadata replacement" >&2
  exit 1
fi
grep -F "metadata input changed during semantic validation" "$tmp/race.stderr" >/dev/null
cmp "$race_metadata" "$race_replacement"
test ! -e "$race_signature"

concurrent_first_metadata="$tmp/concurrent-first-metadata.env"
concurrent_first_replacement="$tmp/concurrent-first-metadata.replacement.env"
concurrent_signature="$tmp/concurrent-metadata.env.sig"
concurrent_lock="$tmp/.$(basename "$concurrent_signature").lock"
concurrent_preload="$tmp/concurrent-publication-preload.cjs"
concurrent_first_renamed="$tmp/concurrent-first-renamed"
concurrent_release_first="$tmp/concurrent-release-first"
concurrent_second_at_lock="$tmp/concurrent-second-at-lock"
concurrent_bin="$tmp/concurrent-bin"
real_flock="$(command -v flock)"
sed 's/CTX_RELEASE_VERSION=0.25.0/CTX_RELEASE_VERSION=0.24.0/' \
  "$metadata" >"$concurrent_first_metadata"
sed 's/CTX_RELEASE_VERSION=0.25.0/CTX_RELEASE_VERSION=0.23.0/' \
  "$metadata" >"$concurrent_first_replacement"
mkdir "$concurrent_bin"
cat >"$concurrent_bin/flock" <<'EOF'
#!/bin/sh
if [ "${CTX_TEST_CONCURRENT_ROLE:-}" = "second" ]; then
  touch "$CTX_TEST_CONCURRENT_SECOND_AT_LOCK"
fi
exec "$CTX_TEST_REAL_FLOCK" "$@"
EOF
chmod 0755 "$concurrent_bin/flock"
cat >"$concurrent_preload" <<'NODE'
const fs = require("fs");

const role = process.env.CTX_TEST_CONCURRENT_ROLE;
if (role) {
  const signatureOut = process.env.CTX_TEST_CONCURRENT_SIGNATURE;
  const eventLog = `${process.env.CTX_TEST_CONCURRENT_EVENT_PREFIX}.${role}`;
  const originalFsyncSync = fs.fsyncSync.bind(fs);
  const originalRenameSync = fs.renameSync.bind(fs);
  const originalUnlinkSync = fs.unlinkSync.bind(fs);
  const waitArray = new Int32Array(new SharedArrayBuffer(4));
  let directoryFsyncs = 0;

  function log(event) {
    fs.appendFileSync(eventLog, `${event}\n`);
  }

  log("node-start");
  fs.renameSync = (source, destination) => {
    originalRenameSync(source, destination);
    if (destination !== signatureOut) return;
    log("rename");
    if (role === "first") {
      fs.writeFileSync(process.env.CTX_TEST_CONCURRENT_FIRST_RENAMED, "");
      while (!fs.existsSync(process.env.CTX_TEST_CONCURRENT_RELEASE_FIRST)) {
        Atomics.wait(waitArray, 0, 0, 20);
      }
    }
  };
  fs.fsyncSync = (descriptor) => {
    const isDirectory = fs.fstatSync(descriptor).isDirectory();
    originalFsyncSync(descriptor);
    log(isDirectory ? "directory-fsync" : "file-fsync");
    if (role === "first" && isDirectory && directoryFsyncs++ === 0) {
      fs.copyFileSync(
        process.env.CTX_TEST_CONCURRENT_FIRST_REPLACEMENT,
        process.env.CTX_TEST_CONCURRENT_FIRST_METADATA,
      );
      log("metadata-replaced");
    }
  };
  fs.unlinkSync = (target) => {
    if (target === signatureOut) log("output-unlink");
    return originalUnlinkSync(target);
  };
}
NODE

CTX_TEST_CONCURRENT_ROLE=first \
CTX_TEST_CONCURRENT_SIGNATURE="$concurrent_signature" \
CTX_TEST_CONCURRENT_EVENT_PREFIX="$tmp/concurrent-events" \
CTX_TEST_CONCURRENT_FIRST_RENAMED="$concurrent_first_renamed" \
CTX_TEST_CONCURRENT_RELEASE_FIRST="$concurrent_release_first" \
CTX_TEST_CONCURRENT_FIRST_METADATA="$concurrent_first_metadata" \
CTX_TEST_CONCURRENT_FIRST_REPLACEMENT="$concurrent_first_replacement" \
CTX_TEST_CONCURRENT_SECOND_AT_LOCK="$concurrent_second_at_lock" \
CTX_TEST_REAL_FLOCK="$real_flock" \
NODE_OPTIONS="--require=$concurrent_preload" \
PATH="$concurrent_bin:$PATH" \
CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  scripts/release/ctx_cli_release_metadata_sign.sh \
    --metadata "$concurrent_first_metadata" \
    --out "$concurrent_signature" \
    --allow-legacy-pre-v0260-nonsemantic \
    >"$tmp/concurrent-first.stdout" 2>"$tmp/concurrent-first.stderr" &
concurrent_first_pid=$!
wait_for_file "$concurrent_first_renamed"

SIGNATURE_PATH="$concurrent_signature" node - <<'NODE'
const fs = require("fs");
const mode = fs.statSync(process.env.SIGNATURE_PATH).mode & 0o777;
if (mode !== 0o644) {
  console.error(`renamed signature has mode ${mode.toString(8)}, expected 644`);
  process.exit(1);
}
NODE
if flock -n "$concurrent_lock" true; then
  echo "signer did not retain its output publication lock after rename" >&2
  exit 1
fi

CTX_TEST_CONCURRENT_ROLE=second \
CTX_TEST_CONCURRENT_SIGNATURE="$concurrent_signature" \
CTX_TEST_CONCURRENT_EVENT_PREFIX="$tmp/concurrent-events" \
CTX_TEST_CONCURRENT_SECOND_AT_LOCK="$concurrent_second_at_lock" \
CTX_TEST_REAL_FLOCK="$real_flock" \
NODE_OPTIONS="--require=$concurrent_preload" \
PATH="$concurrent_bin:$PATH" \
CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  scripts/release/ctx_cli_release_metadata_sign.sh \
    --metadata "$metadata" \
    --out "$concurrent_signature" \
    --allow-legacy-pre-v0260-nonsemantic \
    >"$tmp/concurrent-second.stdout" 2>"$tmp/concurrent-second.stderr" &
concurrent_second_pid=$!
wait_for_file "$concurrent_second_at_lock"
kill -0 "$concurrent_second_pid"
if [[ -e "$tmp/concurrent-events.second" ]]; then
  echo "second signer entered publication while the first signer held the lock" >&2
  exit 1
fi
touch "$concurrent_release_first"

if wait "$concurrent_first_pid"; then
  echo "first signer unexpectedly succeeded after post-fsync metadata replacement" >&2
  exit 1
fi
concurrent_first_pid=""
wait "$concurrent_second_pid"
concurrent_second_pid=""

grep -F \
  "metadata input changed during signature publication" \
  "$tmp/concurrent-first.stderr" >/dev/null
cmp "$concurrent_first_metadata" "$concurrent_first_replacement"
cmp "$concurrent_signature" "$signature"
if compgen -G "${concurrent_signature}.*.tmp" >/dev/null; then
  echo "concurrent signing left a temporary signature behind" >&2
  exit 1
fi

FIRST_EVENTS="$tmp/concurrent-events.first" \
SECOND_EVENTS="$tmp/concurrent-events.second" \
node - <<'NODE'
const fs = require("fs");

function readEvents(path) {
  return fs.readFileSync(path, "utf8").trim().split("\n");
}

const expectedFirst = [
  "node-start",
  "file-fsync",
  "rename",
  "directory-fsync",
  "metadata-replaced",
  "output-unlink",
  "directory-fsync",
];
const expectedSecond = [
  "node-start",
  "file-fsync",
  "rename",
  "directory-fsync",
];
const first = readEvents(process.env.FIRST_EVENTS);
const second = readEvents(process.env.SECOND_EVENTS);
if (JSON.stringify(first) !== JSON.stringify(expectedFirst)) {
  console.error(`unexpected first publication events: ${JSON.stringify(first)}`);
  process.exit(1);
}
if (JSON.stringify(second) !== JSON.stringify(expectedSecond)) {
  console.error(`unexpected second publication events: ${JSON.stringify(second)}`);
  process.exit(1);
}
NODE

if scripts/release/ctx_cli_release_metadata_sign.sh \
  --metadata "$metadata" \
  --out "$signature.missing" \
  --allow-legacy-pre-v0260-nonsemantic \
  >/dev/null 2>"$tmp/missing-key.stderr"; then
  echo "signer unexpectedly succeeded without CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM" >&2
  exit 1
fi
grep -F "CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM" "$tmp/missing-key.stderr" >/dev/null
test ! -e "$signature.missing"

if CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="not-a-private-key" \
  scripts/release/ctx_cli_release_metadata_sign.sh \
    --metadata "$metadata" --out "$tmp/invalid-key.sig" \
    --allow-legacy-pre-v0260-nonsemantic \
    >"$tmp/invalid-key.stdout" 2>"$tmp/invalid-key.stderr"; then
  echo "signer unexpectedly accepted an invalid signing key" >&2
  exit 1
fi
grep -F "failed to sign metadata" "$tmp/invalid-key.stderr" >/dev/null
test ! -e "$tmp/invalid-key.sig"

# A failed atomic rename must leave the existing output directory intact and
# remove the temporary signature, without changing the input metadata.
mkdir "$tmp/output-directory"
cp "$metadata" "$tmp/output-input.env"
if CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  scripts/release/ctx_cli_release_metadata_sign.sh \
    --metadata "$metadata" --out "$tmp/output-directory" \
    --allow-legacy-pre-v0260-nonsemantic \
    >"$tmp/output-directory.stdout" 2>"$tmp/output-directory.stderr"; then
  echo "signer unexpectedly replaced an output directory" >&2
  exit 1
fi
grep -F "failed to publish metadata signature atomically" "$tmp/output-directory.stderr" >/dev/null
test -d "$tmp/output-directory"
cmp "$metadata" "$tmp/output-input.env"
if compgen -G "$tmp/output-directory.*.tmp" >/dev/null; then
  echo "failed output rename left a temporary signature behind" >&2
  exit 1
fi

metadata_alias="$tmp/metadata-alias.env"
cp "$metadata" "$metadata_alias"
if CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  scripts/release/ctx_cli_release_metadata_sign.sh \
    --metadata "$metadata_alias" \
    --out "$metadata_alias" \
    >"$tmp/alias.stdout" 2>"$tmp/alias.stderr"; then
  echo "signer unexpectedly overwrote metadata with its signature" >&2
  exit 1
fi
grep -F "must not overwrite release metadata" "$tmp/alias.stderr" >/dev/null
cmp "$metadata" "$metadata_alias"

semantic_metadata="$tmp/semantic-release-metadata.env"
semantic_signature="$tmp/semantic-release-metadata.env.sig"
cp "$metadata" "$semantic_metadata"
printf '%s\n' 'CTX_RELEASE_SEMANTIC_SCHEMA_VERSION=1' >>"$semantic_metadata"
if CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  scripts/release/ctx_cli_release_metadata_sign.sh \
    --metadata "$semantic_metadata" \
    --out "$semantic_signature" \
    >"$tmp/partial-semantic.stdout" 2>"$tmp/partial-semantic.stderr"; then
  echo "signer unexpectedly signed incomplete semantic metadata" >&2
  exit 1
fi
grep -F "wrong semantic field set" "$tmp/partial-semantic.stderr" >/dev/null
test ! -e "$semantic_signature"

mkdir "$tmp/semantic-artifacts"
if CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  scripts/release/ctx_cli_release_metadata_sign.sh \
    --metadata "$metadata" \
    --out "$tmp/non-semantic-with-artifacts.sig" \
    --semantic-artifact-dir "$tmp/semantic-artifacts" \
    --allow-legacy-pre-v0260-nonsemantic \
    >"$tmp/non-semantic.stdout" 2>"$tmp/non-semantic.stderr"; then
  echo "signer unexpectedly accepted an artifact directory without semantic metadata" >&2
  exit 1
fi
grep -F "metadata has no semantic fields" "$tmp/non-semantic.stderr" >/dev/null

candidate_repo="$tmp/public-ctx"
candidate_signer_root="$tmp/signer-fixture"
candidate_handoff="$tmp/candidate-handoff"
candidate_metadata="$tmp/candidate-release-metadata.env"
candidate_signature="$tmp/candidate-release-metadata.env.sig"
candidate_verifier_log="$tmp/candidate-verifier.log"
authority_git_bin="$tmp/authority-git-bin"
real_git="$(command -v git)"
mkdir -p \
  "$candidate_repo/crates/ctx-cli/src" \
  "$candidate_repo/scripts" \
  "$candidate_signer_root/scripts/release" \
  "$candidate_repo/services/install-site/src" \
  "$candidate_handoff" \
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
# Discover only this fixture's annotated tags; keep real object/patch checks.
if [[ "${1:-}" == "-C" && "${3:-}" == "ls-remote" &&
  "${4:-}" == "--tags" && "${5:-}" == "https://github.com/ctxrs/ctx.git" ]]; then
  "${CTX_TEST_REAL_GIT:?}" -C "$2" show-ref --tags --dereference | tr ' ' '\t'
  exit
fi
exec "${CTX_TEST_REAL_GIT:?}" "$@"
SH
chmod 755 "$authority_git_bin/git"
printf '[workspace.package]\nversion = "0.25.0"\n' >"$candidate_repo/Cargo.toml"
# Run unmodified signer/validators with a fixture-only release policy. This
# legacy signer test must not depend on the project's live published tags.
cp scripts/release/ctx_cli_release_metadata_sign.sh "$candidate_signer_root/scripts/release/"
for validator in release-candidate-manifest-contract.cjs released-source-continuity.py \
  semantic_runtime_metadata.py semantic_runtime_archive.py semantic-runtime-layout-v1.json; do
  cp "scripts/release/$validator" "$candidate_signer_root/scripts/release/"
done
printf '%s\n' '{"minimum_release":"0.0.0","required_patches":[],"dispositions":{}}' \
  >"$candidate_signer_root/scripts/release/released-source-continuity.json"
candidate_signer="$candidate_signer_root/scripts/release/ctx_cli_release_metadata_sign.sh"
printf '[package]\nname = "ctx"\nversion = "0.25.0"\n' \
  >"$candidate_repo/crates/ctx-cli/Cargo.toml"
PUBLIC_KEY_PATH="$public_key" \
PUBLIC_INSTALLER_PATH="$candidate_repo/services/install-site/src/cli-install-script.js" \
PUBLIC_UPGRADE_PATH="$candidate_repo/crates/ctx-cli/src/upgrade.rs" node - <<'NODE'
const crypto = require("node:crypto");
const fs = require("node:fs");
const publicKey = fs.readFileSync(process.env.PUBLIC_KEY_PATH, "utf8").trim();
const pkcs1 = crypto.createPublicKey(publicKey).export({
  format: "pem",
  type: "pkcs1",
}).trim();
fs.writeFileSync(
  process.env.PUBLIC_INSTALLER_PATH,
  `const DEFAULT_METADATA_PUBLIC_KEY_PEM = \`${publicKey}\`;\n`,
);
fs.writeFileSync(
  process.env.PUBLIC_UPGRADE_PATH,
  `const RELEASE_METADATA_PUBLIC_KEY_PEM: &str = r#"${pkcs1}"#;\n`,
);
NODE
cat >"$candidate_repo/scripts/release-sbom.py" <<'PY'
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
    "normal-ci.json",
    "release-validation.json",
    "windows-authenticode.json",
}
if os.environ.get("CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM") or os.environ.get(
    "CTX_CLI_METADATA_SIGNING_PRIVATE_KEY"
):
    raise SystemExit("release verifier inherited a metadata signing key")
if len(sys.argv) != 6 or sys.argv[1:3] != ["verify-release", "--handoff-dir"] or sys.argv[4] != "--expected-handoff-sha256":
    raise SystemExit("wrong release verifier interface")
handoff = Path(sys.argv[3])
if {entry.name for entry in handoff.iterdir()} != expected_names:
    raise SystemExit("release authority handoff does not have the exact production inventory")
actual = hashlib.sha256((handoff / "ctx-core-github-handoff.json").read_bytes()).hexdigest()
if actual != sys.argv[5]:
    raise SystemExit("Core GitHub handoff digest does not match expected digest")
Path(os.environ["CTX_TEST_VERIFIER_LOG"]).write_text("verify-release\n")
print(actual)
PY
git -C "$candidate_repo" init -q
git -C "$candidate_repo" checkout -q -b main
git -C "$candidate_repo" config user.email ctx-signer@example.test
git -C "$candidate_repo" config user.name "ctx signer fixture"
git -C "$candidate_repo" add .
git -C "$candidate_repo" commit -qm "candidate verifier fixture"
candidate_source_commit="$(git -C "$candidate_repo" rev-parse HEAD)"
git -C "$candidate_repo" tag -a v0.24.0 -m 'fixture released source'

CANDIDATE_HANDOFF="$candidate_handoff" \
CANDIDATE_METADATA="$candidate_metadata" \
CANDIDATE_SOURCE_COMMIT="$candidate_source_commit" \
  node - <<'NODE'
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const matrix = [
  ["linux_x64", "linux-x64", "linux-x64", "ctx", "scripts/release/build-public-candidate-on-linux.sh", "x86_64-unknown-linux-gnu", "ctx.candidate.json"],
  ["linux_aarch64", "linux-arm64", "linux-aarch64", "ctx-linux-aarch64", "scripts/release/build-public-candidate-on-linux.sh", "aarch64-unknown-linux-gnu", "ctx-linux-aarch64.candidate.json"],
  ["macos_arm64", "macos-arm64", "macos-arm64", "ctx-macos-arm64", "scripts/release/build-public-candidate-on-linux.sh", "aarch64-apple-darwin", "ctx-macos-arm64.candidate.json"],
  ["macos_x64", "macos-x64", "macos-x64", "ctx-macos-x64", "scripts/release/build-public-candidate-on-linux.sh", "x86_64-apple-darwin", "ctx-macos-x64.candidate.json"],
  ["windows_x64", "windows-x64", "windows-x64", "ctx.exe", "scripts/release/build-public-candidate-on-linux.sh", "x86_64-pc-windows-gnu", "ctx.exe.candidate.json"],
];
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}
const handoff = process.env.CANDIDATE_HANDOFF;
const artifactSha = "a".repeat(64);
const lines = [
  "CTX_RELEASE_SCHEMA_VERSION=1",
  "CTX_RELEASE_VERSION=0.25.0",
  "CTX_RELEASE_CHANNEL=stable",
  "CTX_RELEASE_BASE_URL=https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/stable/0.25.0",
  `CTX_RELEASE_SOURCE_COMMIT=${process.env.CANDIDATE_SOURCE_COMMIT}`,
];
for (const [key, id, platform, artifact, constructionLabel, rustTriple, manifest] of matrix) {
  const candidate = {
    artifact: { file: artifact, sha256: artifactSha, size_bytes: 1 },
    construction: {
      authority: "linux-cross-cargo-zigbuild-v1",
      label: constructionLabel,
    },
    evidence: {},
    kind: "ctx-public-cli-candidate",
    product: "core",
    schema_version: 1,
    source: { clean: true, commit: process.env.CANDIDATE_SOURCE_COMMIT },
    tantivy: {},
    target: { id, platform, rust_triple: rustTriple },
    version: "0.25.0",
  };
  const bytes = Buffer.from(`${canonical(candidate)}\n`);
  const digest = crypto.createHash("sha256").update(bytes).digest("hex");
  fs.writeFileSync(path.join(handoff, manifest), bytes);
  fs.writeFileSync(path.join(handoff, `${manifest}.sha256`), `${digest}\n`);
  lines.push(`CTX_RELEASE_ARTIFACT_${key}=${artifact}`);
  lines.push(`CTX_RELEASE_SHA256_${key}=${artifactSha}`);
  lines.push(`CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_${key}=${digest}`);
}
for (const name of [
  "SHA256SUMS",
  "ctx.exe",
  "ctx.exe.build-info.json",
  "ctx.exe.cdx.json",
  "ctx.exe.size.json",
  "ctx.exe.third-party-notices.txt",
  "ctx-core.release-complete.json",
  "ctx-release-factory.json",
  "normal-ci.json",
  "release-validation.json",
  "windows-authenticode.json",
]) {
  fs.writeFileSync(path.join(handoff, name), `fixture ${name}\n`);
}
const handoffBytes = Buffer.from('{"kind":"fixture"}\n', "utf8");
const handoffSha256 = crypto.createHash("sha256").update(handoffBytes).digest("hex");
fs.writeFileSync(path.join(handoff, "ctx-core-github-handoff.json"), handoffBytes);
fs.writeFileSync(
  path.join(handoff, "ctx-core-github-handoff.json.sha256"),
  `${handoffSha256}\n`,
);
lines.push(`CTX_RELEASE_CORE_GITHUB_HANDOFF_SHA256=${handoffSha256}`);
fs.writeFileSync(process.env.CANDIDATE_METADATA, `${lines.join("\n")}\n`);
NODE
candidate_handoff_sha256="$(sha256sum \
  "$candidate_handoff/ctx-core-github-handoff.json" | awk '{print $1}')"

if CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="not-a-private-key" \
  "$candidate_signer" \
    --metadata "$candidate_metadata" \
    --out "$tmp/candidate-wrong-handoff.sig" \
    --candidate-manifest-handoff "$candidate_handoff" \
    --candidate-handoff-sha256 "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" \
    --public-ctx-repo "$candidate_repo" \
    --allow-legacy-pre-v0260-nonsemantic \
    >"$tmp/candidate-wrong-handoff.stdout" \
    2>"$tmp/candidate-wrong-handoff.stderr"; then
  echo "signer unexpectedly accepted the wrong independent handoff digest" >&2
  exit 1
fi
grep -F "does not match --candidate-handoff-sha256" \
  "$tmp/candidate-wrong-handoff.stderr" >/dev/null
test ! -e "$tmp/candidate-wrong-handoff.sig"
test ! -e "$candidate_verifier_log"

CTX_TEST_VERIFIER_LOG="$candidate_verifier_log" \
CTX_TEST_REAL_GIT="$real_git" \
CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
PATH="$authority_git_bin:$PATH" \
  "$candidate_signer" \
    --metadata "$candidate_metadata" \
    --out "$candidate_signature" \
    --candidate-manifest-handoff "$candidate_handoff" \
    --candidate-handoff-sha256 "$candidate_handoff_sha256" \
    --public-ctx-repo "$candidate_repo" \
    --allow-legacy-pre-v0260-nonsemantic >/dev/null
test "$(cat "$candidate_verifier_log")" = "verify-release"
METADATA_PATH="$candidate_metadata" \
SIGNATURE_PATH="$candidate_signature" \
PUBLIC_KEY_PATH="$public_key" node - <<'NODE'
const crypto = require("node:crypto");
const fs = require("node:fs");
const ok = crypto.verify(
  "RSA-SHA256",
  fs.readFileSync(process.env.METADATA_PATH),
  {
    key: fs.readFileSync(process.env.PUBLIC_KEY_PATH, "utf8"),
    padding: crypto.constants.RSA_PKCS1_PADDING,
  },
  Buffer.from(fs.readFileSync(process.env.SIGNATURE_PATH, "utf8").trim(), "base64"),
);
if (!ok) process.exit(1);
NODE

authenticated_digest="$(
  CTX_TEST_REAL_GIT="$real_git" \
  CTX_TEST_VERIFIER_LOG="$candidate_verifier_log" \
  PATH="$authority_git_bin:$PATH" \
  node - "$candidate_signer_root/scripts/release/release-candidate-manifest-contract.cjs" \
    "$candidate_metadata" "$candidate_signature" \
    "$candidate_handoff" "$candidate_repo" <<'NODE'
const path = require("node:path");
const contract = require(path.resolve(process.argv[2]));
const digests = contract.verifySignedCandidateManifestHandoff({
  metadataPath: process.argv[3],
  signaturePath: process.argv[4],
  handoffDir: process.argv[5],
  publicRepo: process.argv[6],
});
process.stdout.write(`${digests.windows_x64}\n`);
NODE
)"
test "$authenticated_digest" = "$(sha256sum "$candidate_handoff/ctx.exe.candidate.json" | awk '{print $1}')"

git -C "$candidate_repo" checkout -qb released-fix
printf 'released repair\n' >"$candidate_repo/released-fix"
git -C "$candidate_repo" add released-fix
git -C "$candidate_repo" commit -qm 'fixture released repair'
git -C "$candidate_repo" tag -a v0.25.0 -m 'fixture release with repair'
git -C "$candidate_repo" checkout -q main
if CTX_TEST_REAL_GIT="$real_git" PATH="$authority_git_bin:$PATH" \
  CTX_CLI_METADATA_SIGNING_PRIVATE_KEY="$tmp/absent-private-key" \
  "$candidate_signer" --metadata "$candidate_metadata" \
    --out "$tmp/candidate-omitted-fix.sig" \
    --candidate-manifest-handoff "$candidate_handoff" \
    --candidate-handoff-sha256 "$candidate_handoff_sha256" \
    --public-ctx-repo "$candidate_repo" --allow-legacy-pre-v0260-nonsemantic \
    >"$tmp/candidate-omitted-fix.stdout" 2>"$tmp/candidate-omitted-fix.stderr"; then
  echo "signer accepted a candidate missing a released fix" >&2
  exit 1
fi
grep -F 'released changes are absent without a reviewed disposition' \
  "$tmp/candidate-omitted-fix.stderr" >/dev/null
test ! -e "$tmp/candidate-omitted-fix.sig"
git -C "$candidate_repo" tag -d v0.25.0 >/dev/null

candidate_replacement="$tmp/candidate-replacement.env"
sed '/^CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_macos_x64=/d' \
  "$candidate_metadata" >"$candidate_replacement"
if CTX_TEST_VERIFIER_LOG="$candidate_verifier_log" \
  CTX_TEST_REAL_GIT="$real_git" \
  CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  PATH="$authority_git_bin:$PATH" \
  "$candidate_signer" \
    --metadata "$candidate_replacement" \
    --out "$candidate_replacement.sig" \
    --candidate-manifest-handoff "$candidate_handoff" \
    --candidate-handoff-sha256 "$candidate_handoff_sha256" \
    --public-ctx-repo "$candidate_repo" \
    --allow-legacy-pre-v0260-nonsemantic \
    >"$tmp/candidate-incomplete.stdout" 2>"$tmp/candidate-incomplete.stderr"; then
  echo "signer unexpectedly accepted an incomplete candidate manifest matrix" >&2
  exit 1
fi
grep -F "missing macos_x64" "$tmp/candidate-incomplete.stderr" >/dev/null
test ! -e "$candidate_replacement.sig"

if CTX_TEST_VERIFIER_LOG="$candidate_verifier_log" \
  CTX_TEST_REAL_GIT="$real_git" \
  CTX_TEST_REJECT_PUBLIC_AUTHORITY_DESCENDANT=1 \
  CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  PATH="$authority_git_bin:$PATH" \
  "$candidate_signer" \
    --metadata "$candidate_metadata" \
    --out "$tmp/candidate-wrong-authority.sig" \
    --candidate-manifest-handoff "$candidate_handoff" \
    --candidate-handoff-sha256 "$candidate_handoff_sha256" \
    --public-ctx-repo "$candidate_repo" \
    --allow-legacy-pre-v0260-nonsemantic \
    >"$tmp/candidate-wrong-authority.stdout" \
    2>"$tmp/candidate-wrong-authority.stderr"; then
  echo "signer unexpectedly accepted a public source outside the pinned authority history" >&2
  exit 1
fi
grep -F "is not a descendant of manifest authority 4eb7234af45b568a4200e7331570d9056a1c5cdd" \
  "$tmp/candidate-wrong-authority.stderr" >/dev/null
test ! -e "$tmp/candidate-wrong-authority.sig"

ln -s "$candidate_handoff" "$tmp/candidate-handoff-link"
if CTX_TEST_VERIFIER_LOG="$candidate_verifier_log" \
  CTX_TEST_REAL_GIT="$real_git" \
  CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM="$(cat "$private_key")" \
  PATH="$authority_git_bin:$PATH" \
  "$candidate_signer" \
    --metadata "$candidate_metadata" \
    --out "$tmp/candidate-link.sig" \
    --candidate-manifest-handoff "$tmp/candidate-handoff-link" \
    --candidate-handoff-sha256 "$candidate_handoff_sha256" \
    --public-ctx-repo "$candidate_repo" \
    --allow-legacy-pre-v0260-nonsemantic \
    >"$tmp/candidate-link.stdout" 2>"$tmp/candidate-link.stderr"; then
  echo "signer unexpectedly accepted a linked candidate manifest handoff" >&2
  exit 1
fi
grep -F "must be a non-symlink directory" "$tmp/candidate-link.stderr" >/dev/null
test ! -e "$tmp/candidate-link.sig"

# Real closed Python schema must refuse changed/partial/arbitrary runtime fields
# before the signer reads the deliberately absent private key. Other authority
# files are placeholders: this test is the signer's schema boundary only.
SIGNER_SCHEMA_ROOT="$tmp" python3 - <<'PY'
from pathlib import Path
import os
root = Path(os.environ["SIGNER_SCHEMA_ROOT"])
lines = ["CTX_RELEASE_SCHEMA_VERSION=1", "CTX_RELEASE_VERSION=1.3.3", "CTX_RELEASE_CHANNEL=stable", "CTX_RELEASE_ONNXRUNTIME_VERSION=1.27.0"]
for target, artifact in [("linux_x64", "ctx-onnxruntime-linux-x64.tar.gz"), ("linux_aarch64", "ctx-onnxruntime-linux-aarch64.tar.gz"), ("windows_x64", "ctx-onnxruntime-windows-x64.zip"), ("macos_x64", "ctx-onnxruntime-macos-x64.tar.gz"), ("macos_arm64", "ctx-onnxruntime-macos-arm64.tar.gz")]:
    lines += [f"CTX_RELEASE_ONNXRUNTIME_ARTIFACT_{target}={artifact}", f"CTX_RELEASE_ONNXRUNTIME_SHA256_{target}=" + "a" * 64]
    for field in ("ENVELOPE", "CORE_OBJECT", "CORE_SHA256", "COMPANION_OBJECT", "COMPANION_SHA256"):
        lines += [f"CTX_RELEASE_MANAGED_PAIR_{field}_{target}=fixture"]
text = "\n".join(lines) + "\n"
variants = {
    "partial-artifact": "\n".join(line for line in lines if "ARTIFACT_linux_x64" not in line) + "\n",
    "partial-digest": "\n".join(line for line in lines if "SHA256_linux_x64" not in line) + "\n",
    "altered": text.replace("a" * 64, "z" * 64),
    "arbitrary": text + "CTX_RELEASE_ONNXRUNTIME_ARTIFACT_freebsd_x64=extra.tar.gz\n",
    "malformed-version": text.replace("1.27.0", "01.28.0"),
    "unknown": text + "CTX_RELEASE_ONNXRUNTIME_ARTIFACT_other_x64=other.tar.gz\n",
}
for name, value in variants.items():
    (root / f"schema-{name}.env").write_text(value)
PY
for schema_case in partial-artifact partial-digest altered arbitrary malformed-version unknown; do
  if CTX_CLI_METADATA_SIGNING_PRIVATE_KEY="$tmp/absent-private-key" \
    scripts/release/ctx_cli_release_metadata_sign.sh \
      --metadata "$tmp/schema-$schema_case.env" \
      --out "$tmp/schema-$schema_case.sig" \
      --managed-pair-publication "$tmp/managed-publication.json" \
      --runtime-handoff "$tmp/runtime-handoff.json" \
      --public-ctx-repo "$tmp/public-repo" \
      --semantic-artifact-dir "$tmp/managed-semantic" \
      --candidate-manifest-handoff "$tmp/managed-candidates" \
      --candidate-handoff-sha256 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
      >"$tmp/schema-$schema_case.stdout" 2>"$tmp/schema-$schema_case.stderr"; then
    echo "signer accepted $schema_case historical runtime metadata" >&2; exit 1
  fi
  grep -F 'invalid platform compatibility ONNX Runtime fields' "$tmp/schema-$schema_case.stderr" >/dev/null
  test ! -e "$tmp/schema-$schema_case.sig"
done

printf 'ctx CLI release metadata signing contract: OK\n'
