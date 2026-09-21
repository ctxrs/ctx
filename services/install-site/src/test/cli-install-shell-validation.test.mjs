// POSIX validation, setup, Semantic, and diagnostics contracts.
import {
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
} from "./cli-install-test-helpers.mjs";
import {
  assertRuntimeRepairUsesVerifiedMetadata,
  readStageReports,
} from "./cli-install-report-helpers.mjs";
import { CLI_INSTALL_SHELL_VALIDATION } from "../cli-install-shell-validation.js";

test(
  "rendered CLI installer confirms installation before streamed setup progress begins",
  { skip: utilLinuxScriptCommand ? false : "util-linux script is not installed" },
  () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-pro-trial", "--no-skill", "--no-man"],
      env: {
        CTX_FAKE_SETUP_PROGRESS_STREAM: "1",
        CTX_SETUP_PROGRESS: "auto",
      },
      installDirOnPath: true,
      ttyInput: Buffer.alloc(0),
      unsetNoColor: true,
    });
    try {
      const output = installerOutcome(fixture.result);
      assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
      const settledInstall = "Installing ctx 9.9.9...\n\u001b[32m✓\u001b[0m Installed and verified\n";
      assert.ok(output.includes(settledInstall), output);
      assert.doesNotMatch(output, /\u001b\[2K\u001b\[32m✓\u001b\[0m Installed and verified/);
      assert.match(output, /\u001b\[36mIndexing local agent history: 50%\u001b\[0m/);
      assert.match(output, /\u001b\[36mIndexing local agent history: 100%\u001b\[0m/);
      assert.ok(
        output.indexOf("Installed and verified") <
          output.indexOf("Indexing local agent history: 50%"),
        output,
      );
      assert.match(
        output,
        /Installed and verified\r?\n\r?\n(?:\u001b\[2K)?\u001b\[36mIndexing local agent history: 50%/,
      );
      assert.deepEqual(readCtxCommands(fixture.commandLogPath), ["setup --quiet --format json --wait --progress auto"]);
    } finally {
      fixture.cleanup();
    }
  },
);

test(
  "rendered CLI installer animates only on a colored TTY",
  { skip: utilLinuxScriptCommand ? false : "util-linux script is not installed" },
  () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--no-man", "--no-modify-path"],
      env: { CTX_FAKE_ARTIFACT_DOWNLOAD_DELAY_SECONDS: "0.35" },
      ttyInput: Buffer.alloc(0),
      unsetNoColor: true,
    });
    try {
      const terminalOutput = `${fixture.result.stdout}${fixture.result.stderr}`;
      const frames = terminalOutput.match(/\rInstalling ctx 9\.9\.9(?:\.\.\.|\. {2}|\.\. )/g) ?? [];
      assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
      assert.ok(frames.length >= 3, terminalOutput);
      assert.match(terminalOutput, /\rInstalling ctx 9\.9\.9\.\.\./);
      assert.match(terminalOutput, /\rInstalling ctx 9\.9\.9\. {2}/);
    } finally {
      fixture.cleanup();
    }
  },
);

test(
  "rendered CLI installer keeps the install line static and plain without color",
  { skip: utilLinuxScriptCommand ? false : "util-linux script is not installed" },
  () => {
    const fixtures = [
      runRenderedCliInstaller({ args: ["--no-setup", "--no-man", "--no-modify-path"] }),
      runRenderedCliInstaller({
        args: ["--no-setup", "--no-man", "--no-modify-path"],
        env: { NO_COLOR: "" },
        ttyInput: Buffer.alloc(0),
      }),
    ];
    try {
      for (const fixture of fixtures) {
        const output = installerOutcome(fixture.result);
        assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
        assert.equal(output.match(/Installing ctx 9\.9\.9\.\.\./g)?.length, 1, output);
        assert.doesNotMatch(output, /\u001b/);
      }
    } finally {
      for (const fixture of fixtures) fixture.cleanup();
    }
  },
);

test("rendered CLI installer gives generic CI an explicit PATH handoff", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-skill", "--no-man"],
    env: { CI: "true" },
  });
  try {
    const output = installerOutput(fixture.result);
    assert.equal(fixture.result.status, 0, output);
    assert.match(output, /  Status:    ctx status/);
    assert.match(output, /To use the newly installed ctx in this shell, run:/);
    assert.match(output, /To add it for future terminal sessions, add .* to your shell profile\./);
    assert.ok(output.includes(`  export PATH="${fixture.installBin}:$PATH"`), output);
  } finally {
    fixture.cleanup();
  }
});

test("rendered CLI installer default-off performs zero semantic provisioning", () => {
  const {
    result,
    cleanup,
    runtimeRepairLogPath,
    semanticRoot,
    setupArgsPath,
  } = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
    semanticConfig: false,
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.doesNotMatch(result.stderr, /semantic:|runtime\/model provisioning/);
    assert.equal(existsSync(runtimeRepairLogPath), false);
    assert.equal(existsSync(semanticRoot), false);
    assert.equal(existsSync(setupArgsPath), false);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer accepts only canonical Semantic opt-ins", () => {
  const searchOnly = runRenderedCliInstaller({
    args: ["--no-skill", "--no-man"],
    env: { CTX_SEARCH_SEMANTIC: "1" },
  });
  try {
    assert.equal(searchOnly.result.status, 0, searchOnly.result.stderr);
    assert.doesNotMatch(searchOnly.result.stderr, /semantic:|runtime\/model provisioning/);
    assertRuntimeRepairUsesVerifiedMetadata(
      searchOnly.runtimeRepairLogPath,
      searchOnly.installerTmpRoot,
    );
    assert.equal(existsSync(path.join(searchOnly.semanticRoot, "runtime.installed")), true);
    assert.equal(
      readFileSync(searchOnly.setupArgsPath, "utf8"),
      "upgrade\n--channel\nstable\n--format=json\nsetup\n--quiet\n--format\njson\n--wait\n--semantic\n--progress\nnone\n",
    );
  } finally {
    searchOnly.cleanup();
  }

  const canonical = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
    env: { CTX_INSTALL_SEMANTIC: "true" },
  });
  try {
    assert.equal(canonical.result.status, 0, canonical.result.stderr);
    assert.doesNotMatch(canonical.result.stderr, /semantic:|runtime\/model provisioning/);
    assert.equal(existsSync(path.join(canonical.semanticRoot, "runtime.installed")), true);
  } finally {
    canonical.cleanup();
  }

  const invalid = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
    env: { CTX_INSTALL_SEMANTIC: "garbage" },
  });
  try {
    assert.notEqual(invalid.result.status, 0);
    assert.match(
      invalid.result.stderr,
      /CTX_INSTALL_SEMANTIC must be a canonical boolean/,
    );
    assert.equal(existsSync(invalid.semanticRoot), false);
  } finally {
    invalid.cleanup();
  }

  const invalidSearch = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
    env: { CTX_SEARCH_SEMANTIC: "garbage" },
  });
  try {
    assert.notEqual(invalidSearch.result.status, 0);
    assert.match(
      invalidSearch.result.stderr,
      /CTX_SEARCH_SEMANTIC must be a canonical boolean/,
    );
    assert.equal(existsSync(invalidSearch.semanticRoot), false);
  } finally {
    invalidSearch.cleanup();
  }
});

test("rendered CLI installer --semantic repairs complete assets before durable setup", () => {
  const {
    result,
    cleanup,
    runtimeRepairLogPath,
    semanticRoot,
    installerTmpRoot,
    setupArgsPath,
  } = runRenderedCliInstaller({
    args: ["--semantic", "--no-skill", "--no-man"],
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.doesNotMatch(result.stderr, /semantic:|runtime\/model provisioning/);
    assertRuntimeRepairUsesVerifiedMetadata(runtimeRepairLogPath, installerTmpRoot);
    assert.equal(readFileSync(path.join(semanticRoot, "model.installed"), "utf8"), "model\n");
    assert.equal(readFileSync(path.join(semanticRoot, "runtime.installed"), "utf8"), "runtime\n");
    assert.equal(
      readFileSync(setupArgsPath, "utf8"),
      "upgrade\n--channel\nstable\n--format=json\nsetup\n--quiet\n--format\njson\n--wait\n--semantic\n--progress\nnone\n",
    );
  } finally {
    cleanup();
  }
});

test("rendered CLI installer preserves supported semantic and source config", () => {
  for (const rawConfig of [
    "[daemon]\n[search]\nsemantic = true\n[indexing]\nmode = 'auto'\n[semantic]\nbuiltin_throttling = false\n",
    "[semantic]\nexecutor = 'http://127.0.0.1:8080'\nspace_id = 'my-space'\ndimensions = 768\n",
    "[sources]\nautomatic = false\n[sources.roots.work]\nprovider = 'codex'\npath = '/tmp/provider #1'\ngroup = 'work'\n[indexing]\nmode = 'manual'\n",
  ]) {
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--no-skill", "--no-man", "--no-modify-path"], rawConfig,
    });
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      assert.equal(readFileSync(fixture.configPath, "utf8"), rawConfig);
      assert.equal(existsSync(fixture.runtimeRepairLogPath), rawConfig.includes("semantic = true"));
    } finally {
      fixture.cleanup();
    }
  }
});

test("rendered CLI installer repairs when persisted config already enables Semantic", () => {
  const {
    result,
    cleanup,
    runtimeRepairLogPath,
    semanticRoot,
    installerTmpRoot,
  } = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
    semanticConfig: true,
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assertRuntimeRepairUsesVerifiedMetadata(runtimeRepairLogPath, installerTmpRoot);
    assert.equal(existsSync(path.join(semanticRoot, "model.installed")), true);
    assert.equal(existsSync(path.join(semanticRoot, "runtime.installed")), true);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer treats empty Semantic env as no override for a root dotted key", () => {
  const {
    result,
    cleanup,
    runtimeRepairLogPath,
    semanticRoot,
    installerTmpRoot,
    configPath,
  } = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
    env: { CTX_SEARCH_SEMANTIC: "" },
    rawConfig: "search.semantic = true\n",
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assertRuntimeRepairUsesVerifiedMetadata(runtimeRepairLogPath, installerTmpRoot);
    assert.equal(existsSync(path.join(semanticRoot, "model.installed")), true);
    assert.equal(existsSync(path.join(semanticRoot, "runtime.installed")), true);
    assert.equal(readFileSync(configPath, "utf8"), "search.semantic = true\n");
  } finally {
    cleanup();
  }
});

test("rendered CLI installer normalizes Semantic env like the public CLI", () => {
  for (const value of [" true ", '"true"', '""', "\u00a0true\u00a0"]) {
    const enabled = value === '""'
      ? runRenderedCliInstaller({
        args: ["--no-setup", "--no-man"],
        env: { CTX_SEARCH_SEMANTIC: value },
        rawConfig: "search.semantic = true\n",
      })
      : runRenderedCliInstaller({
        args: ["--no-setup", "--no-man"],
        env: { CTX_SEARCH_SEMANTIC: value },
      });
    try {
      assert.equal(enabled.result.status, 0, `${JSON.stringify(value)}: ${enabled.result.stderr}`);
      assert.doesNotMatch(enabled.result.stderr, /semantic:|runtime\/model provisioning/);
      assert.equal(existsSync(path.join(enabled.semanticRoot, "runtime.installed")), true);
    } finally {
      enabled.cleanup();
    }
  }

  const whitespace = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
    env: { CTX_SEARCH_SEMANTIC: "\u00a0" },
  });
  try {
    assert.equal(whitespace.result.status, 0, whitespace.result.stderr);
    assert.doesNotMatch(whitespace.result.stderr, /semantic:|runtime\/model provisioning/);
    assert.equal(existsSync(whitespace.semanticRoot), false);
  } finally {
    whitespace.cleanup();
  }
});

test("rendered CLI installer explicit Semantic false overrides persisted true", () => {
  const {
    result,
    cleanup,
    runtimeRepairLogPath,
    semanticRoot,
    setupArgsPath,
    configPath,
  } = runRenderedCliInstaller({
    args: ["--no-daemon", "--no-skill", "--no-man"],
    env: {
      CTX_FAKE_PERSIST_HOSTED_SEMANTIC: "1",
      CTX_SEARCH_SEMANTIC: "false",
    },
    semanticConfig: true,
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.doesNotMatch(result.stderr, /semantic:|runtime\/model provisioning/);
    assert.equal(existsSync(runtimeRepairLogPath), false);
    assert.equal(existsSync(semanticRoot), false);
    assert.equal(
      readFileSync(setupArgsPath, "utf8"),
      "setup\n--quiet\n--format\njson\n--progress\nnone\n--no-daemon\n",
    );
    assert.equal(readFileSync(configPath, "utf8"), "[search]\nsemantic = false\n");
  } finally {
    cleanup();
  }
});

test("rendered CLI installer fails closed before setup when Semantic repair fails", () => {
  const {
    result,
    cleanup,
    runtimeRepairLogPath,
    semanticRoot,
    setupArgsPath,
  } = runRenderedCliInstaller({
    args: ["--semantic", "--no-skill", "--no-man"],
    env: { CTX_FAKE_RUNTIME_REPAIR_STATUS: "73" },
  });
  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /ctx Semantic runtime repair failed/);
    assert.equal(existsSync(runtimeRepairLogPath), true);
    assert.equal(existsSync(semanticRoot), false);
    assert.equal(
      readFileSync(setupArgsPath, "utf8"),
      "upgrade\n--channel\nstable\n--format=json\n",
    );
  } finally {
    cleanup();
  }
});

test("rendered CLI installer falls back to the raw artifact when gzip is unavailable", () => {
  const { result, cleanup, installBin } = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
    compressedArtifact: "missing",
    env: { CTX_FAKE_GZIP_PROBE_NOISE: "1" },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.doesNotMatch(result.stderr, /optional gzip artifact is unavailable/);
    assert.doesNotMatch(result.stderr, /Downloaded gzip-compressed artifact/);
    assert.equal(existsSync(path.join(installBin, "ctx")), true);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer fails closed for a corrupt gzip sidecar", () => {
  const { result, cleanup, installBin } = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
    compressedArtifact: "corrupt",
  });
  try {
    assert.equal(result.status, 1);
    assert.match(result.stderr, /could not decompress release artifact/);
    assert.equal(existsSync(path.join(installBin, "ctx")), false);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer honors install attempt override for tests", () => {
  const { result, cleanup, installStageLogPath, installBin } = runRenderedCliInstaller({
    args: ["--no-setup"],
    env: { CTX_INSTALL_ATTEMPT_ID: "ia_test_attempt_123" },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    const marker = JSON.parse(readFileSync(path.join(installBin, "ctx.install.json"), "utf8"));
    assert.equal(marker.install_attempt_id, "ia_test_attempt_123");
    const reports = readStageReports(installStageLogPath);
    assert.ok(reports.length > 0);
    assert.ok(reports.every((report) => report.install_attempt_id === "ia_test_attempt_123"));
    assert.deepEqual(
      [reports.at(-2).stage, reports.at(-2).status, reports.at(-1).stage, reports.at(-1).status],
      ["setup", "skipped", "installer", "completed"],
    );
  } finally {
    cleanup();
  }
});

test("rendered CLI installer never sends an invalid attempt override", () => {
  const unsafeAttemptId = "ia_/home/alice/private path";
  const { result, cleanup, installStageLogPath } = runRenderedCliInstaller({
    args: ["--no-setup"],
    env: { CTX_INSTALL_ATTEMPT_ID: unsafeAttemptId },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    const reports = readStageReports(installStageLogPath);
    assert.ok(reports.length > 0);
    assert.ok(reports.every((report) => /^ia_[A-Za-z0-9_-]{8,128}$/.test(report.install_attempt_id)));
    assert.doesNotMatch(JSON.stringify(reports), /alice|private path/);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer normalizes canonical analytics opt-outs", () => {
  for (const value of [" 0 ", " false ", "\tNO\n", " oFf "]) {
    const { result, cleanup, installStageLogPath } = runRenderedCliInstaller({
      args: ["--no-setup"],
      env: { CTX_ANALYTICS_ENABLED: value },
    });
    try {
      assert.equal(result.status, 0, `${JSON.stringify(value)}: ${result.stderr}`);
      assert.equal(
        existsSync(installStageLogPath),
        false,
        `${JSON.stringify(value)} must disable installer diagnostics`,
      );
    } finally {
      cleanup();
    }
  }
});

test("rendered CLI installer keeps diagnostics enabled for canonical non-opt-outs", () => {
  for (const value of ["", "   ", " true ", " YES ", " on ", "invalid"]) {
    const { result, cleanup, installStageLogPath } = runRenderedCliInstaller({
      args: ["--no-setup"],
      env: { CTX_ANALYTICS_ENABLED: value },
    });
    try {
      assert.equal(result.status, 0, `${JSON.stringify(value)}: ${result.stderr}`);
      assert.ok(
        readStageReports(installStageLogPath).length > 0,
        `${JSON.stringify(value)} must retain default-enabled installer diagnostics`,
      );
    } finally {
      cleanup();
    }
  }
});

test("rendered CLI installer translates and clears deprecated controls", () => {
  const { result, cleanup, installStageLogPath, setupEnvPath } = runRenderedCliInstaller({
    env: {
      CTX_ANALYTICS_ENABLED: "true",
      CTX_INSTALL_DIAGNOSTICS_OFF: " yes ",
      CTX_DAEMON_ENABLED: "true",
      CTX_DAEMON_OFF: "anything",
      CTX_UPGRADE_AUTO: "apply",
      CTX_DISABLE_AUTO_UPGRADE: "ON",
    },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.equal(
      result.stderr.match(/deprecated environment variables detected/g)?.length,
      1,
      result.stderr,
    );
    assert.match(result.stderr, /CTX_INSTALL_DIAGNOSTICS_OFF -> CTX_ANALYTICS_ENABLED=false/);
    assert.match(result.stderr, /CTX_DAEMON_OFF -> CTX_DAEMON_ENABLED=false/);
    assert.match(result.stderr, /CTX_DISABLE_AUTO_UPGRADE -> CTX_UPGRADE_AUTO=off/);
    assert.equal(existsSync(installStageLogPath), false, "privacy opt-out must precede diagnostics");
    for (const line of readFileSync(setupEnvPath, "utf8").trim().split("\n")) {
      assert.equal(line, "false|false|off|||||||", line);
    }
  } finally {
    cleanup();
  }
});

test("setup forwards the supported nonsecret supervisor environment without ambient capture", () => {
  const supported = {
    ASTRBOT_ROOT: "/provider/astrbot",
    CLAUDE_CONFIG_DIR: "/provider/claude",
    CODEX_HOME: "/provider/codex",
    COPILOT_HOME: "/provider/copilot",
    CTX_ANALYTICS_ENABLED: "false",
    CTX_UPGRADE_AUTO: "off",
    CTX_UPGRADE_CHANNEL: "stable",
    CTX_UPGRADE_INTERVAL_SECONDS: "7200",
    FORGE_CONFIG: "/provider/forge/config.json",
    HERMES_HOME: "/provider/hermes",
    HTTPS_PROXY: "https://proxy.example.test",
    HTTP_PROXY: "http://proxy.example.test",
    MIMOCODE_CONFIG_DIR: "/provider/mimocode",
    NO_PROXY: "localhost,127.0.0.1",
    SSL_CERT_DIR: "/trust/certs",
    SSL_CERT_FILE: "/trust/ca.pem",
    XDG_CONFIG_HOME: "/provider/xdg-config",
  };
  const fixture = runRenderedCliInstaller({
    env: {
      ...supported,
      AWS_SECRET_ACCESS_KEY: "must-not-be-captured",
      CTX_TEST_AMBIENT_SECRET: "must-not-be-captured",
    },
  });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    const forwarded = readFileSync(fixture.supervisorEnvPath, "utf8")
      .trim()
      .split("\n");
    assert.deepEqual(
      forwarded,
      Object.entries(supported).map(([name, value]) => `${name}=${value}`),
    );
    assert.doesNotMatch(
      forwarded.join("\n"),
      /AWS_SECRET_ACCESS_KEY|CTX_TEST_AMBIENT_SECRET|must-not-be-captured/,
    );
  } finally {
    fixture.cleanup();
  }
});

test("rendered CLI installer warns on inactive deprecated control presence", () => {
  const { result, cleanup, installStageLogPath } = runRenderedCliInstaller({
    args: ["--no-setup"],
    env: {
      CTX_ANALYTICS_ENABLED: "true",
      CTX_ANALYTICS_OFF: " false ",
    },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stderr, /CTX_ANALYTICS_OFF -> CTX_ANALYTICS_ENABLED=false/);
    assert.ok(readStageReports(installStageLogPath).length > 0);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer emits only the install_stage@1 field allowlist", () => {
  const { result, cleanup, installStageLogPath } = runRenderedCliInstaller({
    args: ["--no-setup"],
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    const reports = readStageReports(installStageLogPath);
    assert.ok(reports.length > 0);
    for (const report of reports) {
      assert.deepEqual(Object.keys(report).sort(), INSTALL_STAGE_PAYLOAD_KEYS);
      assert.ok(INSTALL_STAGES.includes(report.stage));
      assert.ok(INSTALL_STAGE_STATUSES.includes(report.status));
      assert.ok(INSTALL_SCRIPT_FAMILIES.includes(report.script_family));
    }
  } finally {
    cleanup();
  }
});

test("rendered CLI installer latches diagnostics off after the first failed POST", () => {
  const { result, cleanup, installStageLogPath, installBin } = runRenderedCliInstaller({
    args: ["--no-setup"],
    env: { CTX_FAKE_INSTALL_STAGE_STATUS: "66" },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.equal(
      result.stderr,
      `Installing ctx 9.9.9...\nInstalled and verified\n\nTo use the newly installed ctx in this shell, run:\n  export PATH="${installBin}:$PATH"\n\nNew terminal sessions will include it automatically.\n`,
    );
    assert.deepEqual(readStageReports(installStageLogPath).map(({ stage, status }) => [stage, status]), [
      ["installer", "started"],
    ]);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer reports a content-free terminal failure", () => {
  const { result, cleanup, installStageLogPath } = runRenderedCliInstaller({
    args: ["--no-setup"],
    metadataChecksum: "1111111111111111111111111111111111111111111111111111111111111111",
  });
  try {
    assert.equal(result.status, 1);
    assert.match(result.stderr, /checksum mismatch/);
    const reports = readStageReports(installStageLogPath);
    assert.equal(reports.at(-1).stage, "installer");
    assert.equal(reports.at(-1).status, "failed");
    assert.deepEqual(Object.keys(reports.at(-1)).sort(), INSTALL_STAGE_PAYLOAD_KEYS);
    assert.doesNotMatch(JSON.stringify(reports), /checksum|error|path|command|user/i);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer fails closed without metadata signature", () => {
  const { result, cleanup } = runRenderedCliInstaller({
    args: ["--no-setup"],
    includeMetadataSignature: false,
  });
  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /metadata\.env\.sig|CTX_FAKE_METADATA_SIGNATURE/);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer rejects placeholder checksums", () => {
  const { result, cleanup } = runRenderedCliInstaller({
    args: ["--no-setup"],
    metadataChecksum: "0000000000000000000000000000000000000000000000000000000000000000",
  });
  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /checksum for linux-x64 is a placeholder/);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer rejects metadata channel mismatch", () => {
  const { result, cleanup } = runRenderedCliInstaller({
    args: ["--no-setup"],
    metadataChannel: "beta",
  });
  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /metadata channel beta does not match requested channel stable/);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer can skip setup", () => {
  const { result, cleanup, setupArgsPath } = runRenderedCliInstaller({ args: ["--no-setup"] });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.doesNotMatch(result.stderr, /skill|Setup skipped/);
    assert.match(readFileSync(setupArgsPath, "utf8"), /^docs\nman\n--out\n.*\/generated-man\n$/u);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer honors explicit skill target with setup skipped", () => {
  const { result, cleanup, setupArgsPath } = runRenderedCliInstaller({
    args: ["--no-setup", "--skill-agent", "codex"],
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.doesNotMatch(result.stderr, /skill:|Setup skipped/);
    assert.match(
      readFileSync(setupArgsPath, "utf8"),
      /^docs\nman\n--out\n.*\/generated-man\nintegrations\ninstall\nskills\n--agent\ncodex\n--format=json\n$/u,
    );
  } finally {
    cleanup();
  }
});

test("rendered CLI installer can skip only skill install", () => {
  const { result, cleanup, setupArgsPath } = runRenderedCliInstaller({
    args: ["--no-skill"],
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.doesNotMatch(result.stderr, /Agent skill skipped|Indexing local agent history/);
    assert.match(
      readFileSync(setupArgsPath, "utf8"),
      /^docs\nman\n--out\n.*\/generated-man\nsetup\n--quiet\n--format\njson\n--wait\n--progress\nnone\n$/u,
    );
  } finally {
    cleanup();
  }
});

test("rendered CLI installer routes --no-daemon exactly to setup", () => {
  const { result, cleanup, setupArgsPath } = runRenderedCliInstaller({
    args: ["--no-daemon", "--no-skill", "--no-man", "--no-modify-path"],
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.equal(
      readFileSync(setupArgsPath, "utf8"),
      "setup\n--quiet\n--format\njson\n--progress\nnone\n--no-daemon\n",
    );
  } finally {
    cleanup();
  }
});

test("POSIX status validation recognizes bounded complete UTF-8 JSON documents", () => {
  const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-json-validation-"));
  const validatorPath = path.join(sandbox, "validate.sh");
  const documentPath = path.join(sandbox, "status.json");
  writeExecutable(
    validatorPath,
    `#!/bin/sh
${CLI_INSTALL_SHELL_VALIDATION}
json_document_is_well_formed "$1"
`,
  );

  const invalidUtf8 = (bytes) => Buffer.concat([
    Buffer.from('{"note":"', "utf8"),
    Buffer.from(bytes),
    Buffer.from('"}\n', "utf8"),
  ]);
  const cases = [
    {
      name: "Unicode scalar and unescaped DEL",
      document: Buffer.from('{"note":"snowman ☃\u007f"}\n', "utf8"),
      valid: true,
    },
    {
      name: "escaped surrogate pair",
      document: '{"note":"\\uD83D\\uDE03"}\n',
      valid: true,
    },
    {
      name: "lone high surrogate escape",
      document: '{"note":"\\uD83D"}\n',
      valid: false,
    },
    {
      name: "lone low surrogate escape",
      document: '{"note":"\\uDE03"}\n',
      valid: false,
    },
    { name: "missing comma", document: '{"a":1 "b":2}\n', valid: false },
    { name: "trailing JSON document", document: '{"a":1}\n{"b":2}\n', valid: false },
    { name: "embedded JSON value", document: '{"a":[true false]}\n', valid: false },
    { name: "complete JSON number", document: '{"a":-1.25e+3}\n', valid: true },
    { name: "leading-zero number", document: '{"a":01}\n', valid: false },
    { name: "incomplete fraction", document: '{"a":1.}\n', valid: false },
    { name: "incomplete exponent", document: '{"a":1e+}\n', valid: false },
    { name: "invalid string escape", document: '{"a":"\\x"}\n', valid: false },
    { name: "raw NUL", document: invalidUtf8([0x00]), valid: false },
    { name: "raw control byte", document: invalidUtf8([0x01]), valid: false },
    { name: "overlong UTF-8", document: invalidUtf8([0xc0, 0xaf]), valid: false },
    { name: "UTF-8 surrogate", document: invalidUtf8([0xed, 0xa0, 0x80]), valid: false },
    { name: "out-of-range UTF-8", document: invalidUtf8([0xf4, 0x90, 0x80, 0x80]), valid: false },
    {
      name: "excessive nesting",
      document: `${"[".repeat(66)}0${"]".repeat(66)}\n`,
      valid: false,
    },
    {
      name: "bounded multiline document",
      document: `${" \n".repeat(1113)}0\n`,
      valid: true,
    },
    {
      name: "excessive record count",
      document: `${" \n".repeat(4097)}0\n`,
      valid: false,
    },
    { name: "oversized document", document: Buffer.alloc(1048577, 0x20), valid: false },
  ];

  try {
    for (const testCase of cases) {
      writeFileSync(documentPath, testCase.document);
      const result = spawnSync(validatorPath, [documentPath], { encoding: "utf8" });
      assert.equal(
        result.status,
        testCase.valid ? 0 : 1,
        `${testCase.name}: ${installerOutput(result)}`,
      );
    }
  } finally {
    rmSync(sandbox, { recursive: true, force: true });
  }
});

test("rendered CLI installer accepts the native empty receipt after explicit no-daemon setup", () => {
  for (const testCase of [
    {
      name: "flag",
      args: ["--no-daemon", "--no-skill", "--no-man", "--no-modify-path"],
    },
    {
      name: "environment",
      args: ["--no-skill", "--no-man", "--no-modify-path"],
      env: { CTX_INSTALL_NO_DAEMON: "1" },
    },
  ]) {
    const fixture = runRenderedCliInstaller({
      args: testCase.args,
      env: { CTX_FAKE_INITIALIZED: "false", ...testCase.env },
    });
    try {
      assert.equal(fixture.result.status, 0, `${testCase.name}: ${fixture.result.stderr}`);
      assert.deepEqual(readCtxCommands(fixture.commandLogPath), [
        "setup --quiet --format json --progress none --no-daemon",
      ], testCase.name);
      assert.match(fixture.result.stderr, /Indexing deferred — daemon not started/);
      assert.doesNotMatch(fixture.result.stderr, /Indexing started|Indexing will continue/);
    } finally {
      fixture.cleanup();
    }
  }
});

test("rendered CLI installer reuses but does not start a consistent running daemon", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-daemon", "--no-skill", "--no-man", "--no-modify-path"],
    env: {
      CTX_FAKE_INITIALIZED: "true",
      CTX_FAKE_SETUP_MODE: "ready",
      CTX_FAKE_INDEXED_SESSIONS: "3",
      CTX_FAKE_INDEXED_ITEMS: "30",
      CTX_FAKE_DAEMON_STATUS: "running",
      CTX_FAKE_DAEMON_ENABLED: "true",
      CTX_FAKE_DAEMON_RUNNING: "true",
    },
  });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    assert.deepEqual(readCtxCommands(fixture.commandLogPath), [
      "setup --quiet --format json --progress none --no-daemon",
    ]);
    assert.match(fixture.result.stderr, /\nIndex ready\n/);
    assert.doesNotMatch(fixture.result.stderr, /daemon disabled|daemon not started/);
  } finally {
    fixture.cleanup();
  }
});

test("rendered CLI installer does not reinterpret daemon lifecycle fields", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-daemon", "--no-skill", "--no-man", "--no-modify-path"],
    env: {
      CTX_FAKE_SETUP_RECEIPT: JSON.stringify({
        schema_version: 2,
        initialized: true,
        mode: "ready",
        indexed_sessions: 3,
        indexed_items: 30,
        daemon: { status: "unknown", enabled: false, running: false },
      }, null, 2),
    },
  });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    assert.deepEqual(readCtxCommands(fixture.commandLogPath), [
      "setup --quiet --format json --progress none --no-daemon",
    ]);
    assert.match(fixture.result.stderr, /Index ready/);
  } finally {
    fixture.cleanup();
  }
});

test("rendered CLI installer rejects Semantic with any effective daemon opt-out before mutation", () => {
  const cases = [
    {
      name: "installer flag wins canonical enable",
      args: ["--semantic", "--no-daemon"],
      env: { CTX_DAEMON_ENABLED: "true" },
    },
    {
      name: "installer environment",
      env: {
        CTX_INSTALL_NO_DAEMON: "1",
        CTX_SEARCH_SEMANTIC: "true",
      },
    },
    {
      name: "canonical daemon false",
      env: {
        CTX_INSTALL_SEMANTIC: "true",
        CTX_DAEMON_ENABLED: ' "FaLsE" ',
      },
      daemonConfig: true,
    },
    {
      name: "deprecated daemon off wins canonical enable",
      env: {
        CTX_SEARCH_SEMANTIC: "true",
        CTX_DAEMON_ENABLED: "true",
        CTX_DAEMON_OFF: "anything",
      },
    },
    {
      name: "deprecated disable daemon",
      env: {
        CTX_DISABLE_DAEMON: " yes ",
      },
      semanticConfig: true,
    },
    {
      name: "persisted daemon opt-out wins canonical enable",
      env: { CTX_DAEMON_ENABLED: "true" },
      semanticConfig: true,
      daemonConfig: false,
    },
    {
      name: "public-valid spaced section headers",
      args: ["--semantic"],
      daemonConfig: false,
      spacedConfigSections: true,
    },
    {
      name: "root dotted search and daemon keys",
      rawConfig: "search.semantic = true\ndaemon.enabled = false\n",
    },
    {
      name: "canonical manual indexing overrides legacy daemon enable",
      rawConfig: '[search]\nsemantic = true\n[daemon]\nenabled = true\n[indexing]\nmode = "manual"\n',
    },
  ];

  for (const testCase of cases) {
    const {
      result,
      cleanup,
      setupArgsPath,
      runtimeRepairLogPath,
      semanticRoot,
      installStageLogPath,
      installBin,
      configPath,
      configContents,
      installerTmpRoot,
    } = runRenderedCliInstaller(testCase);
    try {
      assert.notEqual(result.status, 0, `${testCase.name}: installer unexpectedly succeeded`);
      assert.match(
        result.stderr,
        /Semantic installation requires an enabled daemon/,
      );
      assert.equal(existsSync(path.join(installBin, "ctx")), false, testCase.name);
      assert.equal(existsSync(path.join(installBin, "ctx.install.json")), false, testCase.name);
      assert.equal(existsSync(runtimeRepairLogPath), false, testCase.name);
      assert.equal(existsSync(semanticRoot), false, testCase.name);
      assert.equal(existsSync(setupArgsPath), false, testCase.name);
      assert.equal(existsSync(installStageLogPath), false, testCase.name);
      assert.deepEqual(readdirSync(installerTmpRoot), [], testCase.name);
      if (configContents) {
        assert.equal(readFileSync(configPath, "utf8"), configContents);
      } else {
        assert.equal(existsSync(configPath), false, testCase.name);
      }
    } finally {
      cleanup();
    }
  }
});

test("rendered CLI installer rejects malformed config and invalid installer controls before mutation", () => {
  const cases = [
    {
      name: "root dotted and section Semantic key",
      rawConfig: "search.semantic = true\n[search]\nsemantic = false\n",
      error: /duplicate config key `search\.semantic` at line 3; first set at line 1/,
    },
    {
      name: "normalized repeated daemon section",
      rawConfig: "[daemon]\nenabled = false\n[ daemon ]\nenabled = true\n",
      error: /duplicate config key `daemon\.enabled` at line 4; first set at line 2/,
    },
    {
      name: "unrelated public config key",
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
    { name: "invalid indexing mode", rawConfig: "[indexing]\nmode = \"on-demand\"\n", error: /indexing\.mode at line 2 must be either/ },
    {
      name: "invalid UTF-8 hidden in comment",
      rawConfig: Buffer.concat([
        Buffer.from("[search]\nsemantic = true # ", "utf8"),
        Buffer.from([0xff]),
        Buffer.from("\n", "utf8"),
      ]),
      error: /persisted config is not valid UTF-8/,
    },
    {
      name: "overlong UTF-8",
      rawConfig: Buffer.concat([
        Buffer.from("[search]\nsemantic = true # ", "utf8"),
        Buffer.from([0xc0, 0x80]),
        Buffer.from("\n", "utf8"),
      ]),
      error: /persisted config is not valid UTF-8/,
    },
    {
      name: "UTF-8 surrogate",
      rawConfig: Buffer.concat([
        Buffer.from("[search]\nsemantic = true # ", "utf8"),
        Buffer.from([0xed, 0xa0, 0x80]),
        Buffer.from("\n", "utf8"),
      ]),
      error: /persisted config is not valid UTF-8/,
    },
    {
      name: "UTF-8 code point beyond Unicode",
      rawConfig: Buffer.concat([
        Buffer.from("[search]\nsemantic = true # ", "utf8"),
        Buffer.from([0xf4, 0x90, 0x80, 0x80]),
        Buffer.from("\n", "utf8"),
      ]),
      error: /persisted config is not valid UTF-8/,
    },
    {
      name: "truncated UTF-8",
      rawConfig: Buffer.concat([
        Buffer.from("[search]\nsemantic = true # ", "utf8"),
        Buffer.from([0xe2, 0x82]),
      ]),
      error: /persisted config is not valid UTF-8/,
    },
  ];

  for (const testCase of cases) {
    const {
      result,
      cleanup,
      setupArgsPath,
      runtimeRepairLogPath,
      semanticRoot,
      installStageLogPath,
      installBin,
      manDir,
      configPath,
      configContents,
      installerTmpRoot,
    } = runRenderedCliInstaller(testCase);
    try {
      assert.notEqual(result.status, 0, `${testCase.name}: installer unexpectedly succeeded`);
      assert.match(result.stderr, testCase.error);
      assert.deepEqual(readdirSync(installBin), [], testCase.name);
      assert.deepEqual(readdirSync(manDir), [], testCase.name);
      assert.equal(existsSync(runtimeRepairLogPath), false, testCase.name);
      assert.equal(existsSync(semanticRoot), false, testCase.name);
      assert.equal(existsSync(setupArgsPath), false, testCase.name);
      assert.equal(existsSync(installStageLogPath), false, testCase.name);
      assert.deepEqual(readdirSync(installerTmpRoot), [], testCase.name);
      if (Buffer.isBuffer(configContents)) {
        assert.deepEqual(readFileSync(configPath), configContents, testCase.name);
      } else {
        assert.equal(readFileSync(configPath, "utf8"), configContents, testCase.name);
      }
    } finally {
      cleanup();
    }
  }
});

test("rendered CLI installer accepts public-valid comments, strings, and unsigned intervals", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
    rawConfig: [
      '[analytics]\nenabled = false\nendpoint = "https://example.test/#anchor" # retained hash is inside the string',
      "[local_usage]\nenabled = false",
      '[upgrade]\nauto = "APPLY"\nchannel = "stable"\ninterval_hours = +24',
      '[daemon]\nenabled = false\nmode = "source-refresh-only"',
      '[indexing]\nmode = "automatic"\n[search]\nsemantic = true\n',
    ].join("\n"),
  });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    assert.doesNotMatch(fixture.result.stderr, /semantic:|runtime\/model provisioning/);
    assert.equal(existsSync(path.join(fixture.semanticRoot, "runtime.installed")), true);
  } finally {
    fixture.cleanup();
  }
});

test("rendered CLI installer routes CTX_INSTALL_NO_DAEMON exactly to setup", () => {
  const { result, cleanup, setupArgsPath } = runRenderedCliInstaller({
    args: ["--no-skill", "--no-man", "--no-modify-path"],
    env: { CTX_INSTALL_NO_DAEMON: "1" },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.equal(
      readFileSync(setupArgsPath, "utf8"),
      "setup\n--quiet\n--format\njson\n--progress\nnone\n--no-daemon\n",
    );
  } finally {
    cleanup();
  }
});

test("rendered CLI installer can skip generated man pages", () => {
  const { result, cleanup, setupArgsPath } = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.doesNotMatch(result.stderr, /skill|Setup skipped/);
    assert.equal(existsSync(setupArgsPath), false);
  } finally {
    cleanup();
  }
});
