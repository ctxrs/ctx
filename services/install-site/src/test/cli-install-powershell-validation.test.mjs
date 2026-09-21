// PowerShell control, metadata, and install mutation contracts.
import "./cli-install-powershell-resource-tests.mjs";
import {
  INSTALL_SCRIPT_FAMILIES,
  INSTALL_STAGE_EVENT_NAME,
  INSTALL_STAGE_EVENT_VERSION,
  INSTALL_STAGE_PAYLOAD_KEYS,
  INSTALL_STAGES,
  INSTALL_STAGE_STATUS_PAIRS,
  INSTALL_STAGE_STATUSES,
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
} from "./cli-install-test-helpers.mjs";
import { powerShellControlFixture, powerShellPairStateFixture, powerShellReleaseFixture, powerShellStageFixture } from "./cli-install-powershell-fixture-helpers.mjs";
import {
  assertRuntimeRepairUsesVerifiedMetadata,
  readStageReports,
} from "./cli-install-report-helpers.mjs";

function assertPlainFailure(result) {
  assert.equal(result.status, 1);
  assert.doesNotMatch(result.stderr, /\u001b|\r?\n\s*\|/u);
  assert.match(result.stderr, /(?:^|\r?\n)install\.ps1: [^\r\n]+\r?\n$/u);
}

test(
  "hosted PowerShell config parser preserves supported Core settings and validates installer controls",
  { skip: powerShellCommand ? false : "PowerShell is not installed" },
  () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-config-controls-"));
    try {
      const renderer = path.join(sandbox, "install.ps1");
      writeFileSync(renderer, renderCliInstallPowerShellScript());
      const fixture = fileURLToPath(new URL("./cli-install-powershell-config-fixture.ps1", import.meta.url));
      const result = spawnSync(powerShellCommand, [
        "-NoProfile", "-NonInteractive", "-File", fixture,
        "-RendererPath", renderer, "-OutputDirectory", path.join(sandbox, "data"),
      ], { encoding: "utf8", timeout: 30_000 });
      assert.equal(result.status, 0, result.stderr);
      assert.equal(JSON.parse(result.stdout).cases, 11);
    } finally {
      rmSync(sandbox, { recursive: true, force: true });
    }
  },
);

test(
  "complete hosted PowerShell renderer serializes boolean pair provenance",
  { skip: powerShellCommand ? false : "PowerShell is not installed" },
  () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-marker-serialization-"));
    try {
      const rendererPath = path.join(sandbox, "install.ps1");
      writeFileSync(rendererPath, renderCliInstallPowerShellScript());
      const fixturePath = fileURLToPath(new URL(
        "./cli-install-powershell-marker-fixture.ps1", import.meta.url,
      ));
      for (const paired of [true, false]) {
        const output = path.join(sandbox, paired ? "paired" : "core-only");
        const result = spawnSync(powerShellCommand, [
          "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", fixturePath,
          "-RendererPath", rendererPath, "-OutputDirectory", output,
          ...(paired ? [] : ["-CoreOnly"]),
        ], { encoding: "utf8", timeout: 30_000 });
        assert.equal(result.status, 0, result.stderr);
        const receipt = JSON.parse(result.stdout);
        const bytes = readFileSync(path.join(output, "ctx.install.json"));
        assert.equal(receipt.renderer_sha256, sha256(readFileSync(rendererPath)));
        assert.equal(receipt.marker_sha256, sha256(bytes));
        const marker = JSON.parse(bytes);
        assert.equal(marker.managed_pair, paired ? true : undefined);
        assert.equal(Object.hasOwn(marker, "managed_pair"), paired);
      }
    } finally {
      rmSync(sandbox, { recursive: true, force: true });
    }
  },
);

test("rendered Windows CLI installer validates unsigned setup counts", {
  skip: powerShellCommand ? false : "PowerShell is not installed",
}, () => {
  const body = renderCliInstallPowerShellScript();
  const functions = body.slice(body.indexOf("function Test-JsonIntegerValue"), body.indexOf("function Remove-ConfigComment"));
  for (const [value, accepted] of [[3, true], [0, true], [null, true], [-1, false], [2.5, false], ["3", false]]) {
    const result = spawnSync(powerShellCommand, ["-NoProfile", "-NonInteractive", "-Command",
      `$ErrorActionPreference = 'Stop'\n${functions}\n$receipt = $env:CTX_FAKE_SETUP_RECEIPT | ConvertFrom-Json\nGet-OptionalUnsignedCount -Json $receipt -Name indexed_sessions`], {
      encoding: "utf8", timeout: 10000, env: { ...process.env, CTX_FAKE_SETUP_RECEIPT: JSON.stringify({ indexed_sessions: value }) },
    });
    assert.equal(result.status === 0, accepted, result.stderr);
  }
});

test(
  "rendered Windows CLI installer accepts only canonical Semantic opt-ins",
  { skip: powerShellCommand ? false : "PowerShell is not installed" },
  () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-ps-semantic-"));
    try {
      const installBin = path.join(sandbox, "bin");
      const dataRoot = path.join(sandbox, "data");
      const configPath = path.join(dataRoot, "config.toml");
      const scriptPath = path.join(sandbox, "install.ps1");
      const wrapperPath = path.join(sandbox, "run-install.ps1");
      mkdirSync(installBin);
      mkdirSync(dataRoot);
      writeFileSync(scriptPath, powerShellControlFixture());
      writeFileSync(
        wrapperPath,
        `
$arguments = @{
    BinDir = $env:CTX_BIN_DIR
    NoSetup = $true
    NoModifyPath = $true
    Semantic = ($env:CTX_FAKE_SEMANTIC_SWITCH -eq "1")
    NoDaemon = ($env:CTX_FAKE_NO_DAEMON_SWITCH -eq "1")
}
$global:LASTEXITCODE = 0
& $env:CTX_INSTALL_PS1 @arguments
$fixtureExit = $global:LASTEXITCODE
exit $fixtureExit
`,
      );

      function runSemanticCase({
        installControl,
        searchControl,
        semanticSwitch = false,
        noDaemonSwitch = false,
        semanticConfig = null,
        daemonConfig = null,
        rawConfig = null,
      }) {
        rmSync(configPath, { force: true });
        const configSections = [];
        if (semanticConfig !== null) {
          configSections.push(`[search]\nsemantic = ${semanticConfig ? "true" : "false"}`);
        }
        if (daemonConfig !== null) {
          configSections.push(`[daemon]\nenabled = ${daemonConfig ? "true" : "false"}`);
        }
        const configContents = rawConfig ?? (
          configSections.length > 0 ? `${configSections.join("\n")}\n` : ""
        );
        if (configContents) {
          writeFileSync(configPath, configContents);
        }
        const env = {
          ...process.env,
          HOME: sandbox,
          CTX_BIN_DIR: installBin,
          CTX_DATA_ROOT: dataRoot,
          CTX_INSTALL_PS1: scriptPath,
          CTX_FAKE_SEMANTIC_SWITCH: semanticSwitch ? "1" : "0",
          CTX_FAKE_NO_DAEMON_SWITCH: noDaemonSwitch ? "1" : "0",
        };
        delete env.CTX_INSTALL_SEMANTIC;
        delete env.CTX_SEARCH_SEMANTIC;
        delete env.CTX_DAEMON_ENABLED;
        delete env.CTX_DAEMON_OFF;
        delete env.CTX_DISABLE_DAEMON;
        delete env.CTX_INSTALL_NO_DAEMON;
        delete env.CTX_INSTALL_PRO_TRIAL;
        delete env.CTX_INSTALL_NO_PRO_TRIAL;
        if (installControl !== undefined) env.CTX_INSTALL_SEMANTIC = installControl;
        if (searchControl !== undefined) env.CTX_SEARCH_SEMANTIC = searchControl;
        return spawnSync(powerShellCommand, ["-NoProfile", "-File", wrapperPath], {
          encoding: "utf8",
          env,
        });
      }

      function assertControls(result, semantic, daemon = true) {
        assert.equal(result.status, 0, result.stderr);
        assert.deepEqual(JSON.parse(result.stdout), { semantic, daemon });
      }

      const searchOnly = runSemanticCase({ searchControl: "1" });
      assertControls(searchOnly, true);

      const persisted = runSemanticCase({ semanticConfig: true });
      assertControls(persisted, true);

      const rootDotted = runSemanticCase({ rawConfig: "search.semantic = true\n" });
      assertControls(rootDotted, true);

      const currentPublicConfig = runSemanticCase({
        rawConfig: [
          "[analytics]",
          "enabled = false",
          'endpoint = "https://example.test/#anchor"',
          "[local_usage]",
          "enabled = false",
          "[upgrade]",
          'auto = "APPLY"',
          'channel = "stable"',
          "interval_hours = +24",
          "[daemon]",
          "enabled = false",
          'mode = "source-refresh-only"',
          "[indexing]",
          'mode = "automatic"',
          "[search]",
          "semantic = true",
          "",
        ].join("\n"),
      });
      assertControls(currentPublicConfig, true);

      const emptySearch = runSemanticCase({
        searchControl: "",
        rawConfig: "search.semantic = true\n",
      });
      assertControls(emptySearch, true);
      assert.equal(readFileSync(configPath, "utf8"), "search.semantic = true\n");

      for (const value of [" true ", '"true"', "\u00a0true\u00a0"]) {
        const normalized = runSemanticCase({ searchControl: value });
        assertControls(normalized, true);
      }
      for (const value of ["\u00a0", '""']) {
        const normalizedEmpty = runSemanticCase({
          searchControl: value,
          rawConfig: "search.semantic = true\n",
        });
        assertControls(normalizedEmpty, true);
      }

      const explicitFalse = runSemanticCase({
        searchControl: "false",
        noDaemonSwitch: true,
        semanticConfig: true,
      });
      assertControls(explicitFalse, false, false);
      assert.equal(readFileSync(configPath, "utf8"), "[search]\nsemantic = true\n");

      for (const value of ["1", "true", "TRUE", "yes", "on"]) {
        const enabled = runSemanticCase({ installControl: value });
        assertControls(enabled, true);
      }
      for (const value of ["", "0", "false", "NO", "off"]) {
        const disabled = runSemanticCase({ installControl: value });
        assertControls(disabled, false);
      }

      const explicitSwitch = runSemanticCase({ semanticSwitch: true });
      assertControls(explicitSwitch, true);

      for (const value of ["garbage", " true "]) {
        const invalid = runSemanticCase({ installControl: value });
        assert.notEqual(invalid.status, 0);
        assert.match(invalid.stderr, /CTX_INSTALL_SEMANTIC must be a canonical boolean/);
        assertPlainFailure(invalid);
      }
      const invalidSearch = runSemanticCase({ searchControl: "garbage" });
      assert.notEqual(invalidSearch.status, 0);
      assert.match(invalidSearch.stderr, /CTX_SEARCH_SEMANTIC must be a canonical boolean/);
      assertPlainFailure(invalidSearch);
    } finally {
      rmSync(sandbox, { recursive: true, force: true });
    }
  },
);

test(
  "rendered Windows CLI installer rejects invalid persisted config preflight before mutation",
  { skip: powerShellCommand ? false : "PowerShell is not installed" },
  () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-ps-preflight-"));
    try {
      const installBin = path.join(sandbox, "bin");
      const dataRoot = path.join(sandbox, "data");
      const configPath = path.join(dataRoot, "config.toml");
      const installerTmpRoot = path.join(sandbox, "installer-tmp");
      const webRequestLogPath = path.join(sandbox, "web-request.txt");
      const scriptPath = path.join(sandbox, "install.ps1");
      const wrapperPath = path.join(sandbox, "run-install.ps1");
      mkdirSync(installBin);
      mkdirSync(dataRoot);
      mkdirSync(installerTmpRoot);
      writeFileSync(scriptPath, powerShellControlFixture());
      writeFileSync(
        wrapperPath,
        `
$ErrorActionPreference = "Stop"
function Invoke-WebRequest {
    param(
        [string]$Uri,
        [string]$OutFile,
        [string]$Method,
        [string]$ContentType,
        [string]$Body,
        [switch]$UseBasicParsing,
        [int]$TimeoutSec
    )
    Add-Content -LiteralPath $env:CTX_FAKE_WEB_REQUEST_LOG -Value $Uri
    throw "unexpected web request before Semantic/no-daemon preflight: $Uri"
}
$arguments = @{
    BinDir = $env:CTX_BIN_DIR
    NoModifyPath = $true
    Semantic = ($env:CTX_FAKE_SEMANTIC_SWITCH -eq "1")
    NoDaemon = ($env:CTX_FAKE_NO_DAEMON_SWITCH -eq "1")
}
$global:LASTEXITCODE = 0
. $env:CTX_INSTALL_PS1 @arguments
$fixtureExit = $global:LASTEXITCODE
exit $fixtureExit
`,
      );

      function runPreflightCase({
        installControl,
        searchControl,
        daemonControl,
        daemonOffControl,
        disableDaemonControl,
        semanticSwitch = false,
        noDaemonSwitch = false,
        noDaemonEnvironment = false,
        semanticConfig = null,
        daemonConfig = null,
        spacedConfigSections = false,
        rawConfig = null,
      }) {
        rmSync(configPath, { force: true });
        rmSync(webRequestLogPath, { force: true });
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
          writeFileSync(configPath, configContents);
        }
        const env = {
          ...process.env,
          CTX_BIN_DIR: installBin,
          CTX_DATA_ROOT: dataRoot,
          TMPDIR: installerTmpRoot,
          TEMP: installerTmpRoot,
          TMP: installerTmpRoot,
          CTX_FAKE_WEB_REQUEST_LOG: webRequestLogPath,
          CTX_INSTALL_PS1: scriptPath,
          CTX_FAKE_SEMANTIC_SWITCH: semanticSwitch ? "1" : "0",
          CTX_FAKE_NO_DAEMON_SWITCH: noDaemonSwitch ? "1" : "0",
        };
        delete env.CTX_INSTALL_SEMANTIC;
        delete env.CTX_SEARCH_SEMANTIC;
        delete env.CTX_INSTALL_NO_DAEMON;
        delete env.CTX_DAEMON_ENABLED;
        delete env.CTX_DAEMON_OFF;
        delete env.CTX_DISABLE_DAEMON;
        if (installControl !== undefined) env.CTX_INSTALL_SEMANTIC = installControl;
        if (searchControl !== undefined) env.CTX_SEARCH_SEMANTIC = searchControl;
        if (noDaemonEnvironment) env.CTX_INSTALL_NO_DAEMON = "1";
        if (daemonControl !== undefined) env.CTX_DAEMON_ENABLED = daemonControl;
        if (daemonOffControl !== undefined) env.CTX_DAEMON_OFF = daemonOffControl;
        if (disableDaemonControl !== undefined) {
          env.CTX_DISABLE_DAEMON = disableDaemonControl;
        }
        return {
          result: spawnSync(powerShellCommand, ["-NoProfile", "-File", wrapperPath], {
            encoding: "utf8",
            env,
          }),
          configContents,
        };
      }

      const cases = [
        {
          name: "installer flag wins canonical enable",
          semanticSwitch: true,
          noDaemonSwitch: true,
          daemonControl: "true",
        },
        {
          name: "installer environment",
          searchControl: "true",
          noDaemonEnvironment: true,
        },
        {
          name: "canonical daemon false",
          installControl: "true",
          daemonControl: ' "FaLsE" ',
          daemonConfig: true,
        },
        {
          name: "deprecated daemon off wins canonical enable",
          searchControl: "true",
          daemonControl: "true",
          daemonOffControl: "anything",
        },
        {
          name: "deprecated disable daemon",
          semanticConfig: true,
          disableDaemonControl: " yes ",
        },
        {
          name: "persisted daemon opt-out wins canonical enable",
          semanticConfig: true,
          daemonConfig: false,
          daemonControl: "true",
        },
        {
          name: "public-valid spaced section headers",
          semanticSwitch: true,
          daemonConfig: false,
          spacedConfigSections: true,
        },
        {
          name: "root dotted search and daemon keys",
          rawConfig: "search.semantic = true\ndaemon.enabled = false\n",
        },
        {
          name: "canonical manual indexing overrides legacy daemon enable",
          rawConfig: [
            "[search]",
            "semantic = true",
            "[daemon]",
            "enabled = true",
            "[indexing]",
            'mode = "manual"',
            "",
          ].join("\n"),
        },
        {
          name: "root dotted and section Semantic duplicate",
          rawConfig: "search.semantic = true\n[search]\nsemantic = false\n",
          error: /duplicate config key `search\.semantic` at line 3; first set at line 1/,
        },
        {
          name: "normalized repeated daemon section",
          rawConfig: "[daemon]\nenabled = false\n[ daemon ]\nenabled = true\n",
          error: /duplicate config key `daemon\.enabled` at line 4; first set at line 2/,
        },
        {
          name: "unrelated public config duplicate",
          rawConfig: "[analytics]\nenabled = false\nenabled = true\n",
          error: /duplicate config key `analytics\.enabled` at line 3; first set at line 2/,
        },
        {
          name: "malformed line",
          rawConfig: "[upgrade]\nthis is not valid\n",
          error: /invalid config line 2/,
        },
        {
          name: "malformed section",
          rawConfig: "[search\nsemantic = true\n",
          error: /invalid config section header at line 1/,
        },
        {
          name: "empty section",
          rawConfig: "[]\nsearch.semantic = true\n",
          error: /empty config section header at line 1/,
        },
        {
          name: "empty key",
          rawConfig: "[search]\n = true\n",
          error: /empty config key at line 2/,
        },
        {
          name: "invalid Semantic boolean",
          rawConfig: "[search]\nsemantic = maybe\n",
          error: /search\.semantic at line 2 must be a boolean/,
        },
        {
          name: "invalid daemon boolean",
          rawConfig: "[daemon]\nenabled = FALSE\n",
          error: /daemon\.enabled at line 2 must be a boolean/,
        },
        {
          name: "unquoted string",
          rawConfig: "[indexing]\nmode = manual\n",
          error: /indexing\.mode at line 2 must be a quoted string/,
        },
        {
          name: "invalid indexing mode",
          rawConfig: "[indexing]\nmode = \"on-demand\"\n",
          error: /indexing\.mode at line 2 must be either/,
        },
        {
          name: "UTF-16LE config",
          rawConfig: Buffer.concat([
            Buffer.from([0xff, 0xfe]),
            Buffer.from("[search]\nsemantic = true\n", "utf16le"),
          ]),
          error: /persisted config is not valid UTF-8/,
        },
      ];

      for (const testCase of cases) {
        const { result, configContents } = runPreflightCase(testCase);
        const output = `${result.stdout}\n${result.stderr}`;
        assert.notEqual(result.status, 0, `${testCase.name}: installer unexpectedly succeeded`);
        assert.match(
          output,
          testCase.error ?? /Semantic installation requires an enabled daemon/,
        );
        assertPlainFailure(result);
        assert.equal(existsSync(webRequestLogPath), false, testCase.name);
        assert.equal(existsSync(path.join(installBin, "ctx.exe")), false, testCase.name);
        assert.equal(existsSync(path.join(installBin, "ctx.exe.install.json")), false, testCase.name);
        assert.deepEqual(readdirSync(installerTmpRoot), [], testCase.name);
        if (configContents) {
          if (Buffer.isBuffer(configContents)) {
            assert.deepEqual(readFileSync(configPath), configContents);
          } else {
            assert.equal(readFileSync(configPath, "utf8"), configContents);
          }
        } else {
          assert.equal(existsSync(configPath), false, testCase.name);
        }
      }
    } finally {
      rmSync(sandbox, { recursive: true, force: true });
    }
  },
);

test(
  "rendered Windows pair-state classifier rejects incomplete pairs without mutation",
  { skip: powerShellCommand ? false : "PowerShell is not installed" },
  () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-ps-pair-state-"));
    try {
      const bin = path.join(sandbox, "bin");
      mkdirSync(bin);
      const binary = path.join(bin, "ctx.exe"), marker = binary + ".install.json";
      const script = path.join(sandbox, "classify.ps1");
      writeFileSync(script, powerShellPairStateFixture());
      const run = () => spawnSync(powerShellCommand, [
        "-NoProfile", "-NonInteractive", "-File", script, "-BinDir", bin,
      ], { encoding: "utf8", timeout: 30_000 });
      const fresh = run();
      assert.equal(fresh.status, 0, fresh.stderr);
      assert.equal(fresh.stdout.trim(), "fresh");
      writeFileSync(binary, "working unmanaged binary");
      const unmanaged = run();
      assert.notEqual(unmanaged.status, 0);
      assert.match(unmanaged.stdout + unmanaged.stderr, /move it to a backup path/);
      assertPlainFailure(unmanaged);
      assert.equal(readFileSync(binary, "utf8"), "working unmanaged binary");
      assert.equal(existsSync(marker), false);
      rmSync(binary);
      writeFileSync(marker, "orphaned marker");
      const orphaned = run();
      assert.notEqual(orphaned.status, 0);
      assert.match(orphaned.stdout + orphaned.stderr, /executable is missing/);
      assertPlainFailure(orphaned);
      assert.equal(readFileSync(marker, "utf8"), "orphaned marker");
      assert.equal(existsSync(binary), false);
      writeFileSync(binary, "paired binary");
      const managed = run();
      assert.equal(managed.status, 0, managed.stderr);
      assert.equal(managed.stdout.trim(), "managed");
      assert.deepEqual(readdirSync(bin).sort(), ["ctx.exe", "ctx.exe.install.json"]);
      assert.equal(readFileSync(binary, "utf8"), "paired binary");
      assert.equal(readFileSync(marker, "utf8"), "orphaned marker");
      // This pure classifier is not the full recovery ordering: authenticated
      // Core recovery before final classification remains in platform.test.
      assert.deepEqual(readdirSync(sandbox).sort(), ["bin", "classify.ps1"]);
    } finally { rmSync(sandbox, { recursive: true, force: true }); }
  },
);

test(
  "rendered Windows release preparation verifies current signed pairs without native publication",
  { skip: powerShellCommand ? false : "PowerShell is not installed" },
  async () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-ps-crypto-"));
    try {
      const source = path.join(sandbox, "source");
      mkdirSync(source);
      const core = Buffer.from("inert Core fixture, never executable\n");
      const pro = Buffer.from("inert Pro fixture, never executable\n");
      const loaded = { version: "1.4.12", baseUrl: "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/1.4.12" };
      const target = { manifestRecord: { name: "managed-pair-envelope.json" }, companion: { identity: { object_key: `sha256/${sha256(pro)}/ctx-pro.exe` } } };
      const metadata = [
        "CTX_RELEASE_SCHEMA_VERSION=1", "CTX_RELEASE_CHANNEL=stable", `CTX_RELEASE_VERSION=${loaded.version}`,
        `CTX_RELEASE_BASE_URL=${loaded.baseUrl}`, "CTX_RELEASE_ARTIFACT_windows_x64=ctx.exe", `CTX_RELEASE_SHA256_windows_x64=${sha256(core)}`,
        "CTX_RELEASE_MANAGED_PAIR_ENVELOPE_windows_x64=managed-pair-envelope.json",
        `CTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_windows_x64=sha256/${sha256(core)}/ctx.exe`,
        `CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_windows_x64=${sha256(core)}`,
        `CTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_windows_x64=${target.companion.identity.object_key}`,
        `CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_windows_x64=${sha256(pro)}`,
      ].join("\n") + "\n";
      const signed = makeSignedMetadataFixture(metadata);
      const script = path.join(sandbox, "prepare.ps1");
      writeFileSync(script, powerShellReleaseFixture({
        metadataPublicKeyModulusBase64Url: signed.publicKeyModulusBase64Url,
        metadataPublicKeyExponentBase64Url: signed.publicKeyExponentBase64Url,
      }));
      const feed = "https://cli.ctx.rs/functions/v2/releases/stable/ctx-release-metadata.env";
      const routes = [
        { uri: feed, file: "metadata.env" }, { uri: feed + ".sig", file: "metadata.sig" },
        { uri: loaded.baseUrl + "/ctx.exe.gz", file: "core.gz" },
        { uri: loaded.baseUrl + "/ctx.exe", file: "core" },
        { uri: loaded.baseUrl + "/" + target.manifestRecord.name, file: "envelope" },
        { uri: loaded.baseUrl + "/" + target.companion.identity.object_key, file: "pro" },
      ];
      writeFileSync(path.join(source, "routes.json"), JSON.stringify(routes));
      writeFileSync(path.join(source, "envelope"), '{"inert":"envelope, not verified by this fixture"}\n');
      let attempt = 0;
      function run({ text = metadata, signature, coreBytes = core, proBytes = pro, gzip = true } = {}) {
        writeFileSync(path.join(source, "metadata.env"), text);
        writeFileSync(path.join(source, "metadata.sig"), signature ?? signMetadataBase64(text, signed.privateKeyPem));
        writeFileSync(path.join(source, "core"), coreBytes);
        if (gzip) writeFileSync(path.join(source, "core.gz"), gzipSync(coreBytes));
        else rmSync(path.join(source, "core.gz"), { force: true });
        writeFileSync(path.join(source, "pro"), proBytes);
        const work = path.join(sandbox, `attempt-${attempt++}`);
        mkdirSync(work);
        const result = spawnSync(powerShellCommand, [
          "-NoProfile", "-NonInteractive", "-File", script, "-WorkRoot", work, "-SourceRoot", source,
        ], { encoding: "utf8", timeout: 30_000 });
        assert.equal(existsSync(path.join(work, "installation")), false, "crypto preparation must not publish");
        return result;
      }
      for (const gzip of [true, false]) {
        const valid = run({ gzip });
        assert.equal(valid.status, 0, valid.stdout + valid.stderr);
        assert.deepEqual(JSON.parse(valid.stdout), {
          version: loaded.version, core: sha256(core), pro: sha256(pro), managed_pair: true,
        });
      }
      const damaged = Buffer.from(signed.signatureBase64, "base64");
      damaged[0] ^= 0xff;
      const cases = [
        [{ text: metadata.replace("CTX_RELEASE_CHANNEL=stable", "CTX_RELEASE_CHANNEL=beta") }, /metadata channel beta does not match/],
        [{ signature: damaged.toString("base64") }, /metadata signature verification failed/],
        [{ text: metadata.replace(`CTX_RELEASE_VERSION=${loaded.version}`, "CTX_RELEASE_VERSION=0.3.0") }, /stable installer targets before 1\.3\.2 are unsupported/],
        [{ coreBytes: Buffer.from("corrupt Core") }, /checksum mismatch/],
        [{ proBytes: Buffer.from("corrupt Pro") }, /checksum mismatch/],
        [{ text: metadata.replace(target.companion.identity.object_key, `sha256/${"f".repeat(64)}/ctx-pro.exe`) }, /object key does not match its checksum/],
        [{ text: metadata.replace(`CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_windows_x64=${sha256(core)}`, `CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_windows_x64=${"e".repeat(64)}`) }, /object key does not match its checksum/],
      ];
      for (const [options, expected] of cases) {
        const rejected = run(options);
        assert.notEqual(rejected.status, 0);
        assert.match(rejected.stdout + rejected.stderr, expected);
        assertPlainFailure(rejected);
      }
      const stageScript = path.join(sandbox, "stage.ps1");
      writeFileSync(stageScript, powerShellStageFixture());
      const emitted = spawnSync(powerShellCommand, ["-NoProfile", "-NonInteractive", "-File", stageScript], {
        encoding: "utf8", timeout: 30_000, env: { ...process.env, CTX_ANALYTICS_ENABLED: "true" },
      });
      assert.equal(emitted.status, 0, emitted.stdout + emitted.stderr);
      const captured = JSON.parse(emitted.stdout);
      assert.equal(captured.requests, 1);
      assert.equal(captured.uri, "https://example.invalid/install-attempt");
      const report = captured.body;
      assert.deepEqual(Object.keys(report).sort(), INSTALL_STAGE_PAYLOAD_KEYS);
      assert.equal(report.install_attempt_id, "ia_ps_test_attempt");
      assert.equal(report.event_name, INSTALL_STAGE_EVENT_NAME);
      assert.equal(report.event_version, INSTALL_STAGE_EVENT_VERSION);
      assert.equal(report.stage, "artifact_download");
      assert.equal(report.status, "completed");
      assert.equal(report.platform, "windows");
      assert.equal(report.arch, "x64");
      assert.equal(report.script_family, "powershell");
      // Actual current pair receipt/caller and marker serialization have their
      // existing platform/marker owners. No fake Core publication here.
      // Both-shell fresh/reinstall is mandatory in tests/install_live_smoke.ps1;
      // native path/ACL protection stays in the Windows platform test owner.
    } finally { rmSync(sandbox, { recursive: true, force: true }); }
  },
);
