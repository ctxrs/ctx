import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import worker, {
  isApprovedStagingBundleObjectPrefix,
  isApprovedStagingInstallerObjectKey,
} from "./index.js";

const INSTALL_SITE_ROOT = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);
const STAGING_INSTALLER_OBJECT_KEY =
  "installers/dogfood-managed-5f5a63228-fa68ad4a8-p32/1.0.0/install.sh";
const STAGING_BUNDLE_OBJECT_PREFIX =
  "bundles/dogfood-managed-5f5a63228-fa68ad4a8-p32/1.0.0/";

function stagingEnvironment(bucket, key = STAGING_INSTALLER_OBJECT_KEY) {
  return {
    STAGING_INSTALLER_OBJECT_KEY: key,
    STAGING_BUNDLE_OBJECT_PREFIX,
    STAGING_RELEASES_BUCKET: bucket,
  };
}

function extractInstallAttemptId(script) {
  const match = script.match(
    /(?:install_attempt_id="|\$installAttemptId = ")(ia_[A-Za-z0-9_-]{8,128})"/,
  );
  assert.ok(match, "expected rendered script to contain an install attempt ID");
  return match[1];
}

test("GET /uninstall returns the uninstall shell script", async () => {
  const response = await worker.fetch(new Request("https://ctx.rs/uninstall"));
  const body = await response.text();

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/x-shellscript; charset=utf-8");
  assert.match(body, /^#!\/bin\/sh/m);
  assert.match(body, /daemon disable\s+--prepare-uninstall --format=json/);
  assert.match(body, /uninstall_pro_with_native_cli/);
  assert.match(body, /"event_name":"install_stage"/);
  assert.match(body, /"stage":"'"\$report_stage"'"/);
  assert.match(body, /ctx uninstall complete\. Local ctx history was preserved\./);
});

test("GET /uninstall.sh returns the uninstall shell script", async () => {
  const response = await worker.fetch(new Request("https://ctx.rs/uninstall.sh"));
  const body = await response.text();

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/x-shellscript; charset=utf-8");
  assert.match(body, /pro uninstall --delete-data/);
  assert.match(body, /pro uninstall --keep-data/);
});

test("GET /uninstall.ps1 returns the managed Windows uninstall script", async () => {
  const response = await worker.fetch(new Request("https://ctx.rs/uninstall.ps1"));
  const body = await response.text();

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/plain; charset=utf-8");
  assert.match(body, /^\[CmdletBinding\(\)\]/);
  assert.match(body, /ctx-hosted-installer/);
  assert.match(body, /Invoke-CoreDaemonTeardown/);
  assert.match(body, /"--prepare-uninstall"/);
  assert.doesNotMatch(body, /Stop-InstalledCtxProcesses|Stop-Process/);
  assert.match(body, /pro", "uninstall"/);
  assert.match(body, /Local ctx history was preserved/i);
});

test("GET cli.ctx.rs/uninstall/cli.ps1 returns the Windows uninstall script", async () => {
  const response = await worker.fetch(new Request("https://cli.ctx.rs/uninstall/cli.ps1"));
  const body = await response.text();

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/plain; charset=utf-8");
  assert.match(body, /\[switch\]\$DeleteData/);
  assert.match(body, /\[switch\]\$KeepData/);
  assert.match(body, /managed install marker/);
});

test("GET ctx.rs/install returns the standalone CLI install script", async () => {
  const response = await worker.fetch(new Request("https://ctx.rs/install"));
  const body = await response.text();

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/x-shellscript; charset=utf-8");
  assert.match(body, /^#!\/bin\/sh/m);
  assert.match(body, /usage: curl -fsSL https:\/\/ctx\.rs\/install \| sh/);
  assert.match(body, /https:\/\/cli\.ctx\.rs\/functions\/v1/);
  assert.match(body, /ctx-release-metadata\.env/);
  assert.doesNotMatch(body, /installed ctx to \$install_path/);
  assert.match(body, /receipt_item "Installed and verified"/);
  assert.doesNotMatch(body, /Installed ctx binary/);
  assert.match(body, /CTX_INSTALL_NO_SETUP/);
  assert.match(body, /CTX_INSTALL_NO_DAEMON/);
  const usage = body.match(/usage\(\) \{[\s\S]*?^}/m)?.[0] ?? "";
  assert.match(body, /CTX_INSTALL_NO_SKILL/);
  assert.match(body, /CTX_INSTALL_SKILL_AGENTS/);
  assert.match(body, /CTX_INSTALL_ALL_SKILL_AGENTS/);
  assert.match(body, /CTX_INSTALL_NO_MODIFY_PATH/);
  assert.match(body, /CTX_INSTALL_NO_MAN/);
  assert.match(body, /CTX_ANALYTICS_ENABLED/);
  assert.match(body, /install_attempt_id="ia_[A-Za-z0-9_-]{8,128}"/);
  assert.match(body, /install-attempt/);
  assert.match(body, /\$install_path" docs man --out "\$generated_man_dir"/);
  assert.match(body, /set -- integrations install skills/);
  assert.match(body, /set -- "\$@" --format=json/);
  assert.match(body, /set -- setup --quiet --format json/);
  assert.match(body, /set -- "\$@" --wait/);
  assert.match(body, /setup_wait_requested=1/);
  assert.doesNotMatch(body, /status --format=json|pro setup --trial-only/);
  assert.match(body, /set -- "\$@" --progress "\$setup_progress"/);
  assert.match(body, /\$install_path" "\$@"/);
  assert.match(body, /ctx installer PATH setup/);
  assert.doesNotMatch(body, /download_id=/);
});

test("GET ctx.rs/install.sh returns the standalone CLI install script", async () => {
  const response = await worker.fetch(new Request("https://ctx.rs/install.sh"));
  const body = await response.text();

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/x-shellscript; charset=utf-8");
  assert.match(body, /usage: curl -fsSL https:\/\/ctx\.rs\/install \| sh/);
  assert.match(body, /Installs the ctx CLI from signed release metadata/);
});

test("GET ctx.rs/install.ps1 returns the Windows CLI install script", async () => {
  const response = await worker.fetch(new Request("https://ctx.rs/install.ps1"));
  const bytes = Buffer.from(await response.arrayBuffer());
  const nonAsciiOffset = bytes.findIndex((byte) => byte > 0x7f);
  const body = bytes.toString("ascii");

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/plain; charset=utf-8");
  assert.equal(
    nonAsciiOffset,
    -1,
    nonAsciiOffset < 0
      ? ""
      : `served install.ps1 contains a non-ASCII byte at offset ${nonAsciiOffset}`,
  );
  assert.match(body, /https:\/\/cli\.ctx\.rs\/functions\/v1/);
  assert.match(body, /CTX_RELEASE_ARTIFACT_windows_x64/);
  assert.match(body, /Write-ReceiptItem "Installed and verified"/);
  assert.doesNotMatch(body, /Installed ctx binary/);
  assert.match(body, /CTX_INSTALL_NO_SETUP/);
  assert.match(body, /CTX_INSTALL_NO_DAEMON/);
  assert.match(body, /CTX_INSTALL_NO_SKILL/);
  assert.match(body, /CTX_INSTALL_SKILL_AGENTS/);
  assert.match(body, /CTX_INSTALL_ALL_SKILL_AGENTS/);
  assert.match(body, /CTX_INSTALL_NO_MODIFY_PATH/);
  assert.match(body, /CTX_ANALYTICS_ENABLED/);
  assert.match(body, /CTX_INSTALL_ATTEMPT_ID/);
  assert.match(body, /install-attempt/);
  assert.match(body, /ctx-hosted-installer/);
  assert.match(body, /"integrations", "install", "skills"/);
  assert.match(body, /\$skillArgs \+= "--format=json"/);
  assert.match(body, /\$setupArgs = @\("setup", "--quiet", "--format", "json"\)/);
  assert.match(body, /\$setupArgs \+= "--wait"/);
  assert.match(body, /\$setupWaitRequested = -not \$setupNoDaemon/);
  assert.doesNotMatch(body, /"status",\s*"--format=json"|"pro",\s*"setup"/);
  assert.match(body, /\$setupArgs \+= @\("--progress", \$SetupProgress\)/);
  assert.match(body, /\$setupArgs \+= "--no-daemon"/);
  assert.match(body, /Invoke-HostedInstallerSetupCtxCaptured -Arguments \$setupArgs/);
});

test("GET cli.ctx.rs/install returns the standalone CLI install script", async () => {
  const response = await worker.fetch(new Request("https://cli.ctx.rs/install"));
  const body = await response.text();

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/x-shellscript; charset=utf-8");
  assert.match(body, /^#!\/bin\/sh/m);
  assert.match(body, /usage: curl -fsSL https:\/\/cli\.ctx\.rs\/install \| sh/);
  assert.match(body, /https:\/\/cli\.ctx\.rs\/functions\/v1/);
  assert.match(body, /ctx-release-metadata\.env/);
  assert.match(body, /receipt_item "Installed and verified"/);
  assert.doesNotMatch(body, /Installed ctx binary/);
  assert.match(body, /CTX_INSTALL_NO_SETUP/);
  assert.match(body, /CTX_INSTALL_NO_DAEMON/);
  assert.match(body, /CTX_INSTALL_NO_SKILL/);
  assert.match(body, /CTX_ANALYTICS_ENABLED/);
  assert.match(body, /install-attempt/);
  assert.match(body, /\$install_path" docs man --out "\$generated_man_dir"/);
  assert.match(body, /set -- integrations install skills/);
  assert.match(body, /set -- "\$@" --format=json/);
  assert.match(body, /set -- setup --quiet --format json/);
  assert.doesNotMatch(body, /status --format=json|pro setup --trial-only/);
  assert.match(body, /set -- "\$@" --progress "\$setup_progress"/);
  assert.match(body, /\$install_path" "\$@"/);
});

test("staging workers.dev shell routes stream only the selected exact R2 object", async () => {
  const installer = new TextEncoder().encode("#!/bin/sh\nprintf 'staging installer\\n'\n");
  const calls = [];
  const bucket = {
    async get(key) {
      calls.push(key);
      return {
        body: new ReadableStream({
          start(controller) {
            controller.enqueue(installer);
            controller.close();
          },
        }),
        size: installer.byteLength,
      };
    },
  };

  for (const route of ["/install", "/install.sh"]) {
    const response = await worker.fetch(
      new Request(`https://ctx-install-site-staging.example.workers.dev${route}`),
      stagingEnvironment(bucket),
    );

    assert.equal(response.status, 200);
    assert.equal(
      response.headers.get("content-type"),
      "text/x-shellscript; charset=utf-8",
    );
    assert.equal(response.headers.get("content-length"), String(installer.byteLength));
    assert.equal(response.headers.get("cache-control"), "no-store");
    assert.equal(response.headers.get("x-content-type-options"), "nosniff");
    assert.equal(await response.text(), new TextDecoder().decode(installer));
  }
  assert.deepEqual(calls, [
    STAGING_INSTALLER_OBJECT_KEY,
    STAGING_INSTALLER_OBJECT_KEY,
  ]);
});

test("staging installer configuration rejects missing and unapproved object keys before R2", async () => {
  const calls = [];
  const bucket = {
    async get(key) {
      calls.push(key);
      throw new Error("R2 must not be called for an invalid key");
    },
  };
  const invalidKeys = [
    undefined,
    "",
    "https://example.com/install.sh",
    "installers/stable/0.26.2/install.sh",
    "installers/staging/0.26.2/install.sh",
    "installers/dogfood-UPPER/0.26.2/install.sh",
    "installers/dogfood-safe/latest/install.sh",
    "installers/dogfood-safe/01.2.3/install.sh",
    "installers/dogfood-safe/0.26.2/install.ps1",
    "installers/dogfood-safe/0.26.2/install.sh/extra",
    "installers/dogfood-safe/0.26.2/../install.sh",
  ];

  for (const key of invalidKeys) {
    const env = { STAGING_RELEASES_BUCKET: bucket };
    if (key !== undefined) env.STAGING_INSTALLER_OBJECT_KEY = key;
    const response = await worker.fetch(
      new Request("https://ctx-install-site-staging.example.workers.dev/install"),
      env,
    );
    assert.equal(response.status, 503, String(key));
    assert.equal(response.headers.get("cache-control"), "no-store");
    assert.match(await response.text(), /unavailable/u);
  }
  assert.deepEqual(calls, []);
});

test("staging installer accepts only canonical dogfood installer object keys", () => {
  const validKeys = [
    "installers/dogfood-a/0.0.0/install.sh",
    STAGING_INSTALLER_OBJECT_KEY,
    "installers/dogfood-release-123/10.20.30/install.sh",
  ];
  for (const key of validKeys) {
    assert.equal(isApprovedStagingInstallerObjectKey(key), true, key);
  }

  const invalidKeys = [
    "installers/dogfood-/0.26.2/install.sh",
    "installers/dogfood--unsafe/0.26.2/install.sh",
    "installers/dogfood-unsafe-/0.26.2/install.sh",
    "installers/dogfood-safe/1.2.3-rc.1/install.sh",
    "installers/dogfood-safe/1.2/install.sh",
    "releases/dogfood-safe/1.2.3/install.sh",
    `installers/dogfood-${"a".repeat(64)}/1.2.3/install.sh`,
  ];
  for (const key of invalidKeys) {
    assert.equal(isApprovedStagingInstallerObjectKey(key), false, key);
  }
});

test("staging workers.dev streams only exact allowlisted immutable bundle objects", async () => {
  const body = new TextEncoder().encode("bundle-object");
  const calls = [];
  const env = stagingEnvironment({
    async get(key) {
      calls.push(key);
      return { body, size: body.byteLength };
    },
  });
  const allowed = [
    "metadata.env",
    "metadata.env.sig",
    "managed-pair-envelope.json",
    `sha256/${"a".repeat(64)}/ctx-linux-x64`,
  ];
  for (const relative of allowed) {
    const response = await worker.fetch(new Request(
      `https://ctx-install-site-staging.example.workers.dev/bundle/${relative}`,
    ), env);
    assert.equal(response.status, 200, relative);
    assert.equal(response.headers.get("cache-control"), "no-store");
    assert.equal(await response.text(), "bundle-object");
  }
  assert.deepEqual(calls, allowed.map((relative) =>
    `${STAGING_BUNDLE_OBJECT_PREFIX}${relative}`));

  for (const relative of [
    "",
    "../secret",
    "installer-support.tar.gz",
    "metadata.env/extra",
    `sha256/${"A".repeat(64)}/ctx-linux-x64`,
    `sha256/${"a".repeat(64)}/../ctx-linux-x64`,
    "arbitrary-private-object",
  ]) {
    const response = await worker.fetch(new Request(
      `https://ctx-install-site-staging.example.workers.dev/bundle/${relative}`,
    ), env);
    assert.equal(response.status, 404, relative);
  }
  assert.equal(calls.length, allowed.length);
});

test("staging bundle prefix is one canonical immutable dogfood namespace", () => {
  assert.equal(isApprovedStagingBundleObjectPrefix(STAGING_BUNDLE_OBJECT_PREFIX), true);
  for (const prefix of [
    "",
    "bundles/stable/1.0.0/",
    "bundles/dogfood-UPPER/1.0.0/",
    "bundles/dogfood-safe/latest/",
    "bundles/dogfood-safe/1.0.0/../",
    "bundles/dogfood-safe/1.0.0",
  ]) {
    assert.equal(isApprovedStagingBundleObjectPrefix(prefix), false, prefix);
  }
});

test("staging installer maps bounded R2 absence and errors without a fallback", async () => {
  const cases = [
    {
      expectedStatus: 404,
      get: async () => null,
    },
    {
      expectedStatus: 503,
      get: async () => {
        throw new Error("private storage detail");
      },
    },
    {
      expectedStatus: 503,
      get: async () => ({
        body: "empty",
        size: 0,
      }),
    },
    {
      expectedStatus: 503,
      get: async () => ({
        body: "oversized",
        size: 1024 * 1024 + 1,
      }),
    },
  ];

  for (const { expectedStatus, get } of cases) {
    let calls = 0;
    const response = await worker.fetch(
      new Request("https://ctx-install-site-staging.example.workers.dev/install"),
      stagingEnvironment({
        async get(key) {
          calls += 1;
          assert.equal(key, STAGING_INSTALLER_OBJECT_KEY);
          return get();
        },
      }),
    );
    const body = await response.text();

    assert.equal(response.status, expectedStatus);
    assert.equal(response.headers.get("content-type"), "text/plain; charset=utf-8");
    assert.equal(response.headers.get("cache-control"), "no-store");
    assert.equal(calls, 1);
    assert.doesNotMatch(body, /private storage detail/u);
    assert.doesNotMatch(body, /signed release metadata/u);
  }

  const missingBinding = await worker.fetch(
    new Request("https://ctx-install-site-staging.example.workers.dev/install"),
    { STAGING_INSTALLER_OBJECT_KEY },
  );
  assert.equal(missingBinding.status, 503);
});

test("production install routes ignore staging configuration and keep existing renderers", async () => {
  let stagingReads = 0;
  const env = stagingEnvironment({
    async get() {
      stagingReads += 1;
      throw new Error("production must not read staging R2");
    },
  });
  const shellResponse = await worker.fetch(
    new Request("https://ctx.rs/install"),
    env,
  );
  const shellBody = await shellResponse.text();
  const powershellResponse = await worker.fetch(
    new Request("https://cli.ctx.rs/install.ps1"),
    env,
  );
  const powershellBody = await powershellResponse.text();

  assert.equal(shellResponse.status, 200);
  assert.match(shellBody, /https:\/\/cli\.ctx\.rs\/functions\/v1/u);
  assert.match(shellBody, /ctx-release-metadata\.env/u);
  assert.equal(powershellResponse.status, 200);
  assert.match(powershellBody, /CTX_RELEASE_ARTIFACT_windows_x64/u);
  assert.equal(stagingReads, 0);
});

test("staging workers.dev does not claim an unselected PowerShell installer", async () => {
  let stagingReads = 0;
  const response = await worker.fetch(
    new Request("https://ctx-install-site-staging.example.workers.dev/install.ps1"),
    stagingEnvironment({
      async get() {
        stagingReads += 1;
        return null;
      },
    }),
  );
  const body = await response.text();

  assert.equal(response.status, 404);
  assert.doesNotMatch(body, /CTX_RELEASE_ARTIFACT_windows_x64/u);
  assert.equal(stagingReads, 0);
});

test("wrangler binds the exact staging installer without changing production routes", () => {
  const wrangler = fs.readFileSync(
    path.join(INSTALL_SITE_ROOT, "wrangler.toml"),
    "utf8",
  );
  const production = wrangler.match(/^[\s\S]*?(?=\n\[env\.staging\])/u)?.[0];
  const staging = wrangler.match(/\[env\.staging\][\s\S]*$/u)?.[0];

  assert.ok(production);
  assert.ok(staging);
  assert.match(production, /pattern = "cli\.ctx\.rs"/u);
  assert.match(production, /pattern = "ctx\.rs\/install\*"/u);
  assert.doesNotMatch(production, /STAGING_INSTALLER_OBJECT_KEY/u);
  assert.doesNotMatch(production, /STAGING_BUNDLE_OBJECT_PREFIX/u);
  assert.doesNotMatch(production, /STAGING_RELEASES_BUCKET/u);
  assert.doesNotMatch(production, /ctx-releases-staging/u);
  assert.match(
    staging,
    new RegExp(
      `STAGING_INSTALLER_OBJECT_KEY = "${STAGING_INSTALLER_OBJECT_KEY.replaceAll(".", "\\.")}"`,
      "u",
    ),
  );
  assert.match(staging, /binding = "STAGING_RELEASES_BUCKET"/u);
  assert.match(
    staging,
    new RegExp(
      `STAGING_BUNDLE_OBJECT_PREFIX = "${STAGING_BUNDLE_OBJECT_PREFIX.replaceAll(".", "\\.")}"`,
      "u",
    ),
  );
  assert.equal([...staging.matchAll(/ctx-releases-staging/gu)].length, 2);
  assert.doesNotMatch(wrangler, /ALLOW_WORKERS_DEV_INSTALL_ROUTES/u);
});

test("GET cli.ctx.rs/install.ps1 returns the Windows CLI install script", async () => {
  const response = await worker.fetch(new Request("https://cli.ctx.rs/install.ps1"));
  const body = await response.text();

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/plain; charset=utf-8");
  assert.match(body, /https:\/\/cli\.ctx\.rs\/functions\/v1/);
  assert.match(body, /CTX_RELEASE_ARTIFACT_windows_x64/);
  assert.match(body, /Write-ReceiptItem "Installed and verified"/);
  assert.doesNotMatch(body, /Installed ctx binary/);
  assert.match(body, /CTX_INSTALL_NO_SETUP/);
  assert.match(body, /CTX_INSTALL_NO_SKILL/);
  assert.match(body, /CTX_ANALYTICS_ENABLED/);
  assert.match(body, /CTX_INSTALL_ATTEMPT_ID/);
  assert.match(body, /install-attempt/);
  assert.match(body, /ctx-hosted-installer/);
  assert.match(body, /"integrations", "install", "skills"/);
  assert.match(body, /\$skillArgs \+= "--format=json"/);
  assert.match(body, /\$setupArgs = @\("setup", "--quiet", "--format", "json"\)/);
  assert.doesNotMatch(body, /"status",\s*"--format=json"|"pro",\s*"setup"/);
  assert.match(body, /\$setupArgs \+= @\("--progress", \$SetupProgress\)/);
});

test("CLI install routes render a fresh anonymous attempt ID per response", async () => {
  const first = await worker.fetch(new Request("https://ctx.rs/install"));
  const second = await worker.fetch(new Request("https://ctx.rs/install"));
  const firstBody = await first.text();
  const secondBody = await second.text();

  const firstId = extractInstallAttemptId(firstBody);
  const secondId = extractInstallAttemptId(secondBody);

  assert.match(firstId, /^ia_[A-Za-z0-9_-]{8,128}$/);
  assert.match(secondId, /^ia_[A-Za-z0-9_-]{8,128}$/);
  assert.notEqual(firstId, secondId);
});

test("uninstall routes render a fresh anonymous attempt ID per response", async () => {
  const first = await worker.fetch(new Request("https://ctx.rs/uninstall"));
  const second = await worker.fetch(new Request("https://ctx.rs/uninstall.sh"));
  const firstId = extractInstallAttemptId(await first.text());
  const secondId = extractInstallAttemptId(await second.text());

  assert.match(firstId, /^ia_[A-Za-z0-9_-]{8,128}$/);
  assert.match(secondId, /^ia_[A-Za-z0-9_-]{8,128}$/);
  assert.notEqual(firstId, secondId);
});

test("PowerShell uninstall routes render a fresh anonymous attempt ID per response", async () => {
  const first = await worker.fetch(new Request("https://ctx.rs/uninstall.ps1"));
  const second = await worker.fetch(new Request("https://ctx.rs/uninstall/cli.ps1"));
  const firstId = extractInstallAttemptId(await first.text());
  const secondId = extractInstallAttemptId(await second.text());

  assert.match(firstId, /^ia_[A-Za-z0-9_-]{8,128}$/);
  assert.match(secondId, /^ia_[A-Za-z0-9_-]{8,128}$/);
  assert.notEqual(firstId, secondId);
});

test("GET ade.ctx.rs/install returns the public archive ADE install script", async () => {
  const response = await worker.fetch(
    new Request(
      "https://ade.ctx.rs/install?ctx_download_id=ctx-download-123&utm_source=hello world&utm_medium=Email&utm_campaign=Spring Sale&referrer_domain=https://Example.COM/path",
    ),
  );
  const body = await response.text();

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/x-shellscript; charset=utf-8");
  assert.match(body, /^#!\/bin\/sh/m);
  assert.match(body, /functions_base="\$\{CTX_FUNCTIONS_BASE:-https:\/\/api\.ade\.ctx\.rs\/functions\/v1\}"/);
  assert.match(body, /manifest_url_base="\$\{functions_base%\/\}\/releases\/\$channel\/latest\.json"/);
  assert.match(body, /download_id="\$\{CTX_DOWNLOAD_ID:-ctx-download-123\}"/);
  assert.match(body, /referrer_domain="\$\{CTX_INSTALL_REFERRER_DOMAIN:-example\.com\}"/);
  assert.match(body, /utm_source="\$\{CTX_INSTALL_UTM_SOURCE:-hello_world\}"/);
  assert.match(body, /utm_medium="\$\{CTX_INSTALL_UTM_MEDIUM:-Email\}"/);
  assert.match(body, /utm_campaign="\$\{CTX_INSTALL_UTM_CAMPAIGN:-Spring_Sale\}"/);
  assert.match(body, /ctx desktop/);
});

test("GET ade.ctx.rs/install.sh falls back to a normalized Referer hostname", async () => {
  const response = await worker.fetch(
    new Request("https://ade.ctx.rs/install.sh?utm_source=desktop-launch", {
      headers: {
        referer: "https://Docs.Ctx.rs/guides/install?from=nav",
      },
    }),
  );
  const body = await response.text();

  assert.equal(response.status, 200);
  assert.match(body, /download_id="\$\{CTX_DOWNLOAD_ID:-[0-9a-f-]{36}\}"/);
  assert.match(body, /referrer_domain="\$\{CTX_INSTALL_REFERRER_DOMAIN:-docs\.ctx\.rs\}"/);
  assert.match(body, /utm_source="\$\{CTX_INSTALL_UTM_SOURCE:-desktop-launch\}"/);
  assert.match(body, /ctx desktop/);
});

test("GET ctx.rs/install/ade is not a public install alias", async () => {
  const response = await worker.fetch(new Request("https://ctx.rs/install/ade"));
  const body = await response.text();

  assert.equal(response.status, 404);
  assert.doesNotMatch(body, /ctx desktop/);
});

test("GET /install/control-plane is no longer served", async () => {
  const response = await worker.fetch(new Request("https://ctx.rs/install/control-plane"));
  const body = await response.text();

  assert.equal(response.status, 404);
  assert.match(body, /Not Found/);
});

test("GET ctx.rs root advertises the CLI installer", async () => {
  const response = await worker.fetch(new Request("https://ctx.rs/"));
  const body = await response.text();

  assert.equal(response.status, 200);
  assert.match(body, /curl -fsSL https:\/\/ctx\.rs\/install \| sh/);
  assert.doesNotMatch(body, /ctx\.rs\/uninstall/);
});

test("GET cli.ctx.rs root advertises the ctx.rs CLI installer", async () => {
  const response = await worker.fetch(new Request("https://cli.ctx.rs/"));
  const body = await response.text();

  assert.equal(response.status, 200);
  assert.match(body, /curl -fsSL https:\/\/ctx\.rs\/install \| sh/);
});
