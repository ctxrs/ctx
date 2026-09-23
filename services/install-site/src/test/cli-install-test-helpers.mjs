import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { constants, createHash, generateKeyPairSync, sign } from "node:crypto";
import {
  chmodSync,
  existsSync,
  linkSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  statSync,
  symlinkSync,
  writeFileSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";
import { renderCliInstallScript } from "../cli-install-script.js";
import { renderCliInstallPowerShellScript } from "../cli-install-powershell-script.js";
import { renderUninstallScript } from "../uninstall-script.js";
import {
  INSTALL_SCRIPT_FAMILIES,
  INSTALL_STAGE_EVENT_NAME,
  INSTALL_STAGE_EVENT_VERSION,
  INSTALL_STAGE_PAYLOAD_KEYS,
  INSTALL_STAGES,
  INSTALL_STAGE_STATUS_PAIRS,
  INSTALL_STAGE_STATUSES,
} from "../install-stage-contract.js";

function sha256(text) {
  return createHash("sha256").update(text).digest("hex");
}

function findPowerShell() {
  for (const command of ["pwsh", "powershell"]) {
    const result = spawnSync(command, ["-NoProfile", "-Command", "$PSVersionTable.PSVersion.ToString()"], {
      encoding: "utf8",
    });
    if (result.status === 0) {
      return command;
    }
  }
  return null;
}

function powerShellNativeGuardBlock(body) {
  const block = body.match(
    /if \(\$null -eq \("CtxInstallerPathGuard" -as \[type\]\)\) \{[\s\S]*?\n'@\n\}/,
  )?.[0];
  assert.ok(block, "rendered PowerShell installer must contain its native path guard");
  return block;
}

function findUtilLinuxScript() {
  const result = spawnSync("script", ["--version"], { encoding: "utf8" });
  if (result.status === 0 && /util-linux/i.test(`${result.stdout}${result.stderr}`)) {
    return "script";
  }
  return null;
}

function shellQuote(value) {
  return `'${String(value).replaceAll("'", `'\"'\"'`)}'`;
}

function signMetadataBase64(metadataText, privateKey) {
  return sign("RSA-SHA256", Buffer.from(metadataText, "utf8"), {
    key: privateKey,
    padding: constants.RSA_PKCS1_PADDING,
  }).toString("base64");
}

function makeSignedMetadataFixture(metadataText) {
  const { publicKey, privateKey } = generateKeyPairSync("rsa", {
    modulusLength: 2048,
    publicExponent: 0x10001,
  });
  const publicJwk = publicKey.export({ format: "jwk" });
  const privateKeyPem = privateKey.export({ format: "pem", type: "pkcs8" });
  return {
    privateKeyPem,
    publicKeyPem: publicKey.export({ format: "pem", type: "spki" }),
    publicKeyModulusBase64Url: publicJwk.n,
    publicKeyExponentBase64Url: publicJwk.e,
    signatureBase64: signMetadataBase64(metadataText, privateKeyPem),
  };
}

const powerShellCommand = findPowerShell();
const utilLinuxScriptCommand = findUtilLinuxScript();

function writeExecutable(filePath, body) {
  writeFileSync(filePath, body, { mode: 0o755 });
}

function runRenderedCliInstaller({
  args = [],
  env = {},
  metadataChannel = "stable",
  managedPair = false,
  releaseVersion = managedPair ? "1.4.12" : "9.9.9",
  metadataChecksum = null,
  includeMetadataSignature = true,
  platform = "linux-x64",
  compressedArtifact = "valid",
  semanticConfig = null,
  daemonConfig = null,
  spacedConfigSections = false,
  rawConfig = null,
  ttyInput = null,
  precreateInstallBin = true,
  installUmask = null,
  installDirOnPath = false,
  stagingDogfood = false,
  unsetNoColor = false,
  ttyStderrRedirected = false,
  initialProfileContents = null,
  prepareInstall = null,
  installBinPath = null,
  renderInstaller = renderCliInstallScript,
} = {}) {
  const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-run-"));
  const fakeBin = path.join(sandbox, "fake-bin");
  const homeDir = path.join(sandbox, "home");
  const installerTmpRoot = path.join(sandbox, "temporary files-语义");
  const installBin = installBinPath === null
    ? path.join(sandbox, "bin")
    : path.join(homeDir, installBinPath);
  const manDir = path.join(sandbox, "man1");
  const metadataPath = path.join(sandbox, "metadata.env");
  const metadataSignaturePath = path.join(sandbox, "metadata.env.sig");
  const artifactPath = path.join(sandbox, "ctx-artifact");
  const companionArtifactPath = path.join(sandbox, "ctx-pro-artifact");
  const pairEnvelopePath = path.join(sandbox, "managed-pair-envelope.json");
  const pairMarkerSourceLogPath = path.join(sandbox, "managed-pair-marker-source.json");
  const pairInvocationPathLogPath = path.join(sandbox, "managed-pair-invocation-paths.txt");
  const compressedArtifactPath = path.join(sandbox, "ctx-artifact.gz");
  const setupArgsPath = path.join(sandbox, "setup-args.txt");
  const setupEnvPath = path.join(sandbox, "setup-env.txt");
  const supervisorEnvPath = path.join(sandbox, "supervisor-env.txt");
  const commandLogPath = path.join(sandbox, "ctx-commands.txt");
  const downloadUrlLogPath = path.join(sandbox, "download-urls.txt");
  const hostedSetupEnvPath = path.join(sandbox, "hosted-setup-env.txt");
  const runtimeRepairLogPath = path.join(sandbox, "runtime-repair.txt");
  const noDaemonSetupPath = path.join(sandbox, "no-daemon-setup");
  const semanticRoot = path.join(sandbox, "semantic-sidecar");
  const installStageLogPath = path.join(sandbox, "install-stage.jsonl");
  const redirectedReceiptPath = path.join(sandbox, "redirected-receipt.txt");
  const configPath = path.join(homeDir, ".ctx", "config.toml");
  const scriptPath = path.join(sandbox, "install.sh");
  mkdirSync(fakeBin);
  mkdirSync(homeDir);
  mkdirSync(installerTmpRoot);
  if (precreateInstallBin) {
    mkdirSync(installBin, { recursive: true });
  }
  mkdirSync(manDir);
  chmodSync(manDir, 0o755);
  if (initialProfileContents !== null) {
    writeFileSync(path.join(homeDir, ".bashrc"), initialProfileContents);
  }
  const dataRoot = path.join(homeDir, ".ctx");
  const configSections = [];
  const searchSection = spacedConfigSections ? "[ search ]" : "[search]";
  const daemonSection = spacedConfigSections ? "[ daemon ]" : "[daemon]";
  if (semanticConfig !== null) {
    configSections.push(`${searchSection}\nsemantic = ${semanticConfig ? "true" : "false"}`);
  }
  if (daemonConfig !== null) {
    configSections.push(`${daemonSection}\nenabled = ${daemonConfig ? "true" : "false"}`);
  }
  const configContents = rawConfig ?? (
    configSections.length > 0 ? `${configSections.join("\n")}\n` : ""
  );
  if (configContents) {
    mkdirSync(dataRoot);
    writeFileSync(configPath, configContents);
  }

  let artifact = `#!/bin/sh
fixture_version=9.9.9
hosted_transaction_command=0
if [ "\${1-}" = "upgrade" ] && [ "\${2-}" = "--hosted-transaction" ]; then
  hosted_transaction_command=1
fi
if [ "$hosted_transaction_command" = "1" ] &&
   [ "\${CTX_FAKE_LOG_MUTATIONS:-0}" = "1" ]; then
  printf '%s\n' "$*" >> "$CTX_FAKE_CTX_COMMAND_LOG"
fi
if [ "$hosted_transaction_command" != "1" ]; then
{
  first=1
  for arg in "$@"; do
    if [ "$first" = "1" ]; then
      first=0
    else
      printf ' '
    fi
    printf '%s' "$arg"
  done
  printf '\\n'
} >> "$CTX_FAKE_CTX_COMMAND_LOG"
printf '%s|%s|%s\n' \
  "$*" "\${CTX_HOSTED_INSTALLER_SETUP-}" "\${CTX_SEARCH_SEMANTIC-}" \
  >> "$CTX_FAKE_HOSTED_SETUP_ENV_LOG"
if [ "\${CTX_FAKE_CHILD_NOISE:-0}" = "1" ] && [ "\${1-}" != "status" ]; then
  if [ "\${1-}" != "setup" ]; then
    printf '%s\\n' "child stdout: $*"
  fi
  printf '%s\\n' "internal child error at $CTX_FAKE_CTX_COMMAND_LOG" >&2
fi
for arg in "$@"; do
  printf '%s\\n' "$arg"
done >> "$CTX_FAKE_CTX_ARGS_LOG"
printf '%s|%s|%s|%s|%s|%s|%s|%s|%s|%s\\n' \
  "\${CTX_ANALYTICS_ENABLED-}" "\${CTX_DAEMON_ENABLED-}" "\${CTX_UPGRADE_AUTO-}" \
  "\${CTX_ANALYTICS_OFF+present}" "\${CTX_DISABLE_ANALYTICS+present}" \
  "\${CTX_INSTALL_DIAGNOSTICS_OFF+present}" "\${CTX_DAEMON_OFF+present}" \
  "\${CTX_DISABLE_DAEMON+present}" "\${CTX_UPGRADE_OFF+present}" \
  "\${CTX_DISABLE_AUTO_UPGRADE+present}" >> "$CTX_FAKE_CTX_ENV_LOG"
fi
if [ "$#" = "1" ] && [ "\${1-}" = "--ctx-core-disable-managed-man-pages-v1" ]; then
  "$CTX_FAKE_NODE" -e '
    const fs = require("node:fs");
    const markerPath = process.argv[1];
    const marker = JSON.parse(fs.readFileSync(markerPath, "utf8"));
    if (Object.hasOwn(marker, "man_pages")) {
      marker.man_pages = { schema_version: 1, status: "disabled" };
      fs.writeFileSync(markerPath + ".new", JSON.stringify(marker, null, 2) + "\\n", { mode: 0o600 });
      fs.renameSync(markerPath + ".new", markerPath);
    }
  ' "$0.install.json" || exit 65
  exit 0
fi
if [ "$#" = "7" ] && [ "\${1-}" = "--ctx-core-managed-pair-apply-v1" ]; then
  [ -n "$2" ] && [ "$3" = "-" ] && [ -f "$4" ] && [ -f "$5" ] &&
    [ -f "$6" ] && [ -f "$7" ] && [ "$0" = "$5" ] || exit 64
  printf 'apply\t%s\t%s\n' "$4" "$7" >>"$CTX_FAKE_PAIR_INVOCATION_PATH_LOG"
  if [ "\${CTX_FAKE_PAIR_INSTALL_STATUS:-0}" != "0" ]; then
    printf '%s\n' "\${CTX_FAKE_PAIR_INSTALL_ERROR:-managed pair apply failed}" >&2
    exit "$CTX_FAKE_PAIR_INSTALL_STATUS"
  fi
  if [ "\${CTX_FAKE_RETAINED_14:-0}" = "1" ]; then
    # Authored control-flow fixture. Native signature/transaction validation is
    # covered by the updater owner; only the downloaded candidate may run here.
    "$CTX_FAKE_NODE" -e '
      const assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path"),crypto=require("node:crypto");
      const [root,envelope,candidate,companion,markerSource]=process.argv.slice(1);
      const retained=path.join(root,"share/ctx/.managed-pair-apply-v1");
      assert.equal(envelope,path.join(retained,"share/ctx/managed-pair-envelope.json"));
      assert.equal(companion,path.join(retained,"libexec/ctx-pro"));
      assert.equal(markerSource,path.join(retained,"bin/ctx.install.json"));
      assert.ok(fs.existsSync(path.join(root,"bin/.ctx.upgrade-install-transaction.json")));
      assert.notEqual(candidate,path.join(retained,"bin/ctx"));
      const bytes=fs.readFileSync(path.join(retained,"bin/ctx"));
      const marker=JSON.parse(fs.readFileSync(markerSource,"utf8"));
      assert.equal(marker.version,"1.4.12");
      assert.equal(marker.sha256,crypto.createHash("sha256").update(bytes).digest("hex"));
      fs.writeFileSync(path.join(root,"bin/ctx"),bytes,{mode:0o755});
      fs.copyFileSync(markerSource,path.join(root,"bin/ctx.install.json"));
      fs.rmSync(path.join(root,"bin/.ctx.upgrade-install-transaction.json"));
    ' "$2" "$4" "$5" "$6" "$7" || exit 65
    printf '%s\n' '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}'
    exit 0
  fi
  "$CTX_FAKE_NODE" -e '
    const fs = require("node:fs"), crypto = require("node:crypto");
    const marker = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const digest = crypto.createHash("sha256").update(fs.readFileSync(process.argv[2])).digest("hex");
    if (marker.managed_pair !== true || marker.version !== process.argv[3] || marker.sha256 !== digest) process.exit(65);
  ' "$7" "$5" "$fixture_version" || exit 65
  cp "$7" "$CTX_FAKE_PAIR_MARKER_SOURCE_LOG"
  # Model the kernel's retained-input precedence, including the outer candidate
  # identity error after a different retained B has safely finished.
  if [ "\${CTX_FAKE_RETAINED_B:-0}" = "1" ] && [ -f "$2/bin/.ctx.upgrade-install-transaction.json" ]; then
    "$CTX_FAKE_NODE" -e '
      const fs=require("node:fs"),path=require("node:path"),crypto=require("node:crypto");
      const [root,artifact,markerSource,companion,envelope]=process.argv.slice(1);
      const bytes=fs.readFileSync(artifact), marker=JSON.parse(fs.readFileSync(markerSource,"utf8"));
      marker.version="1.3.2"; marker.sha256=crypto.createHash("sha256").update(bytes).digest("hex");
      fs.mkdirSync(path.join(root,"libexec"),{recursive:true}); fs.mkdirSync(path.join(root,"share","ctx"),{recursive:true});
      fs.writeFileSync(path.join(root,"bin","ctx"),bytes,{mode:0o755});
      fs.writeFileSync(path.join(root,"bin","ctx.install.json"),JSON.stringify(marker,null,2)+"\\n");
      fs.copyFileSync(companion,path.join(root,"libexec","ctx-pro"));
      fs.copyFileSync(envelope,path.join(root,"share","ctx","managed-pair-envelope.json"));
      fs.writeFileSync(path.join(root,"share","ctx","managed-pair-state.json"),"{}\\n");
      fs.rmSync(path.join(root,"bin",".ctx.upgrade-install-transaction.json"));
    ' "$2" "$CTX_FAKE_BRIDGE_ARTIFACT" "$7" "$6" "$4" || exit 65
    if [ "$fixture_version" != "1.3.2" ]; then
      printf '%s\n' 'published managed pair does not match the requested signed candidate' >&2
      exit 65
    fi
    printf '%s\n' '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}'
    exit 0
  fi
  mkdir -p "$2/bin" "$2/libexec" "$2/share/ctx"
  cp "$4" "$2/share/ctx/managed-pair-envelope.json"
  chmod 0600 "$2/share/ctx/managed-pair-envelope.json"
  cp "$6" "$2/libexec/ctx-pro"
  chmod 0755 "$2/libexec/ctx-pro"
  cp "$5" "$2/bin/.ctx.managed-pair-new"
  chmod 0755 "$2/bin/.ctx.managed-pair-new"
  mv -f "$2/bin/.ctx.managed-pair-new" "$2/bin/ctx"
  cp "$7" "$2/bin/ctx.install.json"
  chmod 0600 "$2/bin/ctx.install.json"
  printf '%s\n' '{"contract":"ctx-managed-pair-state","schema_version":1}' \
    >"$2/share/ctx/managed-pair-state.json"
  chmod 0600 "$2/share/ctx/managed-pair-state.json"
  [ -z "\${CTX_FAKE_PAIR_INSTALL_ERROR:-}" ] ||
    printf '%s\n' "$CTX_FAKE_PAIR_INSTALL_ERROR" >&2
  if [ "\${CTX_FAKE_PAIR_INSTALL_RECEIPT:-valid}" = "valid" ]; then
    printf '%s\n' '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}'
  elif [ "$CTX_FAKE_PAIR_INSTALL_RECEIPT" = "warning" ]; then
    printf '%s\n' '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","warnings":["sidecar retry remains pending"]}'
  elif [ "$CTX_FAKE_PAIR_INSTALL_RECEIPT" = "extra" ]; then
    printf '%s\n' '{'
    printf '%s\n' '  "schema_version": 1,'
    printf '%s\n' '  "command": "managed_pair_apply",'
    printf '%s\n' '  "ok": true,'
    printf '%s\n' '  "status": "committed",'
    printf '%s\n' '  "transaction": {"status":"committed"}'
    printf '%s\n' '}'
  elif [ "$CTX_FAKE_PAIR_INSTALL_RECEIPT" = "canonical-extra" ]; then
    printf '%s\n' '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}'
    printf '%s\n' 'extra'
  elif [ "$CTX_FAKE_PAIR_INSTALL_RECEIPT" = "scalar-warning" ]; then
    printf '%s\n' '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","warnings":"not an array"}'
  elif [ "$CTX_FAKE_PAIR_INSTALL_RECEIPT" = "empty-warnings" ]; then
    printf '%s\n' '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","warnings":[]}'
  elif [ "$CTX_FAKE_PAIR_INSTALL_RECEIPT" = "too-many-warnings" ]; then
    printf '%s\n' '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","warnings":["one","two","three","four","five"]}'
  elif [ "$CTX_FAKE_PAIR_INSTALL_RECEIPT" = "unsafe-warning" ]; then
    printf '%s\n' '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","warnings":["unsafe!"]}'
  elif [ "$CTX_FAKE_PAIR_INSTALL_RECEIPT" = "oversized" ]; then
    printf '%s' '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","warnings":["'
    printf '%0600d' 0
    printf '%s\n' '"]}'
  else
    printf '%s\n' '{"schema_version":1,"status":"unknown"}'
  fi
  exit 0
fi
if [ "$#" = "4" ] && [ "\${1-}" = "--ctx-core-managed-pair-reconcile-integration-v1" ]; then
  printf 'reconcile\t%s\n' "$4" >>"$CTX_FAKE_PAIR_INVOCATION_PATH_LOG"
  if [ "\${CTX_FAKE_PAIR_RECONCILE_STATUS:-0}" != "0" ]; then
    printf '%s\n' "\${CTX_FAKE_PAIR_RECONCILE_ERROR:-managed pair reconciliation failed}" >&2
    exit "$CTX_FAKE_PAIR_RECONCILE_STATUS"
  fi
  reconcile_digest="$(sha256sum "$4" | awk '{ print $1 }')"
  reconcile_generation="$2/bin/ctx.install-integrations.$reconcile_digest"
  cp "$4" "$reconcile_generation"
  chmod 0600 "$reconcile_generation"
  "$CTX_FAKE_NODE" -e '
    const fs = require("node:fs");
    const [markerPath, generation, digest] = process.argv.slice(1);
    const marker = JSON.parse(fs.readFileSync(markerPath, "utf8"));
    const previous = marker.integrations_path;
    marker.integrations_path = generation;
    marker.integrations_sha256 = digest;
    fs.writeFileSync(markerPath + ".new", JSON.stringify(marker, null, 2) + "\\n", { mode: 0o600 });
    fs.renameSync(markerPath + ".new", markerPath);
    if (previous && previous !== generation) fs.rmSync(previous, { force: true });
  ' "$2/bin/ctx.install.json" "$reconcile_generation" "$reconcile_digest" || exit 65
  if [ "\${CTX_FAKE_PAIR_RECONCILE_RECEIPT:-valid}" = "warning" ]; then
    printf '%s\n' '{"schema_version":1,"command":"managed_pair_reconcile_integration","ok":true,"status":"committed","warnings":["reconciliation cleanup remains pending"]}'
  else
    printf '%s\n' '{"schema_version":1,"command":"managed_pair_reconcile_integration","ok":true,"status":"committed"}'
  fi
  exit 0
fi
if [ "$#" = "1" ] && [ "\${1-}" = "--version" ]; then
  printf '%s\\n' "ctx 9.9.9"
  exit 0
fi
if [ "$#" = "3" ] && [ "\${1-}" = "pro" ] &&
   [ "\${2-}" = "uninstall" ] && [ "\${3-}" = "--help" ]; then
  exit 0
fi
if [ "$#" = "6" ] && [ "\${1-}" = "--data-root" ] &&
   [ "\${2-}" = "$CTX_DATA_ROOT" ] && [ "\${3-}" = "daemon" ] &&
   [ "\${4-}" = "disable" ] && [ "\${5-}" = "--prepare-uninstall" ] &&
   [ "\${6-}" = "--format=json" ]; then
  json_requested_root="$(printf '%s' "$CTX_DATA_ROOT" | sed 's/\\\\/\\\\\\\\/g; s/"/\\\\"/g')"
  json_canonical_root="$(printf '%s' "$HOME/.ctx" | sed 's/\\\\/\\\\\\\\/g; s/"/\\\\"/g')"
  if [ "$CTX_DATA_ROOT" = "$HOME/.ctx" ]; then
    quiesced_roots="    \\"$json_requested_root\\""
    quiesced_root_count=1
  else
    quiesced_roots="$(printf '    \\"%s\\",\\n    \\"%s\\"' "$json_requested_root" "$json_canonical_root")"
    quiesced_root_count=2
  fi
  cat <<EOF
{
  "schema_version": 1,
  "command": "daemon_prepare_uninstall",
  "ok": true,
  "scope": "installation",
  "requested_data_root": "$json_requested_root",
  "canonical_data_root": "$json_canonical_root",
  "quiesced_roots": [
$quiesced_roots
  ],
  "quiesced_root_count": $quiesced_root_count,
  "installation_quiescent": true,
  "daemon_enabled": false,
  "daemon_running": false,
  "owner_lock_released": true,
  "endpoint_released": true,
  "supervisor_removed": true,
  "coordination_state_removed": true,
  "binary_retained": true,
  "retry_safe": true,
  "local_only": true
}
EOF
  exit 0
fi
if [ "\${1-}" = "upgrade" ] && [ "\${2-}" = "--hosted-transaction" ] &&
   { [ "\${3-}" = "install" ] || [ "\${3-}" = "migrate" ]; }; then
  shift 3
  hosted_install_path=
  hosted_attempt_id=
  hosted_marker_source=
  hosted_ownership_source=
  hosted_binary_sha256=
  while [ "$#" -gt 0 ]; do
    hosted_name="$1"
    shift
    [ "$#" -gt 0 ] || exit 64
    hosted_value="$1"
    shift
    case "$hosted_name" in
      --install-path) hosted_install_path="$hosted_value" ;;
      --attempt-id) hosted_attempt_id="$hosted_value" ;;
      --marker-source) hosted_marker_source="$hosted_value" ;;
      --ownership-source) hosted_ownership_source="$hosted_value" ;;
      --binary-sha256) hosted_binary_sha256="$hosted_value" ;;
      *) exit 64 ;;
    esac
  done
  [ -n "$hosted_install_path" ] && [ -n "$hosted_attempt_id" ] &&
    [ -f "$hosted_marker_source" ] && [ -n "$hosted_binary_sha256" ] || exit 64
  hosted_binary_tmp="$hosted_install_path.hosted-new"
  cp "$0" "$hosted_binary_tmp"
  chmod 0755 "$hosted_binary_tmp"
  mv -f "$hosted_binary_tmp" "$hosted_install_path"
  hosted_journal="$(dirname "$hosted_install_path")/.$(basename "$hosted_install_path").hosted-install-transaction.json"
  hosted_pending_marker="$hosted_install_path.hosted-pending-marker"
  hosted_pending_ownership="$hosted_install_path.hosted-pending-ownership"
  if [ "$CTX_FAKE_HOSTED_PARTIAL_FAILURE" = "1" ]; then
    cp "$hosted_marker_source" "$hosted_pending_marker"
    cp "$hosted_ownership_source" "$hosted_pending_ownership"
    printf '%s\n' 'fixture binary_replaced' >"$hosted_journal"
    chmod 0600 "$hosted_journal"
    exit 75
  fi
  if [ -f "$hosted_journal" ]; then
    hosted_marker_source="$hosted_pending_marker"
    hosted_ownership_source="$hosted_pending_ownership"
  fi
  if [ -n "$hosted_ownership_source" ]; then
    cp "$hosted_ownership_source" "$hosted_install_path.install-integrations"
    chmod 0600 "$hosted_install_path.install-integrations"
  fi
  cp "$hosted_marker_source" "$hosted_install_path.install.json"
  chmod 0600 "$hosted_install_path.install.json"
  if [ -f "$hosted_journal" ]; then
    rm "$hosted_journal" "$hosted_pending_marker" "$hosted_pending_ownership"
  fi
  hosted_marker_sha256="$(sha256sum "$hosted_install_path.install.json" | awk '{ print $1 }')"
  json_hosted_path="$(printf '%s' "$hosted_install_path" | sed 's/\\\\/\\\\\\\\/g; s/"/\\\\"/g')"
  printf '{\\n'
  printf '  "schema_version": 1,\\n'
  printf '  "command": "hosted_install_transaction",\\n'
  printf '  "ok": true,\\n'
  printf '  "status": "committed",\\n'
  printf '  "attempt_id": "%s",\\n' "$hosted_attempt_id"
  printf '  "install_path": "%s",\\n' "$json_hosted_path"
  printf '  "binary_sha256": "%s",\\n' "$hosted_binary_sha256"
  printf '  "marker_sha256": "%s"\\n' "$hosted_marker_sha256"
  printf '}\\n'
  exit 0
fi
if [ "\${1-}" = "upgrade" ] && [ "\${2-}" = "--hosted-transaction" ]; then
  hosted_action="$3"
  shift 3
  hosted_install_path=
  hosted_attempt_id=
  while [ "$#" -gt 0 ]; do
    hosted_name="$1"
    shift
    [ "$#" -gt 0 ] || exit 64
    hosted_value="$1"
    shift
    case "$hosted_name" in
      --install-path) hosted_install_path="$hosted_value" ;;
      --attempt-id) hosted_attempt_id="$hosted_value" ;;
      *) exit 64 ;;
    esac
  done
  hosted_dir="\${hosted_install_path%/*}"
  hosted_leaf="\${hosted_install_path##*/}"
  hosted_marker="$hosted_install_path.install.json"
  hosted_helper="$hosted_dir/.$hosted_leaf.hosted-uninstall-helper"
  hosted_journal="$hosted_dir/.$hosted_leaf.hosted-install-transaction.json"
  hosted_field() { sed -n "s/^$1=//p" "$hosted_journal"; }
  hosted_write() {
    {
      printf 'attempt=%s\\n' "$hosted_attempt_id"
      printf 'binary_sha=%s\\n' "$hosted_binary_sha256"
      printf 'marker_sha=%s\\n' "$hosted_marker_sha256"
      printf 'phase=%s\\n' "$1"
    } >"$hosted_journal.tmp"
    chmod 0600 "$hosted_journal.tmp"
    mv -f "$hosted_journal.tmp" "$hosted_journal"
  }
  hosted_receipt() {
    hosted_json_path="$(printf '%s' "$hosted_install_path" | sed 's/\\\\/\\\\\\\\/g; s/"/\\\\"/g')"
    hosted_json_helper="$(printf '%s' "$hosted_helper" | sed 's/\\\\/\\\\\\\\/g; s/"/\\\\"/g')"
    printf '{\\n'
    printf '  "schema_version": 2,\\n'
    printf '  "command": "hosted_uninstall_transaction",\\n'
    printf '  "ok": true,\\n'
    printf '  "status": "%s",\\n' "$1"
    printf '  "daemon_admission_fenced": true,\\n'
    printf '  "attempt_id": "%s",\\n' "$hosted_attempt_id"
    printf '  "install_path": "%s",\\n' "$hosted_json_path"
    printf '  "helper_path": "%s",\\n' "$hosted_json_helper"
    printf '  "binary_sha256": "%s",\\n' "$hosted_binary_sha256"
    printf '  "marker_sha256": "%s"\\n' "$hosted_marker_sha256"
    printf '}\\n'
  }
  if [ "$hosted_action" = "uninstall-prepare" ]; then
    hosted_binary_sha256="$(sha256sum "$hosted_install_path" | awk '{print $1}')"
    hosted_marker_sha256="$(sha256sum "$hosted_marker" | awk '{print $1}')"
    hosted_write prepared
    cp "$hosted_install_path" "$hosted_helper.new"
    chmod 0700 "$hosted_helper.new"
    mv -f "$hosted_helper.new" "$hosted_helper"
    hosted_write helper_staged
    hosted_receipt prepared
    exit 0
  fi
  [ -f "$hosted_journal" ] || exit 65
  hosted_attempt_id="$(hosted_field attempt)"
  hosted_binary_sha256="$(hosted_field binary_sha)"
  hosted_marker_sha256="$(hosted_field marker_sha)"
  if [ "$hosted_action" = "uninstall-arm" ]; then
    hosted_write armed
    hosted_receipt armed
    exit 0
  fi
  [ "$hosted_action" = "uninstall-commit" ] || exit 64
  [ -z "\${CTX_FAKE_ACTIVE_INTEGRATIONS_PATH-}" ] ||
    rm -f "$CTX_FAKE_ACTIVE_INTEGRATIONS_PATH"
  rm -f "$hosted_install_path"
  hosted_write binary_removed
  rm -f "$hosted_marker"
  hosted_write committed
  hosted_receipt committed
  rm -f "$hosted_journal"
  exit 0
fi
semantic_upgrade_command=0
if [ "\${1-}" = "upgrade" ] && [ "\${CTX_SEARCH_SEMANTIC-}" = "1" ]; then
  case "\${CTX_RELEASE_METADATA_URL-}" in
    file://*) semantic_upgrade_command=1 ;;
  esac
fi
if [ "$semantic_upgrade_command" = "1" ]; then
  if [ "\${CTX_FAKE_SEMANTIC_REQUIRES_PAIR:-0}" = "1" ]; then
    [ -f "$(dirname "$0")/../libexec/ctx-pro" ] || exit 78
  fi
  [ "$#" = "4" ] && [ "\${2-}" = "--channel" ] &&
    [ -n "\${3-}" ] && [ "\${4-}" = "--format=json" ] || exit 64
  printf '%s|%s|%s|%s|%s\\n' \
    "\${1-}" "\${2-}" "\${3-}" \
    "\${CTX_RELEASE_METADATA_URL-}" \
    "\${CTX_RELEASE_METADATA_SIGNATURE_URL-}" >"$CTX_FAKE_RUNTIME_REPAIR_LOG"
  if [ "\${CTX_FAKE_RUNTIME_REPAIR_STATUS:-0}" != "0" ]; then
    exit "$CTX_FAKE_RUNTIME_REPAIR_STATUS"
  fi
  mkdir -p "$CTX_FAKE_SEMANTIC_ROOT"
  printf '%s\\n' model >"$CTX_FAKE_SEMANTIC_ROOT/model.installed"
  printf '%s\\n' runtime >"$CTX_FAKE_SEMANTIC_ROOT/runtime.installed"
fi
if [ "\${1-}" = "upgrade" ] && [ "\${4-}" = "--format=json" ]; then
  if [ "$fixture_version" = "1.3.2" ] && [ "\${CTX_FAKE_FAIL_AT_B:-0}" = "1" ]; then exit 71; fi
  if [ "\${CTX_FAKE_MANAGED_UPGRADE_STATUS:-0}" != "0" ]; then
    printf '%s\\n' "\${CTX_FAKE_MANAGED_UPGRADE_ERROR:-}" >&2
    exit "$CTX_FAKE_MANAGED_UPGRADE_STATUS"
  fi
  if [ "\${CTX_FAKE_MANAGED_UPGRADE_RESULT+x}" = "x" ]; then
    printf '%s\\n' "$CTX_FAKE_MANAGED_UPGRADE_RESULT"
    exit 0
  fi
  fixture_metadata="$CTX_FAKE_METADATA"
  fixture_artifact="$CTX_FAKE_ARTIFACT"
  if [ "\${CTX_FAKE_ENFORCE_OLD_CAP:-0}" = "1" ] &&
     [ "$fixture_version" = "1.6.3" ] &&
     [ "$(wc -c < "$fixture_artifact")" -gt 134217728 ]; then
    printf '%s\n' 'released updater refuses executable above 128 MiB' >&2
    exit 74
  fi
  fixture_feed="https://cli.ctx.rs/functions/v2/releases/stable/ctx-release-metadata.env"
  case "$fixture_version" in
    0.*|1.0.*|1.1.*|1.2.*|1.3.0|1.3.1)
      fixture_metadata="$CTX_FAKE_BRIDGE_METADATA"
      fixture_artifact="$CTX_FAKE_BRIDGE_ARTIFACT"
      fixture_feed="https://cli.ctx.rs/functions/v1/releases/stable/ctx-release-metadata.env" ;;
  esac
  fixture_target_version="$(sed -n 's/^CTX_RELEASE_VERSION=//p' "$fixture_metadata")"
  fixture_pair="$(sed -n 's/^CTX_RELEASE_MANAGED_PAIR_ENVELOPE_linux_x64=//p' "$fixture_metadata")"
  fixture_status="$("$CTX_FAKE_NODE" -e '
    const fs=require("node:fs"), path=require("node:path"), crypto=require("node:crypto");
    const [install, artifact, version, pair, companion, envelope, sourceVersion] = process.argv.slice(1);
    const markerPath=install+".install.json";
    const marker=JSON.parse(fs.readFileSync(markerPath,"utf8"));
    const bytes=fs.readFileSync(artifact);
    const digest=crypto.createHash("sha256").update(bytes).digest("hex");
    // Old fixtures model Core/runtime-only publication; B/current model the
    // accepted native same-owner first-pair repair, not candidate invocation.
    const supportsPair = Number(sourceVersion.split(".")[0]) <= 1 && !(Number(sourceVersion.split(".")[0]) === 1 && Number(sourceVersion.split(".")[1]) >= 5) && !/^(0\\.|1\\.[012]\\.|1\\.3\\.[01]$)/.test(sourceVersion);
    if (marker.sha256 !== digest || (pair && supportsPair && marker.managed_pair !== true)) {
      fs.writeFileSync(install+".fixture-new",bytes,{mode:0o755});
      fs.renameSync(install+".fixture-new",install);
      marker.version=version; marker.sha256=digest;
      if(pair && supportsPair) {
        marker.managed_pair=true;
        const root=path.dirname(path.dirname(install));
        fs.mkdirSync(path.join(root,"libexec"),{recursive:true});
        fs.mkdirSync(path.join(root,"share","ctx"),{recursive:true});
        fs.copyFileSync(companion,path.join(root,"libexec","ctx-pro"));
        fs.copyFileSync(envelope,path.join(root,"share","ctx","managed-pair-envelope.json"));
        fs.writeFileSync(path.join(root,"share","ctx","managed-pair-state.json"),"{}\\n");
      }
      fs.writeFileSync(markerPath,JSON.stringify(marker,null,2)+"\\n");
      console.log("applied");
    } else { console.log("up_to_date"); }
  ' "$0" "$fixture_artifact" "$fixture_target_version" "$fixture_pair" "$CTX_FAKE_COMPANION_ARTIFACT" "$CTX_FAKE_PAIR_ENVELOPE" "$fixture_version")" || exit 65
  fixture_applied=false
  fixture_attempt=null
  if [ "$fixture_status" = "applied" ]; then fixture_applied=true; fixture_attempt='"ua_fixture"'; fi
  json_install_path="$(printf '%s' "$0" | sed 's/\\\\/\\\\\\\\/g; s/"/\\\\"/g')"
  cat <<EOF
{
  "schema_version": 1,
  "command": "upgrade",
  "ok": true,
  "status": "$fixture_status",
  "message": "fixture signed feed operation complete",
  "current_version": "$fixture_target_version",
  "latest_version": "$fixture_target_version",
  "update_available": false,
  "update_was_available": false,
  "channel": "stable",
  "platform": "$CTX_PLATFORM",
  "metadata_url": "$fixture_feed",
  "artifact_url": "https://example.test/releases/ctx-linux-x64",
  "install_path": "$json_install_path",
  "managed": true,
  "applied": $fixture_applied,
  "dry_run": false,
  "warnings": [],
  "upgrade_attempt_id": $fixture_attempt
}
EOF
  exit 0
fi
if [ "\${1-}" = "docs" ] && [ "\${2-}" = "man" ] && [ "\${3-}" = "--out" ]; then
  mkdir -p "$4"
  printf '%s%s\\n' '.TH ctx 1' "\${CTX_FAKE_MAN_PAGE_SUFFIX-}" >"$4/ctx.1"
  printf '%s%s\\n' '.TH ctx-search 1' "\${CTX_FAKE_MAN_PAGE_SUFFIX-}" >"$4/ctx-search.1"
  exit 0
fi
if [ "\${1-}" = "integrations" ] && [ "\${2-}" = "install" ] && [ "\${3-}" = "skills" ]; then
  skill_format_json=0
  for arg in "$@"; do
    case "$arg" in
      --format=json) skill_format_json=1 ;;
      --json) exit 64 ;;
    esac
  done
  [ "$skill_format_json" = "1" ] || exit 64
  skill_path="\${CTX_FAKE_SKILL_PATH:-$HOME/.agents/skills/ctx-agent-history-search}"
  mkdir -p "$skill_path"
  if [ "\${CTX_FAKE_SKILL_PRESERVE_EXISTING:-0}" != "1" ] ||
     [ ! -f "$skill_path/SKILL.md" ]; then
    printf '%s\\n' '# ctx test skill' >"$skill_path/SKILL.md"
    skill_hash="$(sha256sum "$skill_path/SKILL.md" | awk '{ print $1 }')"
    cat >"$skill_path/.ctx-skill.json" <<EOF
{
  "schema_version": 1,
  "installer": "ctx-cli",
  "skill_name": "ctx-agent-history-search",
  "skill_hash": "sha256:$skill_hash"
}
EOF
  fi
  json_skill_path="$(printf '%s' "$skill_path" | sed 's/\\\\/\\\\\\\\/g; s/"/\\\\"/g')"
  printf '{"skill":"ctx-agent-history-search","results":[{"path":"%s","success":true}]}\\n' "$json_skill_path"
  exit 0
fi
if [ "\${1-}" = "setup" ] && [ "\${2-}" = "--semantic" ]; then
  [ -f "$CTX_FAKE_SEMANTIC_ROOT/model.installed" ] || exit 78
  [ -f "$CTX_FAKE_SEMANTIC_ROOT/runtime.installed" ] || exit 79
fi
if [ "\${1-}" = "setup" ]; then
  fake_setup_wait=0
  case " $* " in *" --wait "*) fake_setup_wait=1 ;; esac
  fake_setup_progress=
  fake_previous_setup_arg=
  for fake_setup_arg in "$@"; do
    if [ "$fake_previous_setup_arg" = "--progress" ]; then
      fake_setup_progress="$fake_setup_arg"
      break
    fi
    fake_previous_setup_arg="$fake_setup_arg"
  done
  fake_human_progress=0
  case "$fake_setup_progress" in
    auto|plain) fake_human_progress=1 ;;
  esac
  if [ "$fake_setup_wait" = "1" ] && [ "$fake_human_progress" = "1" ] &&
     [ "\${CTX_FAKE_SETUP_PROGRESS_FRAME:-0}" = "1" ] && [ -t 2 ]; then
    printf '\\033[36mIndexing local agent history: 50%%\\033[0m\\n' >&2
  fi
  if [ "$fake_setup_wait" = "1" ] && [ "$fake_human_progress" = "1" ] &&
     [ "\${CTX_FAKE_SETUP_PROGRESS_STREAM:-0}" = "1" ] && [ -t 2 ]; then
    for progress_percent in 50 75 100; do
      printf '\\r\\033[2K\\033[36mIndexing local agent history: %s%%\\033[0m' "$progress_percent" >&2
      sleep 0.05
    done
    printf '\\r\\033[2K\\n' >&2
  fi
  if [ "\${CTX_FAKE_PERSIST_HOSTED_SEMANTIC:-0}" = "1" ] &&
     [ "\${CTX_HOSTED_INSTALLER_SETUP:-0}" = "1" ]; then
    persisted_semantic=
    case "\${CTX_SEARCH_SEMANTIC-}" in
      1|true|TRUE|yes|YES|on|ON) persisted_semantic=true ;;
      0|false|FALSE|no|NO|off|OFF) persisted_semantic=false ;;
    esac
    if [ -z "$persisted_semantic" ]; then
      case " $* " in
        *" --semantic "*) persisted_semantic=true ;;
      esac
    fi
    if [ -n "$persisted_semantic" ]; then
      mkdir -p "$CTX_DATA_ROOT"
      printf '[search]\nsemantic = %s\n' "$persisted_semantic" >"$CTX_DATA_ROOT/config.toml"
    fi
  fi
  {
    printf '%s\\n' "ASTRBOT_ROOT=\${ASTRBOT_ROOT-}"
    printf '%s\\n' "CLAUDE_CONFIG_DIR=\${CLAUDE_CONFIG_DIR-}"
    printf '%s\\n' "CODEX_HOME=\${CODEX_HOME-}"
    printf '%s\\n' "COPILOT_HOME=\${COPILOT_HOME-}"
    printf '%s\\n' "CTX_ANALYTICS_ENABLED=\${CTX_ANALYTICS_ENABLED-}"
    printf '%s\\n' "CTX_UPGRADE_AUTO=\${CTX_UPGRADE_AUTO-}"
    printf '%s\\n' "CTX_UPGRADE_CHANNEL=\${CTX_UPGRADE_CHANNEL-}"
    printf '%s\\n' "CTX_UPGRADE_INTERVAL_SECONDS=\${CTX_UPGRADE_INTERVAL_SECONDS-}"
    printf '%s\\n' "FORGE_CONFIG=\${FORGE_CONFIG-}"
    printf '%s\\n' "HERMES_HOME=\${HERMES_HOME-}"
    printf '%s\\n' "HTTPS_PROXY=\${HTTPS_PROXY-}"
    printf '%s\\n' "HTTP_PROXY=\${HTTP_PROXY-}"
    printf '%s\\n' "MIMOCODE_CONFIG_DIR=\${MIMOCODE_CONFIG_DIR-}"
    printf '%s\\n' "NO_PROXY=\${NO_PROXY-}"
    printf '%s\\n' "SSL_CERT_DIR=\${SSL_CERT_DIR-}"
    printf '%s\\n' "SSL_CERT_FILE=\${SSL_CERT_FILE-}"
    printf '%s\\n' "XDG_CONFIG_HOME=\${XDG_CONFIG_HOME-}"
  } >"$CTX_FAKE_SUPERVISOR_ENV_LOG"
  case " $* " in
    *" --no-daemon "*) : >"$CTX_FAKE_NO_DAEMON_SETUP" ;;
  esac
  case " $* " in
    *" --format json "*) ;;
    *)
      printf '%s\n' "\${CTX_FAKE_NATIVE_SETUP_OUTPUT:-ctx setup complete}"
      exit "\${CTX_FAKE_SETUP_STATUS:-0}"
      ;;
  esac
  if [ "\${CTX_FAKE_SETUP_RECEIPT+x}" = "x" ]; then
    printf '%s\\n' "$CTX_FAKE_SETUP_RECEIPT"
  else
    setup_initialized="\${CTX_FAKE_INITIALIZED:-true}"
    setup_mode="\${CTX_FAKE_SETUP_MODE:-ready}"
    indexed_sessions="\${CTX_FAKE_INDEXED_SESSIONS:-3800}"
    indexed_items="\${CTX_FAKE_INDEXED_ITEMS:-38000}"
    if [ -f "$CTX_FAKE_NO_DAEMON_SETUP" ] ||
       grep -Eq '^[[:space:]]*enabled[[:space:]]*=[[:space:]]*false' \
         "$CTX_DATA_ROOT/config.toml" 2>/dev/null; then
      [ "\${CTX_FAKE_INITIALIZED+x}" = "x" ] || setup_initialized=false
      [ "\${CTX_FAKE_SETUP_MODE+x}" = "x" ] || setup_mode=unavailable
      [ "\${CTX_FAKE_INDEXED_SESSIONS+x}" = "x" ] || indexed_sessions=null
      [ "\${CTX_FAKE_INDEXED_ITEMS+x}" = "x" ] || indexed_items=null
    fi
    cat <<EOF
{
  "schema_version": \${CTX_FAKE_SCHEMA_VERSION:-3},
  "initialized": $setup_initialized,
  "mode": "$setup_mode",
EOF
    if [ "$indexed_sessions" != "null" ]; then
      printf '  "indexed_sessions": %s,\\n' "$indexed_sessions"
    fi
    if [ "$indexed_items" != "null" ]; then
      printf '  "indexed_items": %s,\\n' "$indexed_items"
    fi
    printf '  "refresh_pending": false\\n'
    printf '}\\n'
  fi
  exit "\${CTX_FAKE_SETUP_STATUS:-0}"
fi
if [ "\${1-}" = "pro" ]; then
  exit 2
fi
`;
  artifact = artifact.replaceAll("9.9.9", releaseVersion);
  const bridgeArtifactPath = path.join(sandbox, "ctx-bridge-artifact");
  const bridgeArtifact = artifact.replaceAll(releaseVersion, "1.3.2");
  writeExecutable(bridgeArtifactPath, bridgeArtifact);
  writeExecutable(artifactPath, artifact);
  writeExecutable(companionArtifactPath, "#!/bin/sh\nexit 0\n");
  writeFileSync(pairEnvelopePath, '{"signed":"test-envelope"}\n');
  writeFileSync(
    compressedArtifactPath,
    compressedArtifact === "corrupt" ? "not a gzip stream" : gzipSync(Buffer.from(artifact), { mtime: 0 }),
  );
  const pairMetadata = managedPair ? `CTX_RELEASE_MANAGED_PAIR_ENVELOPE_linux_x64=managed-pair-envelope.json
CTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_linux_x64=sha256/${sha256(artifact)}/ctx-linux-x64
CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_linux_x64=${sha256(artifact)}
CTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_linux_x64=sha256/${sha256(readFileSync(companionArtifactPath))}/ctx-pro-linux-x64
CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_linux_x64=${sha256(readFileSync(companionArtifactPath))}
` : "";
  const metadataText = `CTX_RELEASE_SCHEMA_VERSION=1
CTX_RELEASE_CHANNEL=${metadataChannel}
CTX_RELEASE_VERSION=${releaseVersion}
CTX_RELEASE_BASE_URL=https://example.test/releases
CTX_RELEASE_ARTIFACT_linux_x64=ctx-linux-x64
CTX_RELEASE_SHA256_linux_x64=${metadataChecksum ?? sha256(artifact)}
CTX_RELEASE_ARTIFACT_linux_aarch64=ctx-linux-aarch64
CTX_RELEASE_SHA256_linux_aarch64=${metadataChecksum ?? sha256(artifact)}
${pairMetadata}CTX_RELEASE_SOURCE_COMMIT=abc123
CTX_RELEASE_PUBLISHED_AT=2026-06-30T00:00:00Z
`;
  const signedMetadata = makeSignedMetadataFixture(metadataText);
  const bridgeMetadataPath = path.join(sandbox, "bridge-metadata.env");
  const bridgeMetadataSignaturePath = bridgeMetadataPath + ".sig";
  const bridgeMetadata = metadataText
    .replace(`CTX_RELEASE_VERSION=${releaseVersion}`, "CTX_RELEASE_VERSION=1.3.2")
    .replaceAll(sha256(artifact), sha256(bridgeArtifact))
    .replace("CTX_RELEASE_BASE_URL=https://example.test/releases", "CTX_RELEASE_BASE_URL=https://example.test/bridge");
  writeFileSync(bridgeMetadataPath, bridgeMetadata);
  writeFileSync(bridgeMetadataSignaturePath, signMetadataBase64(bridgeMetadata, signedMetadata.privateKeyPem) + "\n");
  writeFileSync(metadataPath, metadataText);
  if (includeMetadataSignature) {
    writeFileSync(metadataSignaturePath, `${signedMetadata.signatureBase64}\n`);
  }
  writeExecutable(
    path.join(fakeBin, "curl"),
    `#!/bin/sh
dest=""
url=""
body=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o)
      shift
      dest="$1"
      ;;
    --data|--data-binary)
      shift
      body="$1"
      ;;
    https://*)
      url="$1"
      ;;
  esac
  shift
done
printf '%s\\n' "$url" >> "$CTX_FAKE_DOWNLOAD_URL_LOG"
case "$url" in
  */install-attempt)
    printf '%s\\n' "$body" >> "$CTX_FAKE_INSTALL_STAGE_LOG"
    exit "\${CTX_FAKE_INSTALL_STAGE_STATUS:-0}"
    ;;
  */releases/stable/1.3.2/ctx-release-metadata.env)
    cp "$CTX_FAKE_BRIDGE_METADATA" "$dest" ;;
  */releases/stable/1.3.2/ctx-release-metadata.env.sig)
    cp "$CTX_FAKE_BRIDGE_METADATA_SIGNATURE" "$dest" ;;
  */bridge/*/ctx-linux-x64.gz|*/bridge/ctx-linux-x64.gz)
    exit 22 ;;
  */bridge/*/ctx-linux-x64|*/bridge/ctx-linux-x64)
    cp "$CTX_FAKE_BRIDGE_ARTIFACT" "$dest" ;;
  *metadata.env)
    cp "$CTX_FAKE_METADATA" "$dest"
    ;;
  *metadata.env.sig)
    cp "$CTX_FAKE_METADATA_SIGNATURE" "$dest"
    ;;
  *managed-pair-envelope.json)
    cp "$CTX_FAKE_PAIR_ENVELOPE" "$dest"
    ;;
  *ctx-pro-linux-x64.gz)
    exit 22
    ;;
  *ctx-pro-linux-x64)
    cp "$CTX_FAKE_COMPANION_ARTIFACT" "$dest"
    ;;
  *ctx-linux-x64.gz|*ctx-linux-aarch64.gz)
    if [ "\${CTX_FAKE_GZIP_PROBE_NOISE:-0}" = "1" ]; then
      printf '%s\\n' "optional gzip artifact is unavailable" >&2
    fi
    [ "$CTX_FAKE_COMPRESSED_ARTIFACT" != "missing" ] || exit 22
    if [ -n "\${CTX_FAKE_ARTIFACT_DOWNLOAD_DELAY_SECONDS:-}" ]; then
      sleep "$CTX_FAKE_ARTIFACT_DOWNLOAD_DELAY_SECONDS"
    fi
    cp "$CTX_FAKE_COMPRESSED_ARTIFACT" "$dest"
    ;;
  *ctx-linux-x64)
    if [ -n "\${CTX_FAKE_ARTIFACT_DOWNLOAD_DELAY_SECONDS:-}" ]; then
      sleep "$CTX_FAKE_ARTIFACT_DOWNLOAD_DELAY_SECONDS"
    fi
    cp "$CTX_FAKE_ARTIFACT" "$dest"
    ;;
  *ctx-linux-aarch64)
    if [ -n "\${CTX_FAKE_ARTIFACT_DOWNLOAD_DELAY_SECONDS:-}" ]; then
      sleep "$CTX_FAKE_ARTIFACT_DOWNLOAD_DELAY_SECONDS"
    fi
    cp "$CTX_FAKE_ARTIFACT" "$dest"
    ;;
  *)
    echo "unexpected URL: $url" >&2
    exit 44
    ;;
esac
`,
  );
  writeFileSync(scriptPath, renderInstaller({
    metadataPublicKeyPem: signedMetadata.publicKeyPem,
    stagingDogfood,
  }));

  const childEnv = {
    ...process.env,
    // These children are authored command stubs; exercise product defaults.
    CTX_ANALYTICS_ENABLED: "",
    CTX_DAEMON_ENABLED: "",
    PATH: [
      fakeBin,
      ...(installDirOnPath ? [installBin] : []),
      process.env.PATH,
    ].join(path.delimiter),
    HOME: homeDir,
    TMPDIR: installerTmpRoot,
    CI: "",
    CTX_DATA_ROOT: dataRoot,
    SHELL: "/bin/bash",
    CTX_PLATFORM: platform,
    CTX_BIN_DIR: installBin,
    CTX_MAN_DIR: manDir,
    CTX_RELEASE_METADATA_URL: "",
    CTX_RELEASE_METADATA_SIGNATURE_URL: "",
    CTX_UPGRADE_FUNCTIONS_BASE: "",
    CTX_UPGRADE_CHANNEL: "",
    CTX_SETUP_PROGRESS: "none",
    CTX_INSTALL_NO_DAEMON: "",
    CTX_FAKE_GZIP_PROBE_NOISE: "0",
    CTX_FAKE_HOSTED_PARTIAL_FAILURE: "0",
    CTX_INSTALL_PRO_TRIAL: "",
    CTX_INSTALL_NO_PRO_TRIAL: "",
    CTX_ALLOW_CUSTOM_RELEASE_BASE_URL: "1",
    CTX_FAKE_DOWNLOAD_URL_LOG: downloadUrlLogPath,
    CTX_FAKE_METADATA: metadataPath,
    CTX_FAKE_BRIDGE_METADATA: bridgeMetadataPath,
    CTX_FAKE_BRIDGE_METADATA_SIGNATURE: bridgeMetadataSignaturePath,
    CTX_FAKE_BRIDGE_ARTIFACT: bridgeArtifactPath,
    CTX_FAKE_METADATA_SIGNATURE: metadataSignaturePath,
    CTX_FAKE_ARTIFACT: artifactPath,
    CTX_FAKE_COMPANION_ARTIFACT: companionArtifactPath,
    CTX_FAKE_PAIR_ENVELOPE: pairEnvelopePath,
    CTX_FAKE_PAIR_MARKER_SOURCE_LOG: pairMarkerSourceLogPath,
    CTX_FAKE_PAIR_INVOCATION_PATH_LOG: pairInvocationPathLogPath,
    CTX_FAKE_COMPRESSED_ARTIFACT: compressedArtifact === "missing" ? "missing" : compressedArtifactPath,
    CTX_FAKE_CTX_ARGS_LOG: setupArgsPath,
    CTX_FAKE_CTX_ENV_LOG: setupEnvPath,
    CTX_FAKE_SUPERVISOR_ENV_LOG: supervisorEnvPath,
    CTX_FAKE_CTX_COMMAND_LOG: commandLogPath,
    CTX_FAKE_NODE: process.execPath,
    CTX_FAKE_HOSTED_SETUP_ENV_LOG: hostedSetupEnvPath,
    CTX_FAKE_RUNTIME_REPAIR_LOG: runtimeRepairLogPath,
    CTX_FAKE_NO_DAEMON_SETUP: noDaemonSetupPath,
    CTX_FAKE_SEMANTIC_ROOT: semanticRoot,
    CTX_FAKE_INSTALL_STAGE_LOG: installStageLogPath,
    CTX_FAKE_LOG_MUTATIONS: managedPair ? "1" : "0",
    ...env,
  };
  if (unsetNoColor) {
    delete childEnv.NO_COLOR;
  }
  if (prepareInstall !== null) {
    prepareInstall({ fakeBin, homeDir, installBin, manDir, sandbox });
  }
  const shellArguments = installUmask === null
    ? [scriptPath, ...args]
    : [
      "-c",
      'umask "$1"; shift; exec sh "$@"',
      "ctx-installer-shell",
      installUmask,
      scriptPath,
      ...args,
    ];
  const result = ttyInput === null
    ? spawnSync("sh", shellArguments, {
      encoding: "utf8",
      env: childEnv,
    })
    : spawnSync(
      utilLinuxScriptCommand,
      [
        "-qefc",
        `cat ${shellQuote(scriptPath)} | ${
          unsetNoColor ? "env -u NO_COLOR " : ""
        }sh -s -- ${args.map(shellQuote).join(" ")} ${
          ttyStderrRedirected
            ? `2>${shellQuote(redirectedReceiptPath)}`
            : "2>/dev/tty"
        }`,
        "/dev/null",
      ],
      {
        encoding: "utf8",
        env: childEnv,
        input: ttyInput,
      },
    );
  const rerun = (nextArgs = args, envOverrides = {}) => spawnSync(
    "sh",
    [scriptPath, ...nextArgs],
    {
      encoding: "utf8",
      env: {
        ...childEnv,
        ...envOverrides,
      },
    },
  );

  return {
    result,
    rerun,
    cleanup: () => rmSync(sandbox, { recursive: true, force: true }),
    setupArgsPath,
    setupEnvPath,
    supervisorEnvPath,
    commandLogPath,
    downloadUrlLogPath,
    bridgeArtifactPath,
    artifactPath,
    hostedSetupEnvPath,
    runtimeRepairLogPath,
    semanticRoot,
    installerTmpRoot,
    installStageLogPath,
    pairMarkerSourceLogPath,
    pairInvocationPathLogPath,
    redirectedReceiptPath,
    fakeBin,
    installBin,
    homeDir,
    manDir,
    configPath,
    configContents,
    childEnv,
    metadataPrivateKeyPem: signedMetadata.privateKeyPem,
    dataRoot,
    sandbox,
    scriptPath,
  };
}

function runHostedUninstallForInstallerFixture(fixture, args = ["--keep-data"]) {
  const uninstallPath = path.join(fixture.sandbox, "uninstall.sh");
  writeFileSync(
    uninstallPath,
    renderUninstallScript({ installAttemptId: "ia_installer_upgrade_uninstall" }),
    { mode: 0o755 },
  );
  return spawnSync("sh", [uninstallPath, ...args], {
    encoding: "utf8",
    env: {
      ...fixture.childEnv,
      CTX_ANALYTICS_ENABLED: "false",
      CTX_UNINSTALL_INSTALL_PATH: path.join(fixture.installBin, "ctx"),
      CTX_FAKE_ACTIVE_INTEGRATIONS_PATH: JSON.parse(readFileSync(
        path.join(fixture.installBin, "ctx.install.json"),
        "utf8",
      )).integrations_path ?? "",
      CTX_DATA_ROOT: fixture.dataRoot,
    },
  });
}

function readCtxCommands(filePath) {
  if (!existsSync(filePath)) return [];
  return readFileSync(filePath, "utf8").trim().split("\n").filter(Boolean);
}

const MANAGED_PAIR_RECONCILE_COMMAND =
  "--ctx-core-managed-pair-reconcile-integration-v1";

function readOrderedCtxCommands(fixture) {
  const prefix = `${MANAGED_PAIR_RECONCILE_COMMAND} ${path.dirname(fixture.installBin)} - `;
  return readCtxCommands(fixture.commandLogPath).map((command) => {
    if (!command.startsWith(`${MANAGED_PAIR_RECONCILE_COMMAND} `)) return command;
    assert.ok(command.startsWith(prefix), command);
    const source = command.slice(prefix.length);
    assert.ok(source.startsWith(`${fixture.installerTmpRoot}${path.sep}`), source);
    assert.equal(path.basename(source), "install-integrations");
    return MANAGED_PAIR_RECONCILE_COMMAND;
  });
}

function installerOutput(result) {
  return `${result.stdout ?? ""}${result.stderr ?? ""}`.replaceAll("\r", "");
}

function readOwnershipRecords(installBin) {
  const marker = JSON.parse(readFileSync(path.join(installBin, "ctx.install.json"), "utf8"));
  return readFileSync(marker.integrations_path, "utf8")
    .trimEnd()
    .split("\n")
    .slice(2)
    .filter(Boolean)
    .map((line) => {
      const [kind, digest, target, ...extra] = line.split("\t");
      assert.deepEqual(extra, []);
      return { kind, digest, target };
    });
}

function stripAnsi(value) {
  return value.replace(/\u001b\[[0-9;]*m/g, "");
}

function assertLinesAtMost(value, width) {
  for (const line of stripAnsi(value).split("\n")) {
    assert.ok(
      line.length <= width,
      `expected at most ${width} columns, got ${line.length}: ${line}`,
    );
  }
}

function installerOutcome(result) {
  const output = installerOutput(result);
  const start = output.lastIndexOf("Installing ctx ");
  assert.notEqual(start, -1, output);
  return output.slice(start);
}

export {
  INSTALL_SCRIPT_FAMILIES,
  INSTALL_STAGE_EVENT_NAME,
  INSTALL_STAGE_EVENT_VERSION,
  INSTALL_STAGE_PAYLOAD_KEYS,
  INSTALL_STAGES,
  INSTALL_STAGE_STATUS_PAIRS,
  INSTALL_STAGE_STATUSES,
  MANAGED_PAIR_RECONCILE_COMMAND,
  assert,
  assertLinesAtMost,
  chmodSync,
  existsSync,
  fileURLToPath,
  gzipSync,
  installerOutcome,
  installerOutput,
  linkSync,
  makeSignedMetadataFixture,
  mkdirSync,
  mkdtempSync,
  path,
  powerShellCommand,
  powerShellNativeGuardBlock,
  readCtxCommands,
  readOrderedCtxCommands,
  readFileSync,
  readOwnershipRecords,
  readdirSync,
  renderCliInstallPowerShellScript,
  renderCliInstallScript,
  renderUninstallScript,
  rmSync,
  runHostedUninstallForInstallerFixture,
  runRenderedCliInstaller,
  sha256,
  signMetadataBase64,
  spawnSync,
  statSync,
  stripAnsi,
  symlinkSync,
  test,
  tmpdir,
  utilLinuxScriptCommand,
  writeExecutable,
  writeFileSync,
};
