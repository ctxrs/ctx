import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, chmodSync, existsSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { renderAdeInstallScript } from "./ade-install-script.js";

const makeTempDir = (prefix) => mkdtempSync(path.join(tmpdir(), prefix));
const NODE_EXEC_PATH = process.env.JS_BINARY__NODE_BINARY ?? process.execPath;
const NODE_BIN = JSON.stringify(NODE_EXEC_PATH);
const escapeRegExp = (value) => String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const createFakeAppImage = () => `#!/bin/sh
set -eu
if [ "\${1:-}" = "--appimage-extract" ]; then
  mkdir -p squashfs-root/usr/share/icons/hicolor/512x512/apps
  printf '%s\\n' 'fake-icon-bytes' > squashfs-root/usr/share/icons/hicolor/512x512/apps/ctx.png
  exit 0
fi
exit 0
`;

const writeExecutable = (filePath, contents) => {
  writeFileSync(filePath, contents);
  chmodSync(filePath, 0o755);
};

const createExtractorScript = (stubDir) => {
  const extractorPath = path.join(stubDir, "extract-json.mjs");
  writeExecutable(
    extractorPath,
    `#!${NODE_EXEC_PATH}
import fs from "node:fs";

const manifestPath = process.argv[2];
const keyPath = process.argv[3];
const parts = String(keyPath ?? "").split(".");
let data = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
for (const part of parts) {
  if (data && typeof data === "object" && part in data) {
    data = data[part];
    continue;
  }
  process.stdout.write("");
  process.exit(0);
}
if (["string", "number", "boolean"].includes(typeof data)) {
  process.stdout.write(String(data));
} else {
  process.stdout.write("");
}
`,
  );
  return extractorPath;
};

const installStubCommands = (stubDir) => {
  const extractorPath = createExtractorScript(stubDir);
  writeExecutable(
    path.join(stubDir, "curl"),
    `#!/bin/sh
set -eu
dest=""
url=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o)
      dest="$2"
      shift 2
      ;;
    http://*|https://*|/*)
      url="$1"
      shift
      ;;
    *)
      shift
      ;;
  esac
done
[ -n "$dest" ] || exit 2
case "$url" in
  */latest.json*)
    cp "$CTX_TEST_MANIFEST_PATH" "$dest"
    ;;
  *)
    cp "$CTX_TEST_ARTIFACT_PATH" "$dest"
    ;;
esac
`,
  );
  writeExecutable(
    path.join(stubDir, "cp"),
    `#!/bin/sh
set -eu
src="\${1:-}"
dest="\${2:-}"
case "\${CTX_TEST_FAIL_CP_DEST_MATCH:-}" in
  "")
    ;;
  *)
    case "$dest" in
      *"$CTX_TEST_FAIL_CP_DEST_MATCH"*)
        exit 1
        ;;
    esac
    ;;
esac
exec /bin/cp "$src" "$dest"
`,
  );
  writeExecutable(
    path.join(stubDir, "mv"),
    `#!/bin/sh
set -eu
dest=""
for arg in "$@"; do
  dest="$arg"
done
case "\${CTX_TEST_FAIL_MV_DEST_MATCH:-}" in
  "")
    ;;
  *)
    case "$dest" in
      *"$CTX_TEST_FAIL_MV_DEST_MATCH"*)
        exit 1
        ;;
    esac
    ;;
esac
exec /bin/mv "$@"
`,
  );
  writeExecutable(
    path.join(stubDir, "uname"),
    `#!/bin/sh
set -eu
case "$1" in
  -s) printf '%s\\n' "$CTX_TEST_UNAME_S" ;;
  -m) printf '%s\\n' "$CTX_TEST_UNAME_M" ;;
  *) exit 2 ;;
esac
`,
  );
  writeExecutable(
    path.join(stubDir, "python3"),
    `#!/bin/sh
set -eu
exec ${NODE_BIN} "${extractorPath}" "$2" "$3"
`,
  );
  writeExecutable(
    path.join(stubDir, "id"),
    `#!/bin/sh
set -eu
case "$1" in
  -u) printf '%s\\n' "\${CTX_TEST_ID_U:-1000}" ;;
  *) exit 2 ;;
esac
`,
  );
  writeExecutable(
    path.join(stubDir, "plutil"),
    `#!/bin/sh
set -eu
key=""
manifest=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -extract)
      key="$2"
      shift 2
      ;;
    raw)
      shift
      ;;
    -o)
      shift 2
      ;;
    *)
      manifest="$1"
      shift
      ;;
  esac
done
exec ${NODE_BIN} "${extractorPath}" "$manifest" "$key"
`,
  );
  for (const name of ["hdiutil", "ditto", "open", "xdg-open", "update-desktop-database"]) {
    writeExecutable(
      path.join(stubDir, name),
      `#!/bin/sh
set -eu
exit 0
`,
    );
  }
  writeExecutable(
    path.join(stubDir, "sudo"),
    `#!/bin/sh
set -eu
exec "$@"
`,
  );
  writeExecutable(
    path.join(stubDir, "apt-get"),
    `#!/bin/sh
set -eu
printf '%s\\n' "$*" > "$CTX_TEST_APT_LOG"
exit 0
`,
  );
  writeExecutable(
    path.join(stubDir, "ctx"),
    `#!/bin/sh
set -eu
printf '%s\\n' "ctx $*" > "$CTX_TEST_CTX_LOG"
exit 0
`,
  );
};

const sha256 = (value) => crypto.createHash("sha256").update(value).digest("hex");

const withoutHostPythonEnv = () => {
  const env = { ...process.env };
  delete env.PYTHONHOME;
  delete env.PYTHONPATH;
  return env;
};

const runInstaller = ({
  os,
  arch,
  manifest,
  artifactContents,
  installDirName = "install-root",
  binDirName = "bin-root",
  osReleaseText = "ID=testos\n",
  existingAppImageContents = null,
  existingIconContents = null,
  extraEnv = {},
}) => {
  const sandboxDir = makeTempDir("ctx-install-script-");
  const stubDir = path.join(sandboxDir, "stubs");
  mkdirSync(stubDir);
  installStubCommands(stubDir);

  const scriptPath = path.join(sandboxDir, "install.sh");
  const manifestPath = path.join(sandboxDir, "latest.json");
  const artifactPath = path.join(sandboxDir, "artifact.bin");
  const installDir = path.join(sandboxDir, installDirName);
  const binDir = path.join(sandboxDir, binDirName);
  const xdgDataHome = path.join(sandboxDir, "xdg-data");
  const osReleasePath = path.join(sandboxDir, "os-release");
  const aptLogPath = path.join(sandboxDir, "apt.log");
  const ctxLogPath = path.join(sandboxDir, "ctx.log");

  writeFileSync(scriptPath, renderAdeInstallScript());
  writeFileSync(manifestPath, JSON.stringify(manifest));
  writeFileSync(artifactPath, artifactContents);
  writeFileSync(osReleasePath, osReleaseText);
  chmodSync(scriptPath, 0o755);
  mkdirSync(installDir);
  mkdirSync(binDir);
  mkdirSync(xdgDataHome);
  if (existingAppImageContents !== null) {
    writeFileSync(path.join(installDir, "ctx.AppImage"), existingAppImageContents);
  }
  if (existingIconContents !== null) {
    const existingIconPath = path.join(xdgDataHome, "icons", "hicolor", "512x512", "apps", "ctx.png");
    mkdirSync(path.dirname(existingIconPath), { recursive: true });
    writeFileSync(existingIconPath, existingIconContents);
  }

  const result = spawnSync("sh", [scriptPath], {
    encoding: "utf8",
    env: {
      ...withoutHostPythonEnv(),
      PATH: `${stubDir}:${process.env.PATH ?? ""}`,
      CTX_INSTALL_NO_OPEN: "1",
      CTX_INSTALL_DIR: installDir,
      CTX_BIN_DIR: binDir,
      CTX_TEST_MANIFEST_PATH: manifestPath,
      CTX_TEST_ARTIFACT_PATH: artifactPath,
      CTX_TEST_UNAME_S: os,
      CTX_TEST_UNAME_M: arch,
      CTX_INSTALL_OS_RELEASE_PATH: osReleasePath,
      CTX_TEST_APT_LOG: aptLogPath,
      CTX_TEST_CTX_LOG: ctxLogPath,
      XDG_DATA_HOME: xdgDataHome,
      ...extraEnv,
    },
  });

  const cleanup = () => rmSync(sandboxDir, { recursive: true, force: true });
  return { ...result, installDir, binDir, xdgDataHome, aptLogPath, ctxLogPath, cleanup };
};

test("renderAdeInstallScript emits a bootstrap script with stable defaults", () => {
  const script = renderAdeInstallScript();
  assert.match(script, /^#!\/bin\/sh/m);
  assert.match(script, /set -eu/);
  assert.match(script, /install_macos/);
  assert.match(script, /install_linux/);
  assert.match(script, /install_windows/);
  assert.match(script, /windows support is coming soon!/);
  assert.match(script, /https:\/\/api\.ade\.ctx\.rs\/functions\/v1/);
  assert.match(script, /channel="\$\{CTX_CHANNEL:-stable\}"/);
  assert.match(script, /download_id="\$\{CTX_DOWNLOAD_ID:-\}"/);
});

test("renderAdeInstallScript includes release resolution, checksum verify, and app launch", () => {
  const script = renderAdeInstallScript();
  assert.match(script, /releases\/\$channel\/latest\.json/);
  assert.match(script, /append_release_attribution/);
  assert.match(script, /ctx_download_id/);
  assert.match(script, /plutil -extract/);
  assert.match(script, /sha256sum/);
  assert.match(script, /shasum -a 256/);
  assert.match(script, /PYTHONHOME= PYTHONPATH= python3 - "\$manifest_json" "\$key"/);
  assert.match(script, /fail "manifest missing sha256 for selected artifact"/);
  assert.doesNotMatch(script, /skipping checksum verification/);
  assert.match(script, /hdiutil attach/);
  assert.match(script, /ditto "\$app_src" "\$staged_app"/);
  assert.match(script, /promote_staged_path/);
  assert.match(script, /stage_path_for_target/);
  assert.doesNotMatch(script, /rm -rf "\$target_app"/);
  assert.match(script, /open "\$target_app"/);
  assert.match(script, /CTX_DESKTOP_START_PATH="\$start_path"/);
  assert.match(script, /ctx\.AppImage/);
  assert.match(script, /ctx-desktop/);
  assert.match(script, /ctx\.png/);
  assert.match(script, /--appimage-extract/);
  assert.match(script, /Icon=\$icon_path/);
  assert.match(script, /export CTX_DESKTOP_START_PATH=\//);
  assert.match(script, /Installed desktop entry at/);
  assert.match(script, /ctx\.desktop/);
  assert.match(script, /first_open_start_path/);
  assert.match(script, /CTX_INSTALL_DIR/);
});

test("renderAdeInstallScript allows overriding function base and channel", () => {
  const script = renderAdeInstallScript({
    functionsBase: "https://example.test/functions/v1/",
    channel: "rc",
    downloadId: "dl_123",
    referrerDomain: "ctx.rs",
    utmSource: "twitter",
    utmMedium: "social",
    utmCampaign: "public-beta",
  });
  assert.match(script, /functions_base="\$\{CTX_FUNCTIONS_BASE:-https:\/\/example\.test\/functions\/v1\}"/);
  assert.match(script, /channel="\$\{CTX_CHANNEL:-rc\}"/);
  assert.match(script, /download_id="\$\{CTX_DOWNLOAD_ID:-dl_123\}"/);
  assert.match(script, /referrer_domain="\$\{CTX_INSTALL_REFERRER_DOMAIN:-ctx\.rs\}"/);
  assert.match(script, /utm_source="\$\{CTX_INSTALL_UTM_SOURCE:-twitter\}"/);
  assert.match(script, /utm_medium="\$\{CTX_INSTALL_UTM_MEDIUM:-social\}"/);
  assert.match(script, /utm_campaign="\$\{CTX_INSTALL_UTM_CAMPAIGN:-public-beta\}"/);
});

test("linux install hard-fails when manifest omits sha256", () => {
  const result = runInstaller({
    os: "Linux",
    arch: "x86_64",
    artifactContents: "fake-appimage",
    manifest: {
      channel: "stable",
      latest_version: "0.0.1",
      platforms: {
        "linux-x64": {
          appimage: {
            url_path: "/download/stable/0.0.1/ctx.AppImage",
          },
        },
      },
    },
  });
  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /error: manifest missing sha256 for selected artifact/);
    assert.equal(existsSync(path.join(result.installDir, "ctx.AppImage")), false);
  } finally {
    result.cleanup();
  }
});

test("macOS install hard-fails when manifest omits sha256", () => {
  const result = runInstaller({
    os: "Darwin",
    arch: "x86_64",
    artifactContents: "fake-dmg",
    manifest: {
      channel: "stable",
      latest_version: "0.0.1",
      platforms: {
        "macos-x64": {
          desktop: {
            url_path: "/download/stable/0.0.1/ctx.dmg",
          },
        },
      },
    },
  });
  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /error: manifest missing sha256 for selected artifact/);
  } finally {
    result.cleanup();
  }
});

test("linux install succeeds when manifest includes sha256", () => {
  const artifactContents = createFakeAppImage();
  const result = runInstaller({
    os: "Linux",
    arch: "x86_64",
    artifactContents,
    manifest: {
      channel: "stable",
      latest_version: "0.0.1",
      platforms: {
        "linux-x64": {
          appimage: {
            url_path: "/download/stable/0.0.1/ctx.AppImage",
            sha256: sha256(artifactContents),
          },
        },
      },
    },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stderr, /Verified artifact sha256/);
    assert.equal(existsSync(path.join(result.installDir, "ctx.AppImage")), true);
    const launcherPath = path.join(result.binDir, "ctx-desktop");
    assert.equal(existsSync(launcherPath), true);
    assert.match(readFileSync(launcherPath, "utf8"), /export CTX_DESKTOP_START_PATH=\//);
    assert.match(
      readFileSync(launcherPath, "utf8"),
      new RegExp(`exec \"${escapeRegExp(path.join(result.installDir, "ctx.AppImage"))}\"`),
    );
    assert.match(readFileSync(launcherPath, "utf8"), /"\$@"/);
    const iconPath = path.join(result.xdgDataHome, "icons", "hicolor", "512x512", "apps", "ctx.png");
    assert.equal(existsSync(iconPath), true);
    const desktopEntryPath = path.join(result.xdgDataHome, "applications", "ctx.desktop");
    assert.equal(existsSync(desktopEntryPath), true);
    assert.match(
      readFileSync(desktopEntryPath, "utf8"),
      new RegExp(`Exec=${escapeRegExp(launcherPath)}`),
    );
    assert.match(
      readFileSync(desktopEntryPath, "utf8"),
      new RegExp(`Icon=${escapeRegExp(iconPath)}`),
    );
  } finally {
    result.cleanup();
  }
});

test("linux install uses the AppImage path on Debian-like systems too", () => {
  const artifactContents = createFakeAppImage();
  const result = runInstaller({
    os: "Linux",
    arch: "x86_64",
    artifactContents,
    osReleaseText: "ID=ubuntu\nID_LIKE=debian\n",
    manifest: {
      channel: "stable",
      latest_version: "0.0.1",
      platforms: {
        "linux-x64": {
          appimage: {
            url_path: "/download/stable/0.0.1/ctx.AppImage",
            sha256: sha256(artifactContents),
          },
        },
      },
    },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.equal(existsSync(path.join(result.installDir, "ctx.AppImage")), true);
    assert.equal(existsSync(path.join(result.binDir, "ctx-desktop")), true);
    assert.equal(
      existsSync(path.join(result.xdgDataHome, "icons", "hicolor", "512x512", "apps", "ctx.png")),
      true,
    );
  } finally {
    result.cleanup();
  }
});

test("linux upgrade preserves the existing AppImage when staging the replacement fails", () => {
  const existingAppImageContents = "existing-appimage";
  const artifactContents = createFakeAppImage();
  const result = runInstaller({
    os: "Linux",
    arch: "x86_64",
    artifactContents,
    existingAppImageContents,
    extraEnv: {
      CTX_TEST_FAIL_CP_DEST_MATCH: ".ctx-stage.",
    },
    manifest: {
      channel: "stable",
      latest_version: "0.0.2",
      platforms: {
        "linux-x64": {
          appimage: {
            url_path: "/download/stable/0.0.2/ctx.AppImage",
            sha256: sha256(artifactContents),
          },
        },
      },
    },
  });
  try {
    assert.notEqual(result.status, 0);
    assert.equal(readFileSync(path.join(result.installDir, "ctx.AppImage"), "utf8"), existingAppImageContents);
    assert.match(result.stderr, /Verified artifact sha256/);
  } finally {
    result.cleanup();
  }
});

test("linux install does not promote AppImage when icon extraction fails", () => {
  const artifactContents = `#!/bin/sh
set -eu
exit 42
`;
  const result = runInstaller({
    os: "Linux",
    arch: "x86_64",
    artifactContents,
    manifest: {
      channel: "stable",
      latest_version: "0.0.1",
      platforms: {
        "linux-x64": {
          appimage: {
            url_path: "/download/stable/0.0.1/ctx.AppImage",
            sha256: sha256(artifactContents),
          },
        },
      },
    },
  });
  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /error: failed to extract application icon from AppImage/);
    assert.equal(existsSync(path.join(result.installDir, "ctx.AppImage")), false);
    assert.equal(existsSync(path.join(result.binDir, "ctx-desktop")), false);
  } finally {
    result.cleanup();
  }
});

test("linux upgrade preserves the existing AppImage when replacement icon extraction fails", () => {
  const existingAppImageContents = createFakeAppImage();
  const artifactContents = `#!/bin/sh
set -eu
exit 42
`;
  const result = runInstaller({
    os: "Linux",
    arch: "x86_64",
    artifactContents,
    existingAppImageContents,
    manifest: {
      channel: "stable",
      latest_version: "0.0.2",
      platforms: {
        "linux-x64": {
          appimage: {
            url_path: "/download/stable/0.0.2/ctx.AppImage",
            sha256: sha256(artifactContents),
          },
        },
      },
    },
  });
  try {
    assert.notEqual(result.status, 0);
    assert.equal(readFileSync(path.join(result.installDir, "ctx.AppImage"), "utf8"), existingAppImageContents);
    assert.match(result.stderr, /Verified artifact sha256/);
    assert.match(result.stderr, /error: failed to extract application icon from AppImage/);
  } finally {
    result.cleanup();
  }
});

test("linux upgrade preserves the existing icon when AppImage promotion fails", () => {
  const existingAppImageContents = createFakeAppImage();
  const existingIconContents = "existing-icon";
  const artifactContents = createFakeAppImage();
  const result = runInstaller({
    os: "Linux",
    arch: "x86_64",
    artifactContents,
    existingAppImageContents,
    existingIconContents,
    extraEnv: {
      CTX_TEST_FAIL_MV_DEST_MATCH: ".ctx-backup.",
    },
    manifest: {
      channel: "stable",
      latest_version: "0.0.2",
      platforms: {
        "linux-x64": {
          appimage: {
            url_path: "/download/stable/0.0.2/ctx.AppImage",
            sha256: sha256(artifactContents),
          },
        },
      },
    },
  });
  try {
    assert.notEqual(result.status, 0);
    assert.equal(readFileSync(path.join(result.installDir, "ctx.AppImage"), "utf8"), existingAppImageContents);
    const iconPath = path.join(result.xdgDataHome, "icons", "hicolor", "512x512", "apps", "ctx.png");
    assert.equal(readFileSync(iconPath, "utf8"), existingIconContents);
    assert.match(result.stderr, /error: failed to move existing ctx desktop AppImage aside/);
  } finally {
    result.cleanup();
  }
});
