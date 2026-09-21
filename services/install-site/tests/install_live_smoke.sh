#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: missing required command: $1" >&2
    exit 1
  }
}

run_isolated_lifecycle_smoke() {
  local candidate="$1"
  [[ -x "$candidate" ]] || {
    echo "error: CTX_INSTALL_LIFECYCLE_CTX_BINARY must name an executable" >&2
    exit 1
  }
  for command in node openssl gzip sha256sum stat strip; do
    need_cmd "$command"
  done

  local fixture
  fixture="$(mktemp -d /tmp/ctx-install-lifecycle-live.XXXXXX)"
  local fixture_home="$fixture/home"
  local fixture_bin="$fixture/bin"
  local fixture_man="$fixture/man/man1"
  local fixture_tmp="$fixture/tmp"
  local fixture_fake_bin="$fixture/fake-bin"
  local fixture_release="$fixture/release"
  local canonical_root="$fixture_home/.ctx"
  local custom_root="$fixture/custom-root"
  local second_root="$fixture/second-root"
  local installer="$fixture/install.sh"
  local uninstaller="$fixture/uninstall.sh"
  local installed="$fixture_bin/ctx"
  local public_key="$fixture/public.pem"
  local private_key="$fixture/private.pem"
  local metadata="$fixture/ctx-release-metadata.env"
  local metadata_signature="$metadata.sig"
  local artifact="$fixture_release/ctx-linux-x64"
  local artifact_gzip="$artifact.gz"
  local network_log="$fixture/network.log"
  mkdir -p \
    "$fixture_home" "$fixture_bin" "$fixture_man" "$fixture_tmp" \
    "$fixture_fake_bin" "$fixture_release" "$custom_root" "$second_root" \
    "$fixture/xdg-config" "$fixture/xdg-data" "$fixture/xdg-state" \
    "$fixture/xdg-cache" "$fixture/xdg-runtime" \
    "$fixture/providers/codex/sessions/2026/07/30" "$fixture/providers/claude" \
    "$fixture/providers/copilot"
  chmod 0700 "$fixture/xdg-runtime"
  cat >"$fixture/providers/codex/sessions/2026/07/30/rollout-live-fixture.jsonl" <<'JSONL'
{"timestamp":"2026-07-30T00:00:00Z","type":"session_meta","payload":{"id":"install-lifecycle-live-fixture","timestamp":"2026-07-30T00:00:00Z","cwd":"/workspace/install-lifecycle-live","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}
{"timestamp":"2026-07-30T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"isolated managed reinstall and custom-root uninstall fixture"}]}}
JSONL

  local candidate_version
  candidate_version="$(env -i PATH=/usr/local/bin:/usr/bin:/bin \
    HOME="$fixture_home" CTX_DATA_ROOT="$canonical_root" TMPDIR="$fixture_tmp" \
    XDG_CONFIG_HOME="$fixture/xdg-config" XDG_DATA_HOME="$fixture/xdg-data" \
    XDG_STATE_HOME="$fixture/xdg-state" XDG_CACHE_HOME="$fixture/xdg-cache" \
    XDG_RUNTIME_DIR="$fixture/xdg-runtime" CODEX_HOME="$fixture/providers/codex" \
    CLAUDE_CONFIG_DIR="$fixture/providers/claude" COPILOT_HOME="$fixture/providers/copilot" \
    CTX_ANALYTICS_ENABLED=false CTX_DAEMON_ENABLED=false CTX_UPGRADE_AUTO=off \
    "$candidate" --version | awk '$1 == "ctx" { print $2; exit }')"
  [[ "$candidate_version" =~ ^0\.([2-9][6-9]|[3-9][0-9])\.|^[1-9][0-9]*\. ]] || {
    echo "skip: lifecycle fixture requires a v0.26-or-newer qualification candidate" >&2
    rm -rf "$fixture"
    return 0
  }

  cp "$candidate" "$artifact"
  chmod 0755 "$artifact"
  strip "$artifact"
  gzip -c "$artifact" >"$artifact_gzip"
  local artifact_sha256
  artifact_sha256="$(sha256sum "$artifact" | awk '{ print $1 }')"
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 \
    -out "$private_key" >/dev/null 2>&1
  openssl rsa -in "$private_key" -RSAPublicKey_out \
    -out "$public_key" >/dev/null 2>&1
  {
    printf '%s\n' \
      'CTX_RELEASE_SCHEMA_VERSION=1' \
      'CTX_RELEASE_CHANNEL=stable' \
      "CTX_RELEASE_VERSION=$candidate_version" \
      'CTX_RELEASE_BASE_URL=https://127.0.0.1/fixture-artifacts' \
      'CTX_RELEASE_ARTIFACT_linux_x64=ctx-linux-x64' \
      "CTX_RELEASE_SHA256_linux_x64=$artifact_sha256" \
      'CTX_RELEASE_SELF_UPGRADE_ALLOWED=true' \
      'CTX_RELEASE_AUTO_UPGRADE_ALLOWED=false' \
      'CTX_RELEASE_SOURCE_COMMIT=isolated-live-fixture' \
      'CTX_RELEASE_PUBLISHED_AT=2026-07-30T00:00:00Z'
  } >"$metadata"
  openssl dgst -sha256 -sign "$private_key" \
    -out "$fixture/metadata.sig.raw" "$metadata"
  openssl enc -A -base64 -in "$fixture/metadata.sig.raw" \
    -out "$metadata_signature"

  RENDER_PUBLIC_KEY="$public_key" \
  RENDER_INSTALLER="$installer" \
  RENDER_UNINSTALLER="$uninstaller" \
    node --input-type=module <<'NODE'
import { readFileSync, writeFileSync } from "node:fs";
import { renderCliInstallScript } from "./install-site/src/cli-install-script.js";
import { renderUninstallScript } from "./install-site/src/uninstall-script.js";

writeFileSync(process.env.RENDER_INSTALLER, renderCliInstallScript({
  releaseFunctionsBase: "https://fixture.invalid/functions/v1",
  installTelemetryEndpoint: "https://fixture.invalid/install-attempt",
  metadataPublicKeyPem: readFileSync(process.env.RENDER_PUBLIC_KEY, "utf8"),
}));
writeFileSync(process.env.RENDER_UNINSTALLER, renderUninstallScript({
  functionsBase: "https://fixture.invalid/functions/v1",
}));
NODE
  chmod 0755 "$installer" "$uninstaller"

  cat >"$fixture_fake_bin/curl" <<'SH'
#!/bin/sh
destination=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) shift; destination="$1" ;;
    --data|--data-binary|-H|-X|--retry|--connect-timeout|--max-time|--proto)
      shift
      ;;
    https://*) url="$1" ;;
  esac
  shift
done
printf '%s\n' "$url" >>"$CTX_INSTALL_LIFECYCLE_NETWORK_LOG"
case "$url" in
  https://fixture.invalid/ctx-release-metadata.env)
    cp "$CTX_INSTALL_LIFECYCLE_METADATA" "$destination"
    ;;
  https://fixture.invalid/ctx-release-metadata.env.sig)
    cp "$CTX_INSTALL_LIFECYCLE_METADATA.sig" "$destination"
    ;;
  https://127.0.0.1/fixture-artifacts/ctx-linux-x64.gz)
    cp "$CTX_INSTALL_LIFECYCLE_ARTIFACT.gz" "$destination"
    ;;
  https://127.0.0.1/fixture-artifacts/ctx-linux-x64)
    cp "$CTX_INSTALL_LIFECYCLE_ARTIFACT" "$destination"
    ;;
  https://fixture.invalid/install-attempt|https://fixture.invalid/functions/v1/install-attempt)
    ;;
  *)
    echo "error: isolated fixture rejected network URL: $url" >&2
    exit 44
    ;;
esac
SH
  cat >"$fixture_fake_bin/systemctl" <<'SH'
#!/bin/sh
test "${1:-}" != "--user" || shift
manager_root="$XDG_RUNTIME_DIR/ctx-fixture-systemd"
enabled_path="$manager_root/enabled"
pid_path="$manager_root/main.pid"
unit_path="$XDG_CONFIG_HOME/systemd/user/ctx.service"
owner_lock="$HOME/.ctx/daemon/daemon.lock"
mkdir -p "$manager_root"

manager_process_pid() {
  test -f "$pid_path" || return 1
  pid="$(sed -n '1p' "$pid_path")"
  case "$pid" in
    ''|*[!0-9]*) return 1 ;;
  esac
  kill -0 "$pid" 2>/dev/null || return 1
  state="$(ps -o stat= -p "$pid" 2>/dev/null)" || return 1
  case "$state" in
    *Z*) return 1 ;;
  esac
}

manager_owner_active() {
  manager_process_pid || return 1
  test -f "$owner_lock" || return 1
  grep -Eq "\"pid\"[[:space:]]*:[[:space:]]*$pid([,}])" "$owner_lock"
}

case "${1:-} ${2:-} ${3:-}" in
  "daemon-reload  ")
    exit 0
    ;;
  "enable ctx.service ")
    test -f "$unit_path" || exit 1
    : >"$enabled_path"
    exit 0
    ;;
  "start ctx.service ")
    test -f "$enabled_path" && test -f "$unit_path" || exit 1
    manager_owner_active && exit 0
    service_command="$(sed -n 's/^ExecStart=//p' "$unit_path")"
    test -n "$service_command" || exit 1
    setsid sh -c "exec $service_command" \
      >"$manager_root/service.out" 2>"$manager_root/service.err" &
    printf '%s\n' "$!" >"$pid_path"
    exit 0
    ;;
  "is-enabled ctx.service ")
    if test -f "$enabled_path" && test -f "$unit_path"; then
      printf '%s\n' enabled
      exit 0
    fi
    printf '%s\n' disabled
    exit 1
    ;;
  "is-active ctx.service ")
    if manager_owner_active; then
      printf '%s\n' active
      exit 0
    fi
    printf '%s\n' inactive
    exit 1
    ;;
  "show ctx.service --property=MainPID")
    manager_owner_active || exit 1
    printf '%s\n' "$pid"
    exit 0
    ;;
  "disable --now ctx.service")
    if manager_owner_active; then
      kill -TERM "$pid" 2>/dev/null || exit 1
    fi
    attempt=0
    while manager_process_pid && test "$attempt" -lt 200; do
      sleep 0.025
      attempt=$((attempt + 1))
    done
    manager_process_pid && exit 1
    rm -f "$enabled_path" "$pid_path"
    exit 0
    ;;
  *)
    exit 1
    ;;
esac
SH
  chmod 0755 "$fixture_fake_bin/curl" "$fixture_fake_bin/systemctl"

  local public_key_body
  public_key_body="$(cat "$public_key")"
  local -a isolated_env=(
    "HOME=$fixture_home"
    "PATH=$fixture_fake_bin:$fixture_bin:/usr/local/bin:/usr/bin:/bin"
    "TMPDIR=$fixture_tmp"
    "XDG_CONFIG_HOME=$fixture/xdg-config"
    "XDG_DATA_HOME=$fixture/xdg-data"
    "XDG_STATE_HOME=$fixture/xdg-state"
    "XDG_CACHE_HOME=$fixture/xdg-cache"
    "XDG_RUNTIME_DIR=$fixture/xdg-runtime"
    "CODEX_HOME=$fixture/providers/codex"
    "CLAUDE_CONFIG_DIR=$fixture/providers/claude"
    "COPILOT_HOME=$fixture/providers/copilot"
    "CTX_BIN_DIR=$fixture_bin"
    "CTX_MAN_DIR=$fixture_man"
    "CTX_PLATFORM=linux-x64"
    "CTX_RELEASE_METADATA_URL=https://fixture.invalid/ctx-release-metadata.env"
    "CTX_RELEASE_METADATA_SIGNATURE_URL=https://fixture.invalid/ctx-release-metadata.env.sig"
    "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM=$public_key_body"
    "CTX_ALLOW_CUSTOM_RELEASE_BASE_URL=1"
    "CTX_ANALYTICS_ENABLED=false"
    "CTX_UPGRADE_AUTO=off"
    "CTX_SEARCH_SEMANTIC=false"
    "CTX_SETUP_PROGRESS=none"
    "CTX_INSTALL_LIFECYCLE_METADATA=$metadata"
    "CTX_INSTALL_LIFECYCLE_ARTIFACT=$artifact"
    "CTX_INSTALL_LIFECYCLE_NETWORK_LOG=$network_log"
    "HTTP_PROXY=http://127.0.0.1:9"
    "HTTPS_PROXY=http://127.0.0.1:9"
    "ALL_PROXY=http://127.0.0.1:9"
    "NO_PROXY="
    "DBUS_SESSION_BUS_ADDRESS="
  )
  lifecycle_fixture="$fixture"
  lifecycle_installed="$installed"
  lifecycle_custom_root="$custom_root"
  lifecycle_isolated_env=("${isolated_env[@]}")

  cleanup_lifecycle_fixture() {
    local status="$?"
    trap - EXIT
    if [[ -x "${lifecycle_installed:-}" ]]; then
      if ! env -i "${lifecycle_isolated_env[@]}" CTX_DATA_ROOT="$lifecycle_custom_root" \
          "$lifecycle_installed" daemon disable --prepare-uninstall \
          --format=json >/dev/null 2>&1; then
        echo "warning: retained failed lifecycle fixture for safe teardown: $lifecycle_fixture" >&2
        exit "$status"
      fi
    fi
    if [[ "$status" -ne 0 &&
          "${CTX_INSTALL_LIFECYCLE_RETAIN_ON_FAILURE:-0}" == "1" ]]; then
      echo "warning: retained safely quiesced lifecycle fixture: $lifecycle_fixture" >&2
      exit "$status"
    fi
    rm -rf "$lifecycle_fixture"
    exit "$status"
  }
  trap cleanup_lifecycle_fixture EXIT

  if ! env -i "${isolated_env[@]}" CTX_DATA_ROOT="$canonical_root" \
      sh "$installer" --no-setup --no-man --no-modify-path --no-pro-trial \
      >"$fixture/install-first.out" 2>"$fixture/install-first.err"; then
    echo "error: isolated first install/setup failed" >&2
    cat "$fixture/install-first.err" >&2
    return 1
  fi
  [[ -x "$installed" && -f "$installed.install.json" ]] || {
    echo "error: isolated installer did not publish its managed identity" >&2
    return 1
  }

  if ! env -i "${isolated_env[@]}" CTX_DATA_ROOT="$canonical_root" \
      "$installed" setup --wait --format=json --progress none \
      >"$fixture/canonical-setup.out" 2>"$fixture/canonical-setup.err"; then
    echo "error: isolated canonical-root setup failed" >&2
    cat "$fixture/canonical-setup.err" >&2
    return 1
  fi
  if ! env -i "${isolated_env[@]}" CTX_DATA_ROOT="$custom_root" \
      "$installed" setup --wait --format=json --progress none \
      >"$fixture/custom-setup.out" 2>"$fixture/custom-setup.err"; then
    echo "error: isolated custom-root setup failed" >&2
    cat "$fixture/custom-setup.err" >&2
    return 1
  fi
  if ! env -i "${isolated_env[@]}" CTX_DATA_ROOT="$second_root" \
      "$installed" setup --wait --format=json --progress none \
      >"$fixture/second-setup.out" 2>"$fixture/second-setup.err"; then
    echo "error: isolated second-root setup failed" >&2
    cat "$fixture/second-setup.err" >&2
    return 1
  fi

  wait_for_daemon() {
    local data_root="$1"
    local status_path="$2"
    local attempt
    for attempt in $(seq 1 100); do
      if env -i "${isolated_env[@]}" CTX_DATA_ROOT="$data_root" \
          "$installed" daemon status --format=json >"$status_path" 2>/dev/null &&
        node -e '
          const value = JSON.parse(require("node:fs").readFileSync(process.argv[1], "utf8"));
          process.exit(value.daemon?.enabled === true && value.daemon?.running === true ? 0 : 1);
        ' "$status_path"; then
        return 0
      fi
      sleep 0.1
    done
    echo "error: isolated daemon did not become ready for $data_root" >&2
    [[ ! -f "$status_path" ]] || cat "$status_path" >&2
    return 1
  }

  wait_for_daemon "$canonical_root" "$fixture/canonical-before.json"
  wait_for_daemon "$custom_root" "$fixture/custom-before.json"
  wait_for_daemon "$second_root" "$fixture/second-before.json"
  local inode_before digest_before
  inode_before="$(stat -c '%i' "$installed")"
  digest_before="$(sha256sum "$installed" | awk '{ print $1 }')"

  if ! env -i "${isolated_env[@]}" CTX_DATA_ROOT="$canonical_root" \
      sh "$installer" --no-setup --no-man --no-modify-path --no-pro-trial \
      >"$fixture/install-rerun.out" 2>"$fixture/install-rerun.err"; then
    echo "error: isolated managed rerun failed" >&2
    cat "$fixture/install-rerun.err" >&2
    return 1
  fi
  [[ "$(stat -c '%i' "$installed")" == "$inode_before" ]] || {
    echo "error: up-to-date managed rerun replaced the executable inode" >&2
    return 1
  }
  [[ "$(sha256sum "$installed" | awk '{ print $1 }')" == "$digest_before" ]] || {
    echo "error: up-to-date managed rerun changed the executable bytes" >&2
    return 1
  }
  wait_for_daemon "$canonical_root" "$fixture/canonical-after.json"
  wait_for_daemon "$custom_root" "$fixture/custom-after.json"
  wait_for_daemon "$second_root" "$fixture/second-after.json"

  if ! env -i "${isolated_env[@]}" CTX_DATA_ROOT="$custom_root" \
      sh "$uninstaller" --keep-data \
      >"$fixture/uninstall.out" 2>"$fixture/uninstall.err"; then
    echo "error: isolated custom-root hosted uninstall failed" >&2
    cat "$fixture/uninstall.err" >&2
    return 1
  fi
  [[ ! -e "$installed" && ! -e "$installed.install.json" &&
     ! -e "$installed.install-integrations" ]]
  [[ ! -e "$fixture/xdg-config/systemd/user/ctx.service" ]]
  [[ ! -e "$fixture/xdg-runtime/ctx-fixture-systemd/enabled" &&
     ! -e "$fixture/xdg-runtime/ctx-fixture-systemd/main.pid" ]]
  for data_root in "$canonical_root" "$custom_root" "$second_root"; do
    grep -Eq '^[[:space:]]*enabled[[:space:]]*=[[:space:]]*false' \
      "$data_root/config.toml"
    for residual in \
      daemon/daemon.lock daemon/daemon.guard daemon/query.sock \
      daemon/source-refresh.sock daemon/query-endpoint.json \
      daemon/source-refresh-endpoint.json daemon/supervisor.json \
      daemon/upgrade-handoff.json daemon/upgrade-restart-requests; do
      [[ ! -e "$data_root/$residual" ]] || {
        echo "error: residual lifecycle artifact: $data_root/$residual" >&2
        return 1
      }
    done
  done
  if grep -Ev '^https://(fixture.invalid|127\.0\.0\.1/fixture-artifacts)/' \
      "$network_log" >/dev/null; then
    echo "error: isolated fixture observed a non-fixture URL" >&2
    cat "$network_log" >&2
    return 1
  fi

  trap - EXIT
  rm -rf "$fixture"
  echo "ok: isolated live daemons survived managed hosted rerun"
  echo "ok: custom-root uninstall proved installation-wide teardown before image removal"
}

# Synthetic lifecycle evidence must never satisfy a deployment/live gate.
if [[ -n "${CTX_INSTALL_LIFECYCLE_CTX_BINARY:-}" ]]; then
  if [[ -n "${CTX_INSTALL_SMOKE_RESULT:-}${CTX_INSTALL_SMOKE_SCRIPT:-}${CTX_INSTALL_SMOKE_EXPECTED_VERSION:-}${CTX_INSTALL_SMOKE_EXPECTED_CORE_SHA256:-}${CTX_INSTALL_SMOKE_EXPECTED_PRO_SHA256:-}" ]]; then
    echo "error: lifecycle fixture mode cannot produce installation acceptance" >&2
    exit 1
  fi
  if [[ "$(uname -s)" != Linux ]]; then
    echo "error: lifecycle fixture requires Linux" >&2
    exit 1
  fi
  run_isolated_lifecycle_smoke "$CTX_INSTALL_LIFECYCLE_CTX_BINARY"
  exit 0
fi

need_cmd python3
exec python3 "$ROOT/install-site/tests/install_linux_smoke.py"
