import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { renderUninstallScript } from "../uninstall-script.js";
import { daemonUninstallResult } from "./daemon-uninstall-result-fixture.mjs";

export const makeTempDir = (prefix) => mkdtempSync(path.join(tmpdir(), prefix));
export const sha256 = (contents) =>
  createHash("sha256").update(contents).digest("hex");

export const writeExecutable = (filePath, contents) => {
  writeFileSync(filePath, contents);
  chmodSync(filePath, 0o755);
};

export const runUninstaller = ({
  os,
  args = [],
  canonicalDataDir = null,
  daemonInitiallyRunning = true,
  daemonRemovesState = true,
  daemonResult = null,
  daemonStatus = 0,
  env = {},
  machineArch = "x86_64",
  nativeStatus = 0,
  nativeVersion = "0.26.0",
  paths = {},
  supportsProLifecycle = true,
  markerPatch = {},
  mutateAfterOwnership = null,
  prepareOwnedArtifacts = null,
}) => {
  const sandboxDir = makeTempDir("ctx-uninstall-script-");
  const stubDir = path.join(sandboxDir, "stubs");
  const homeDir = path.join(sandboxDir, "home");
  const scriptPath = path.join(sandboxDir, "uninstall.sh");
  const installPath =
    paths.installPath ?? path.join(homeDir, ".local", "bin", "ctx");
  const markerPath = paths.markerPath ?? `${installPath}.install.json`;
  const integrationsPath = `${installPath}.install-integrations`;
  const manPath =
    paths.manPath ??
    path.join(homeDir, ".local", "share", "man", "man1", "ctx.1");
  const secondManPath = path.join(path.dirname(manPath), "ctx-search.1");
  const dataDir = paths.dataDir ?? path.join(homeDir, ".ctx");
  const canonicalRoot = canonicalDataDir ?? dataDir;
  let boundDaemonResult;
  if (daemonResult === null) {
    boundDaemonResult = daemonUninstallResult(
      {},
      {
        requestedDataRoot: dataDir,
        canonicalDataRoot: canonicalRoot,
      },
    );
  } else {
    try {
      const parsedDaemonResult = JSON.parse(daemonResult);
      if (Object.hasOwn(parsedDaemonResult, "requested_data_root")) {
        parsedDaemonResult.requested_data_root = dataDir;
        parsedDaemonResult.canonical_data_root = canonicalRoot;
        parsedDaemonResult.quiesced_roots =
          canonicalRoot === dataDir ? [dataDir] : [canonicalRoot, dataDir];
        parsedDaemonResult.quiesced_root_count =
          parsedDaemonResult.quiesced_roots.length;
      }
      boundDaemonResult = `${JSON.stringify(parsedDaemonResult, null, 2)}\n`;
    } catch {
      boundDaemonResult = daemonResult;
    }
  }
  const nativeLog = path.join(sandboxDir, "native-args.txt");
  const executionLog = path.join(sandboxDir, "native-execution.txt");
  const nativeEnvLog = path.join(sandboxDir, "native-env.txt");
  const lifecycleLog = path.join(sandboxDir, "lifecycle-order.txt");
  const installStageLog = path.join(sandboxDir, "install-stage.jsonl");
  const daemonStatePaths = {
    coordination: path.join(dataDir, "daemon", "upgrade-handoff.json"),
    endpoint: path.join(dataDir, "daemon", "source-refresh-endpoint.json"),
    ownerLock: path.join(dataDir, "daemon", "daemon.lock"),
    supervisor: path.join(dataDir, "daemon", "supervisor.json"),
  };

  mkdirSync(stubDir);
  mkdirSync(homeDir);
  mkdirSync(path.dirname(installPath), { recursive: true });
  mkdirSync(path.dirname(manPath), { recursive: true });
  mkdirSync(dataDir, { recursive: true });
  if (daemonInitiallyRunning) {
    mkdirSync(path.dirname(daemonStatePaths.endpoint), { recursive: true });
    for (const statePath of Object.values(daemonStatePaths)) {
      writeFileSync(statePath, "owned daemon state\n");
    }
  }
  writeExecutable(
    path.join(stubDir, "uname"),
    `#!/bin/sh
set -eu
case "$1" in
  -s) printf '%s\\n' "${os}" ;;
  -m) printf '%s\\n' "${machineArch}" ;;
  *) exit 2 ;;
esac
`,
  );
  writeExecutable(
    path.join(stubDir, "curl"),
    `#!/bin/sh
set -eu
body=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --data)
      shift
      body="$1"
      ;;
  esac
  shift
done
printf '%s\\n' "$body" >> "$CTX_TEST_INSTALL_STAGE_LOG"
exit "\${CTX_TEST_INSTALL_STAGE_STATUS:-0}"
`,
  );
  const nativeBody = `#!/bin/sh
set -eu
printf '%s|%s\\n' "$0" "$*" >> "$CTX_TEST_NATIVE_EXECUTION_LOG"
if [ "$#" -eq 1 ] && [ "$1" = "--version" ]; then
  printf 'ctx %s\\n' "$CTX_TEST_NATIVE_VERSION"
  exit 0
fi
if [ "$#" -ge 3 ] && [ "$1" = "upgrade" ] &&
   [ "$2" = "--hosted-transaction" ]; then
  action="$3"
  shift 3
  transaction_install=""
  transaction_attempt=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --install-path) shift; transaction_install="$1" ;;
      --attempt-id) shift; transaction_attempt="$1" ;;
      *) exit 91 ;;
    esac
    shift
  done
  [ -n "$transaction_install" ] || exit 92
  transaction_dir="\${transaction_install%/*}"
  transaction_leaf="\${transaction_install##*/}"
  transaction_marker="$transaction_install.install.json"
  transaction_helper="$transaction_dir/.$transaction_leaf.hosted-uninstall-helper"
  transaction_journal="$transaction_dir/.$transaction_leaf.hosted-install-transaction.json"
  transaction_field() {
    case "$1" in
      attempt) field=attempt_id ;;
      install) field=install_path ;;
      binary_sha) field=binary_sha256 ;;
      marker_sha) field=marker_sha256 ;;
      ownership_sha) field=ownership_sha256 ;;
      *) field="$1" ;;
    esac
    "$CTX_TEST_NODE" -e '
      const fs = require("node:fs");
      const value = JSON.parse(fs.readFileSync(process.argv[1], "utf8"))[process.argv[2]];
      if (typeof value !== "string") process.exit(1);
      process.stdout.write(value);
    ' "$transaction_journal" "$field"
  }
  transaction_write() {
    transaction_tmp="$transaction_journal.tmp"
    {
      printf '{\\n  "schema_version": 1,\\n  "kind": "uninstall",\\n'
      printf '  "attempt_id": "%s",\\n' "$(transaction_json_escape "$transaction_attempt")"
      printf '  "install_path": "%s",\\n' "$(transaction_json_escape "$transaction_install")"
      printf '  "binary_sha256": "%s",\\n' "$transaction_binary_sha"
      printf '  "marker_sha256": "%s",\\n' "$transaction_marker_sha"
      printf '  "ownership_sha256": "%s",\\n' "$transaction_ownership_sha"
      printf '  "phase": "%s"\\n}\\n' "$1"
    } > "$transaction_tmp"
    chmod 600 "$transaction_tmp"
    mv -f "$transaction_tmp" "$transaction_journal"
  }
  transaction_fault() {
    [ "\${CTX_TEST_HOSTED_UNINSTALL_FAULT:-}" != "$1" ] || exit 97
  }
  transaction_json_escape() {
    printf '%s' "$1" | sed 's/\\\\/\\\\\\\\/g; s/"/\\\\"/g'
  }
  transaction_receipt() {
    transaction_json_install="$(transaction_json_escape "$transaction_install")"
    transaction_json_helper="$(transaction_json_escape "$transaction_helper")"
    printf '{\\n'
    printf '  "schema_version": %s,\\n' "\${CTX_TEST_HOSTED_UNINSTALL_RECEIPT_SCHEMA:-2}"
    printf '  "command": "hosted_uninstall_transaction",\\n'
    printf '  "ok": true,\\n'
    printf '  "status": "%s",\\n' "$1"
    printf '  "daemon_admission_fenced": %s,\\n' "\${CTX_TEST_HOSTED_UNINSTALL_DAEMON_FENCED:-true}"
    printf '  "attempt_id": "%s",\\n' "$transaction_attempt"
    printf '  "install_path": "%s",\\n' "$transaction_json_install"
    printf '  "helper_path": "%s",\\n' "$transaction_json_helper"
    printf '  "binary_sha256": "%s",\\n' "$transaction_binary_sha"
    printf '  "marker_sha256": "%s"\\n' "$transaction_marker_sha"
    printf '}\\n'
  }

  if [ "$action" = "uninstall-prepare" ]; then
    if [ -f "$transaction_journal" ]; then
      [ "$(transaction_field kind)" = "uninstall" ] || exit 93
      [ "$(transaction_field install)" = "$transaction_install" ] || exit 93
      case "$(transaction_field phase)" in
        prepared|helper_staged) ;;
        *) exit 96 ;;
      esac
      transaction_attempt="$(transaction_field attempt)"
      transaction_binary_sha="$(transaction_field binary_sha)"
      transaction_marker_sha="$(transaction_field marker_sha)"
      transaction_ownership_sha="$(transaction_field ownership_sha)"
      [ "$(sha256sum "$transaction_install" | awk '{print $1}')" = "$transaction_binary_sha" ] || exit 94
      [ "$(sha256sum "$transaction_marker" | awk '{print $1}')" = "$transaction_marker_sha" ] || exit 94
      [ "$(sha256sum "$CTX_TEST_INTEGRATIONS_PATH" | awk '{print $1}')" = "$transaction_ownership_sha" ] || exit 94
    else
      [ -n "$transaction_attempt" ] || exit 92
      transaction_binary_sha="$(sha256sum "$transaction_install" | awk '{print $1}')"
      transaction_marker_sha="$(sha256sum "$transaction_marker" | awk '{print $1}')"
      transaction_ownership_sha="$(sha256sum "$CTX_TEST_INTEGRATIONS_PATH" | awk '{print $1}')"
      transaction_write prepared
      transaction_fault journal_prepared
    fi
    if [ ! -f "$transaction_helper" ]; then
      cp "$transaction_install" "$transaction_helper.new"
      chmod 700 "$transaction_helper.new"
      mv -f "$transaction_helper.new" "$transaction_helper"
    fi
    transaction_write helper_staged
    transaction_fault helper_staged
    transaction_receipt prepared
    exit 0
  fi

  [ -f "$transaction_journal" ] || exit 95
  [ "$(transaction_field kind)" = "uninstall" ] || exit 93
  [ "$(transaction_field install)" = "$transaction_install" ] || exit 93
  transaction_attempt="$(transaction_field attempt)"
  transaction_binary_sha="$(transaction_field binary_sha)"
  transaction_marker_sha="$(transaction_field marker_sha)"
  transaction_ownership_sha="$(transaction_field ownership_sha)"
  if [ "$action" = "uninstall-arm" ]; then
    [ "$(sha256sum "$transaction_install" | awk '{print $1}')" = "$transaction_binary_sha" ] || exit 94
    [ "$(sha256sum "$transaction_marker" | awk '{print $1}')" = "$transaction_marker_sha" ] || exit 94
    [ "$(sha256sum "$CTX_TEST_INTEGRATIONS_PATH" | awk '{print $1}')" = "$transaction_ownership_sha" ] || exit 94
    transaction_write armed
    transaction_fault armed
    transaction_receipt armed
    exit 0
  fi
  [ "$action" = "uninstall-commit" ] || exit 91
  case "$(transaction_field phase)" in
    armed|removing_binary|binary_removed|removing_ownership|ownership_removed|removing_marker|committed) ;;
    *) exit 96 ;;
  esac
  if [ -f "$transaction_install" ]; then
    [ "$(sha256sum "$transaction_install" | awk '{print $1}')" = "$transaction_binary_sha" ] || exit 94
    transaction_write removing_binary
    transaction_fault removing_binary
    rm -f "$transaction_install"
    transaction_fault binary_removed
  fi
  transaction_write binary_removed
  transaction_fault binary_removed_recorded
  if [ -f "$CTX_TEST_INTEGRATIONS_PATH" ]; then
    [ "$(sha256sum "$CTX_TEST_INTEGRATIONS_PATH" | awk '{print $1}')" = "$transaction_ownership_sha" ] || exit 94
    transaction_write removing_ownership
    transaction_fault removing_ownership
    rm -f "$CTX_TEST_INTEGRATIONS_PATH"
    transaction_fault ownership_removed
  fi
  transaction_write ownership_removed
  transaction_fault ownership_removed_recorded
  if [ -f "$transaction_marker" ]; then
    [ "$(sha256sum "$transaction_marker" | awk '{print $1}')" = "$transaction_marker_sha" ] || exit 94
    transaction_write removing_marker
    transaction_fault removing_marker
    rm -f "$transaction_marker"
    transaction_fault marker_removed
  fi
  transaction_write committed
  transaction_fault committed
  transaction_receipt committed
  rm -f "$transaction_journal"
  exit 0
fi
if [ "$#" -eq 3 ] && [ "$1" = "pro" ] && [ "$2" = "uninstall" ] && [ "$3" = "--help" ]; then
  exit "$CTX_TEST_PRO_CAPABILITY_STATUS"
fi
if [ "$#" -eq 6 ] && [ "$1" = "--data-root" ] &&
   [ "$2" = "$CTX_DATA_ROOT" ] && [ "$3" = "daemon" ] &&
   [ "$4" = "disable" ] && [ "$5" = "--prepare-uninstall" ] &&
   [ "$6" = "--format=json" ]; then
  printf '%s\\n' daemon >> "$CTX_TEST_LIFECYCLE_LOG"
  if [ "$CTX_TEST_DAEMON_STATUS" != "0" ]; then
    exit "$CTX_TEST_DAEMON_STATUS"
  fi
  if [ "$CTX_TEST_DAEMON_REMOVE_STATE" = "1" ]; then
    rm -f "$CTX_TEST_DAEMON_ENDPOINT" "$CTX_TEST_DAEMON_OWNER_LOCK" \
      "$CTX_TEST_DAEMON_SUPERVISOR" "$CTX_TEST_DAEMON_COORDINATION"
  fi
  printf '%s\\n' "$CTX_TEST_DAEMON_RESULT"
  exit 0
fi
printf '%s\\n' pro >> "$CTX_TEST_LIFECYCLE_LOG"
for daemon_state in "$CTX_TEST_DAEMON_ENDPOINT" "$CTX_TEST_DAEMON_OWNER_LOCK" \
  "$CTX_TEST_DAEMON_SUPERVISOR" "$CTX_TEST_DAEMON_COORDINATION"; do
  [ ! -e "$daemon_state" ] || exit 96
done
printf '%s\\n' "$@" > "$CTX_TEST_NATIVE_LOG"
printf '%s\\n' "\${CTX_ANALYTICS_ENABLED-}" > "$CTX_TEST_NATIVE_ENV_LOG"
exit "$CTX_TEST_NATIVE_STATUS"
`;
  writeExecutable(installPath, nativeBody);
  const manBody = ".TH ctx 1\n";
  const secondManBody = ".TH ctx-search 1\n";
  writeFileSync(manPath, manBody);
  writeFileSync(secondManPath, secondManBody);
  const extraOwnedArtifacts =
    prepareOwnedArtifacts?.({
      homeDir,
      sandboxDir,
      sha256,
    }) ?? [];
  const integrationRecords = [
    `man\t${sha256(manBody)}\t${manPath}`,
    `man\t${sha256(secondManBody)}\t${secondManPath}`,
    ...extraOwnedArtifacts.map(
      ({ kind, digest, target }) => `${kind}\t${digest}\t${target}`,
    ),
    "",
  ].join("\n");
  const integrationBody = [
    "CTX_INSTALL_INTEGRATIONS_V1",
    `records_sha256\t${sha256(integrationRecords)}`,
    integrationRecords,
  ].join("\n");
  writeFileSync(integrationsPath, integrationBody);
  const platform =
    os === "Darwin"
      ? machineArch === "arm64"
        ? "macos-arm64"
        : "macos-x64"
      : machineArch === "aarch64" || machineArch === "arm64"
          ? "linux-aarch64"
          : "linux-x64";
  const marker = {
    schema_version: 1,
    manager: "ctx-hosted-installer",
    install_path: installPath,
    platform,
    version: nativeVersion,
    sha256: sha256(nativeBody),
    integrations_path: integrationsPath,
    integrations_sha256: sha256(integrationBody),
    ...markerPatch,
  };
  writeFileSync(markerPath, `${JSON.stringify(marker, null, 2)}\n`);
  mutateAfterOwnership?.({
    installPath,
    integrationsPath,
    manPath,
    markerPath,
    secondManPath,
  });
  const activeIntegrationsPath = existsSync(markerPath)
    ? JSON.parse(readFileSync(markerPath, "utf8")).integrations_path ?? integrationsPath
    : integrationsPath;
  writeFileSync(path.join(dataDir, "work.sqlite"), "canonical-history\n");
  writeFileSync(
    scriptPath,
    renderUninstallScript({ installAttemptId: "ia_uninstall_test" }),
  );
  chmodSync(scriptPath, 0o755);

  const childEnv = {
    ...process.env,
    // These children are authored command stubs; exercise product defaults.
    CTX_TEST_NODE: process.execPath,
    CTX_TEST_NATIVE_EXECUTION_LOG: executionLog,
    CTX_ANALYTICS_ENABLED: "",
    CTX_DAEMON_ENABLED: "",
    HOME: homeDir,
    PATH: `${stubDir}:${process.env.PATH ?? ""}`,
    CTX_TEST_NATIVE_LOG: nativeLog,
    CTX_TEST_NATIVE_ENV_LOG: nativeEnvLog,
    CTX_TEST_LIFECYCLE_LOG: lifecycleLog,
    CTX_TEST_INSTALL_STAGE_LOG: installStageLog,
    CTX_TEST_INTEGRATIONS_PATH: activeIntegrationsPath,
    CTX_TEST_DAEMON_COORDINATION: daemonStatePaths.coordination,
    CTX_TEST_DAEMON_ENDPOINT: daemonStatePaths.endpoint,
    CTX_TEST_DAEMON_OWNER_LOCK: daemonStatePaths.ownerLock,
    CTX_TEST_DAEMON_REMOVE_STATE: daemonRemovesState ? "1" : "0",
    CTX_TEST_DAEMON_RESULT: boundDaemonResult,
    CTX_TEST_DAEMON_STATUS: String(daemonStatus),
    CTX_TEST_DAEMON_SUPERVISOR: daemonStatePaths.supervisor,
    CTX_TEST_NATIVE_STATUS: String(nativeStatus),
    CTX_TEST_NATIVE_VERSION: nativeVersion,
    CTX_TEST_PRO_CAPABILITY_STATUS: supportsProLifecycle ? "0" : "2",
    CTX_UNINSTALL_INSTALL_PATH: installPath,
    CTX_UNINSTALL_MARKER_PATH: markerPath,
    CTX_MAN_DIR: path.dirname(manPath),
    CTX_DATA_ROOT: dataDir,
    ...env,
  };
  const execute = (nextArgs = args, envOverrides = {}) =>
    spawnSync("sh", [scriptPath, ...nextArgs], {
      encoding: "utf8",
      env: {
        ...childEnv,
        ...envOverrides,
      },
    });
  const result = execute();
  const helperPath = path.join(
    path.dirname(installPath),
    `.${path.basename(installPath)}.hosted-uninstall-helper`,
  );
  const transactionPath = path.join(
    path.dirname(installPath),
    `.${path.basename(installPath)}.hosted-install-transaction.json`,
  );

  return {
    ...result,
    dataDir,
    daemonStatePaths,
    helperPath,
    homeDir,
    installPath,
    integrationsPath,
    installStageLog,
    lifecycleLog,
    manPath,
    secondManPath,
    markerPath,
    nativeLog,
    executionLog,
    nativeEnvLog,
    sandboxDir,
    scriptPath,
    transactionPath,
    rerun: execute,
    cleanup() {
      rmSync(sandboxDir, { recursive: true, force: true });
    },
  };
};
