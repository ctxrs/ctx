import "./cli-install-bridge-tests.mjs";
import "./cli-install-managed-pair-contract-tests.mjs";
import {
  assert,
  chmodSync,
  statSync,
  utilLinuxScriptCommand,
  existsSync,
  installerOutput,
  mkdirSync,
  path,
  readCtxCommands,
  readFileSync,
  readdirSync,
  rmSync,
  runHostedUninstallForInstallerFixture,
  runRenderedCliInstaller,
  sha256,
  test,
  writeExecutable,
  writeFileSync,
} from "./cli-install-test-helpers.mjs";

function integrationGenerationNames(installBin) {
  return readdirSync(installBin).filter((name) => name.startsWith(
    "ctx.install-integrations.",
  ));
}

function pairInvocationPaths(fixture) {
  return readFileSync(fixture.pairInvocationPathLogPath, "utf8")
    .trim()
    .split("\n")
    .map((line) => line.split("\t"));
}

test("fresh hosted managed pair invokes the candidate Core exactly once", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man", "--no-skill", "--no-modify-path"],
    managedPair: true, releaseVersion: "1.4.12",
  });
  try {
    assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
    assert.ok(existsSync(path.join(fixture.installBin, "ctx")));
    assert.ok(existsSync(path.join(fixture.installBin, "..", "libexec", "ctx-pro")));
    assert.ok(existsSync(path.join(
      fixture.installBin,
      "..",
      "share",
      "ctx",
      "managed-pair-state.json",
    )));
    const preCommitMarker = JSON.parse(readFileSync(fixture.pairMarkerSourceLogPath, "utf8"));
    assert.equal(Object.hasOwn(preCommitMarker, "integrations_path"), false);
    assert.equal(Object.hasOwn(preCommitMarker, "integrations_sha256"), false);
    const committedMarker = JSON.parse(readFileSync(
      path.join(fixture.installBin, "ctx.install.json"),
      "utf8",
    ));
    assert.match(committedMarker.integrations_sha256, /^[0-9a-f]{64}$/u);
    assert.equal(
      committedMarker.integrations_path,
      path.join(
        fixture.installBin,
        `ctx.install-integrations.${committedMarker.integrations_sha256}`,
      ),
    );
    assert.ok(existsSync(committedMarker.integrations_path));
    const freshCommands = readCtxCommands(fixture.commandLogPath);
    const [[, envelopePath, markerSourcePath], [, integrationSourcePath]] =
      pairInvocationPaths(fixture);
    const pairRoot = path.dirname(fixture.installBin);
    const applyTmp = path.dirname(envelopePath);
    const applyCommand = `--ctx-core-managed-pair-apply-v1 ${pairRoot} - ${envelopePath} ${path.join(applyTmp, "ctx")} ${path.join(applyTmp, "ctx-pro")} ${markerSourcePath}`;
    const reconcileCommand = `--ctx-core-managed-pair-reconcile-integration-v1 ${pairRoot} - ${integrationSourcePath}`;
    assert.deepEqual(freshCommands, [
      applyCommand,
      reconcileCommand,
    ]);
    assert.ok(
      freshCommands[0].startsWith(
        `--ctx-core-managed-pair-apply-v1 ${path.dirname(fixture.installBin)} - `,
      ),
    );
    assert.match(
      freshCommands[0],
      /managed-pair-envelope\.json .*\/ctx .*ctx-pro .*install-marker/u,
    );
    assert.doesNotMatch(
      readFileSync(fixture.commandLogPath, "utf8"),
      /--hosted-transaction|--ctx-core-hosted-pair-install-v1/u,
    );
    const rerun = fixture.rerun();
    assert.equal(rerun.status, 0, installerOutput(rerun));
    const allCommands = readCtxCommands(fixture.commandLogPath);
    const [, , [, rerunIntegrationSourcePath]] = pairInvocationPaths(fixture);
    assert.deepEqual(allCommands, [
      applyCommand,
      reconcileCommand,
      "--ctx-core-disable-managed-man-pages-v1",
      "upgrade --channel stable --format=json",
      `--ctx-core-managed-pair-reconcile-integration-v1 ${pairRoot} - ${rerunIntegrationSourcePath}`,
    ]);
    assert.doesNotMatch(
      readFileSync(fixture.commandLogPath, "utf8"),
      /--hosted-transaction|--ctx-core-hosted-pair-install-v1/u,
    );
  } finally {
    fixture.cleanup();
  }
});

test("warningful managed-pair success stays successful and keeps stdout clean", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man", "--no-skill", "--no-modify-path"],
    env: {
      CTX_FAKE_PAIR_INSTALL_RECEIPT: "warning",
      CTX_FAKE_PAIR_RECONCILE_RECEIPT: "warning",
    },
    managedPair: true, releaseVersion: "1.4.12",
  });
  try {
    assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
    assert.match(fixture.result.stderr, /warning: sidecar retry remains pending/u);
    assert.match(fixture.result.stderr, /warning: reconciliation cleanup remains pending/u);
    assert.equal(fixture.result.stdout, "");
  } finally {
    fixture.cleanup();
  }
});

test("managed-pair reconciliation failure preserves installed identity and retries safely", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-setup", "--skill-agent", "codex", "--no-man", "--no-modify-path"],
    env: {
      CTX_FAKE_PAIR_RECONCILE_ERROR: "Bearer private-token",
      CTX_FAKE_PAIR_RECONCILE_STATUS: "70",
    },
    managedPair: true, releaseVersion: "1.4.12",
  });
  const installPath = path.join(fixture.installBin, "ctx");
  const markerPath = `${installPath}.install.json`;
  const integrationPath = `${installPath}.install-integrations`;
  const skillPath = path.join(
    fixture.homeDir,
    ".agents",
    "skills",
    "ctx-agent-history-search",
  );
  try {
    assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
    assert.match(
      fixture.result.stderr,
      /integration ownership reconciliation is pending/u,
    );
    assert.match(fixture.result.stderr, /Bearer <redacted>/u);
    assert.doesNotMatch(fixture.result.stderr, /private-token/u);
    assert.equal(fixture.result.stdout, "");
    const interruptedMarker = JSON.parse(readFileSync(markerPath, "utf8"));
    assert.equal(Object.hasOwn(interruptedMarker, "integrations_path"), false);
    assert.equal(existsSync(integrationPath), false);
    assert.deepEqual(integrationGenerationNames(fixture.installBin), []);

    const retry = fixture.rerun([
      "--no-setup",
      "--skill-agent",
      "codex",
      "--no-modify-path",
    ], {
      CTX_FAKE_PAIR_RECONCILE_STATUS: "0",
    });
    assert.equal(retry.status, 0, installerOutput(retry));
    const marker = JSON.parse(readFileSync(markerPath, "utf8"));
    assert.equal(marker.integrations_path, `${integrationPath}.${marker.integrations_sha256}`);
    assert.equal(marker.integrations_sha256, sha256(readFileSync(marker.integrations_path)));
    assert.deepEqual(integrationGenerationNames(fixture.installBin), [
      path.basename(marker.integrations_path),
    ]);

    const uninstall = runHostedUninstallForInstallerFixture(fixture);
    assert.equal(uninstall.status, 0, installerOutput(uninstall));
    assert.equal(existsSync(path.join(skillPath, "SKILL.md")), false);
  } finally {
    fixture.cleanup();
  }
});

test("installer rerun lets candidate Core finish a marker-first interrupted pair", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man", "--no-skill", "--no-modify-path"],
    managedPair: true, releaseVersion: "1.4.12",
    prepareInstall: ({ installBin, sandbox }) => {
      const installPath = path.join(installBin, "ctx");
      const artifact = readFileSync(path.join(sandbox, "ctx-artifact"));
      writeFileSync(`${installPath}.install.json`, `${JSON.stringify({
        schema_version: 1,
        manager: "ctx-hosted-installer",
        install_path: installPath,
        platform: "linux-x64",
        channel: "stable",
        version: "1.4.12",
        sha256: sha256(artifact),
      }, null, 2)}\n`);
      writeFileSync(path.join(installBin, ".ctx.upgrade-install-transaction.json"), "pending\n");
    },
  });
  try {
    assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
    const commands = readCtxCommands(fixture.commandLogPath);
    const [[, envelopePath, markerSourcePath], [, integrationSourcePath]] =
      pairInvocationPaths(fixture);
    const pairRoot = path.dirname(fixture.installBin);
    const applyTmp = path.dirname(envelopePath);
    assert.deepEqual(commands, [
      `--ctx-core-managed-pair-apply-v1 ${pairRoot} - ${envelopePath} ${path.join(applyTmp, "ctx")} ${path.join(applyTmp, "ctx-pro")} ${markerSourcePath}`,
      "upgrade --channel stable --format=json",
      `--ctx-core-managed-pair-reconcile-integration-v1 ${pairRoot} - ${integrationSourcePath}`,
    ]);
  } finally {
    fixture.cleanup();
  }
});

test("v0.25 routing fixture crosses B and retries final Semantic without reapplying B", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-setup", "--no-skill", "--no-modify-path"],
    managedPair: true, releaseVersion: "1.4.12",
  });
  const installPath = path.join(fixture.installBin, "ctx");
  const markerPath = `${installPath}.install.json`;
  try {
    assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
    const releasedV025Core = "#!/bin/sh\nprintf '%s\\n' 'released v0.25 Core must not be invoked' >&2\nexit 97\n";
    writeExecutable(installPath, releasedV025Core);
    const marker = JSON.parse(readFileSync(markerPath, "utf8"));
    const priorManPages = marker.man_pages;
    const priorIntegrationPath = marker.integrations_path;
    marker.version = "0.25.0";
    marker.sha256 = sha256(releasedV025Core);
    writeFileSync(markerPath, `${JSON.stringify(marker, null, 2)}\n`);
    rmSync(path.join(fixture.installBin, "..", "libexec", "ctx-pro"));
    rmSync(path.join(fixture.installBin, "..", "share", "ctx"), {
      recursive: true,
      force: true,
    });
    writeFileSync(fixture.commandLogPath, "");
    writeFileSync(fixture.pairInvocationPathLogPath, "");

    const retryArgs = ["--semantic", "--no-setup", "--no-skill", "--no-modify-path"];
    const interrupted = fixture.rerun(retryArgs, {
      CTX_FAKE_RUNTIME_REPAIR_STATUS: "77",
    });
    assert.notEqual(interrupted.status, 0);
    assert.match(interrupted.stderr, /ctx Semantic runtime repair failed/u);
    const candidateMarkerSource = JSON.parse(readFileSync(
      fixture.pairMarkerSourceLogPath,
      "utf8",
    ));
    const candidateMarker = JSON.parse(readFileSync(markerPath, "utf8"));
    assert.equal(candidateMarkerSource.version, "1.3.2");
    assert.equal(candidateMarker.version, "1.4.12");
    assert.equal(candidateMarker.managed_pair, true);
    assert.equal(candidateMarkerSource.integrations_path, priorIntegrationPath);
    assert.equal(
      candidateMarkerSource.integrations_sha256,
      sha256(readFileSync(priorIntegrationPath)),
    );
    assert.deepEqual(candidateMarkerSource.man_pages, priorManPages);

    const bridge = fixture.rerun(retryArgs, {
      CTX_FAKE_RUNTIME_REPAIR_STATUS: "0",
    });
    assert.equal(bridge.status, 0, installerOutput(bridge));
    const commands = readCtxCommands(fixture.commandLogPath);
    const [[, envelopePath, markerSourcePath], [, integrationSourcePath]] =
      pairInvocationPaths(fixture);
    const pairRoot = path.dirname(fixture.installBin);
    const applyTmp = path.dirname(envelopePath);
    assert.deepEqual(commands, [
      `--ctx-core-managed-pair-apply-v1 ${pairRoot} - ${envelopePath} ${path.join(applyTmp, "ctx")} ${path.join(applyTmp, "ctx-pro")} ${markerSourcePath}`,
      "upgrade --channel stable --format=json",
      "upgrade --channel stable --format=json",
      "upgrade --channel stable --format=json",
      "upgrade --channel stable --format=json",
      `--ctx-core-managed-pair-reconcile-integration-v1 ${pairRoot} - ${integrationSourcePath}`,
    ]);
    assert.ok(existsSync(path.join(fixture.installBin, "..", "libexec", "ctx-pro")));
    assert.ok(existsSync(path.join(
      fixture.installBin,
      "..",
      "share",
      "ctx",
      "managed-pair-state.json",
    )));
  } finally {
    fixture.cleanup();
  }
});

test("authenticated stale-marker candidate apply survives a post-sidecar retry", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-setup", "--no-skill", "--no-modify-path"],
    managedPair: true, releaseVersion: "1.4.12",
  });
  const installPath = path.join(fixture.installBin, "ctx");
  const markerPath = `${installPath}.install.json`;
  const integrationPath = `${installPath}.install-integrations`;
  const changedArgs = [
    "--no-setup",
    "--skill-agent",
    "codex",
    "--no-modify-path",
  ];
  const skillPath = path.join(
    fixture.homeDir,
    ".agents",
    "skills",
    "ctx-agent-history-search",
  );
  try {
    assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
    const markerText = readFileSync(markerPath, "utf8");
    const marker = JSON.parse(markerText);
    const priorManPages = marker.man_pages;
    const priorIntegrationPath = marker.integrations_path;
    const priorIntegrationDigest = marker.integrations_sha256;
    writeFileSync(
      markerPath,
      markerText.replace(
        `  "sha256": "${marker.sha256}",`,
        `  "sha256": "${"0".repeat(64)}",`,
      ),
    );
    writeFileSync(fixture.commandLogPath, "");
    writeFileSync(fixture.pairInvocationPathLogPath, "");

    const interrupted = fixture.rerun(changedArgs, {
      CTX_FAKE_PAIR_RECONCILE_STATUS: "70",
    });
    assert.equal(interrupted.status, 0, installerOutput(interrupted));
    assert.match(
      interrupted.stderr,
      /integration ownership reconciliation is pending/u,
    );
    const candidateMarkerSource = JSON.parse(readFileSync(
      fixture.pairMarkerSourceLogPath,
      "utf8",
    ));
    const candidateMarker = JSON.parse(readFileSync(markerPath, "utf8"));
    assert.deepEqual(candidateMarker, candidateMarkerSource);
    assert.equal(candidateMarkerSource.integrations_path, priorIntegrationPath);
    assert.equal(candidateMarkerSource.integrations_sha256, priorIntegrationDigest);
    assert.deepEqual(candidateMarkerSource.man_pages, priorManPages);
    assert.equal(sha256(readFileSync(priorIntegrationPath)), priorIntegrationDigest);
    assert.deepEqual(integrationGenerationNames(fixture.installBin), [
      path.basename(priorIntegrationPath),
    ]);

    const recovered = fixture.rerun(changedArgs, {
      CTX_FAKE_PAIR_RECONCILE_STATUS: "0",
    });
    assert.equal(recovered.status, 0, installerOutput(recovered));
    const commands = readCtxCommands(fixture.commandLogPath);
    const [
      [, envelopePath, markerSourcePath],
      [, interruptedIntegrationSourcePath],
      [, recoveredIntegrationSourcePath],
    ] = pairInvocationPaths(fixture);
    const pairRoot = path.dirname(fixture.installBin);
    const applyTmp = path.dirname(envelopePath);
    assert.deepEqual(commands, [
      `--ctx-core-managed-pair-apply-v1 ${pairRoot} - ${envelopePath} ${path.join(applyTmp, "ctx")} ${path.join(applyTmp, "ctx-pro")} ${markerSourcePath}`,
      "integrations install skills --agent codex --format=json",
      `--ctx-core-managed-pair-reconcile-integration-v1 ${pairRoot} - ${interruptedIntegrationSourcePath}`,
      "upgrade --channel stable --format=json",
      "integrations install skills --agent codex --format=json",
      `--ctx-core-managed-pair-reconcile-integration-v1 ${pairRoot} - ${recoveredIntegrationSourcePath}`,
    ]);
    const recoveredMarker = JSON.parse(readFileSync(markerPath, "utf8"));
    assert.equal(recoveredMarker.sha256, sha256(readFileSync(installPath)));
    assert.equal(
      recoveredMarker.integrations_path,
      `${integrationPath}.${recoveredMarker.integrations_sha256}`,
    );
    assert.equal(
      recoveredMarker.integrations_sha256,
      sha256(readFileSync(recoveredMarker.integrations_path)),
    );
    assert.deepEqual(integrationGenerationNames(fixture.installBin), [
      path.basename(recoveredMarker.integrations_path),
    ]);

    const uninstall = runHostedUninstallForInstallerFixture(fixture);
    assert.equal(uninstall.status, 0, installerOutput(uninstall));
    assert.equal(existsSync(path.join(skillPath, "SKILL.md")), false);
  } finally {
    fixture.cleanup();
  }
});

test("fresh managed-pair failure has no preliminary Core-only publication", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man", "--no-skill", "--no-modify-path"],
    env: {
      CTX_FAKE_PAIR_INSTALL_ERROR: `cannot replace companion: permission denied\ntoken=private-value\n${"x".repeat(9000)}`,
      CTX_FAKE_PAIR_INSTALL_STATUS: "70",
    },
    managedPair: true, releaseVersion: "1.4.12",
  });
  try {
    assert.notEqual(fixture.result.status, 0);
    assert.equal(existsSync(path.join(fixture.installBin, "ctx")), false);
    assert.match(
      fixture.result.stderr,
      /managed-pair installation did not complete; resolve the error above before retrying \(release 1\.4\.12, exit code 70\)/u,
    );
    assert.match(fixture.result.stderr, /cannot replace companion: permission denied/u);
    assert.ok(fixture.result.stderr.length < 9000);
    assert.match(fixture.result.stderr, /<redacted-credential>/u);
    assert.match(fixture.result.stderr, /ctx child output truncated/u);
    assert.doesNotMatch(fixture.result.stderr, /private-value/u);
    assert.equal(fixture.result.stdout, "");
  } finally {
    fixture.cleanup();
  }
});

test("hosted managed pair rejects an untyped candidate-Core receipt", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man", "--no-skill", "--no-modify-path"],
    env: { CTX_FAKE_PAIR_INSTALL_RECEIPT: "invalid" },
    managedPair: true, releaseVersion: "1.4.12",
  });
  try {
    assert.notEqual(fixture.result.status, 0);
    assert.ok(existsSync(path.join(fixture.installBin, "ctx")));
    assert.match(fixture.result.stderr, /invalid managed-pair apply proof/u);
  } finally {
    fixture.cleanup();
  }
});

test("hosted managed pair rejects nested transaction output beyond the success receipt", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man", "--no-skill", "--no-modify-path"],
    env: {
      CTX_FAKE_PAIR_INSTALL_ERROR: "secret=receipt-private",
      CTX_FAKE_PAIR_INSTALL_RECEIPT: "extra",
    },
    managedPair: true, releaseVersion: "1.4.12",
  });
  try {
    assert.notEqual(fixture.result.status, 0);
    assert.match(fixture.result.stderr, /invalid managed-pair apply proof/u);
    assert.match(fixture.result.stderr, /secret=<redacted-credential>/u);
    assert.doesNotMatch(fixture.result.stderr, /receipt-private/u);
  } finally {
    fixture.cleanup();
  }
});

for (const receipt of [
  "canonical-extra",
  "scalar-warning",
  "empty-warnings",
  "too-many-warnings",
  "unsafe-warning",
  "oversized",
]) {
  test(`hosted managed pair rejects ${receipt} receipt warnings`, () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--no-man", "--no-skill", "--no-modify-path"],
      env: { CTX_FAKE_PAIR_INSTALL_RECEIPT: receipt },
      managedPair: true, releaseVersion: "1.4.12",
    });
    try {
      assert.notEqual(fixture.result.status, 0);
      assert.match(fixture.result.stderr, /invalid managed-pair apply proof/u);
      assert.equal(fixture.result.stdout, "");
    } finally {
      fixture.cleanup();
    }
  });
}




// These use executable routing fixtures, not immutable released binaries.
const bridgeTestArgs = ["--no-setup", "--no-man", "--no-skill", "--no-modify-path"];
const immutableBridgeUrl = "https://cli.ctx.rs/functions/v1/releases/stable/1.3.2/ctx-release-metadata.env";
function replaceWithCoreOnlyFixture(fixture, version) {
  const executable = path.join(fixture.installBin, "ctx");
  const body = readFileSync(fixture.artifactPath, "utf8")
    .replace(/^fixture_version=[^\n]+$/mu, `fixture_version=${version}`);
  writeExecutable(executable, body);
  const marker = JSON.parse(readFileSync(`${executable}.install.json`, "utf8"));
  marker.version = version;
  marker.sha256 = sha256(body);
  delete marker.managed_pair;
  writeFileSync(`${executable}.install.json`, JSON.stringify(marker, null, 2) + "\n");
  rmSync(path.join(fixture.installBin, "..", "libexec", "ctx-pro"), { force: true });
  rmSync(path.join(fixture.installBin, "..", "share", "ctx"), { recursive: true, force: true });
  writeFileSync(fixture.commandLogPath, "");
  writeFileSync(fixture.pairInvocationPathLogPath, "");
  writeFileSync(fixture.downloadUrlLogPath, "");
  return executable;
}

for (const latest of ["1.3.2", "1.3.3"]) {
  for (const prior of ["0.25.0", "1.2.2", "1.3.2"]) {
    test(`stable ${prior} Core-only fixture completes signed pair at ${latest} without looping`, () => {
      const fixture = runRenderedCliInstaller({ args: bridgeTestArgs, managedPair: true, releaseVersion: latest });
      try {
        assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
        const executable = replaceWithCoreOnlyFixture(fixture, prior);
        const dataWitness = path.join(fixture.homeDir, "retained-user-history");
        writeFileSync(dataWitness, "user history must survive both phases\n");
        const result = fixture.rerun();
        assert.equal(result.status, 0, installerOutput(result));
        const marker = JSON.parse(readFileSync(`${executable}.install.json`, "utf8"));
        assert.equal(marker.version, latest);
        assert.equal(marker.managed_pair, true);
        assert.equal(marker.sha256, sha256(readFileSync(fixture.artifactPath)));
        assert.equal(readFileSync(dataWitness, "utf8"), "user history must survive both phases\n");
        assert.ok(existsSync(path.join(fixture.installBin, "..", "libexec", "ctx-pro")));
        const urls = readFileSync(fixture.downloadUrlLogPath, "utf8").trim().split("\n");
        assert.equal(urls.filter((url) => url === immutableBridgeUrl).length, prior === "1.3.2" ? 0 : 1);
        const commands = readCtxCommands(fixture.commandLogPath);
        assert.equal(commands.filter((command) => command.startsWith("--ctx-core-managed-pair-apply-v1")).length, prior === "0.25.0" ? 1 : 0);
        assert.equal(commands.filter((command) => command === "upgrade --channel stable --format=json").length, prior === "1.2.2" ? 2 : 1);
        writeFileSync(fixture.commandLogPath, "");
        writeFileSync(fixture.downloadUrlLogPath, "");
        const repeat = fixture.rerun();
        assert.equal(repeat.status, 0, installerOutput(repeat));
        assert.ok(!readFileSync(fixture.downloadUrlLogPath, "utf8").includes(immutableBridgeUrl));
        assert.equal(readCtxCommands(fixture.commandLogPath).filter((command) => command.startsWith("--ctx-core-managed-pair-apply-v1")).length, 0);
      } finally { fixture.cleanup(); }
    });
  }
}

test("managed exact metadata is rejected before any installed owner or publication", () => {
  const fixture = runRenderedCliInstaller({ args: bridgeTestArgs, managedPair: true, releaseVersion: "1.4.12" });
  try {
    assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
    const executable = path.join(fixture.installBin, "ctx");
    const before = [readFileSync(executable), readFileSync(`${executable}.install.json`)];
    writeFileSync(fixture.commandLogPath, "");
    const result = fixture.rerun(undefined, { CTX_RELEASE_METADATA_URL: "https://example.test/exact/metadata.env" });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /managed reinstall cannot honor an explicit metadata target/);
    assert.equal(readFileSync(fixture.commandLogPath, "utf8"), "");
    assert.deepEqual([readFileSync(executable), readFileSync(`${executable}.install.json`)], before);
  } finally { fixture.cleanup(); }
});

test("fresh explicit signed target keeps candidate authority", () => {
  const fixture = runRenderedCliInstaller({ args: bridgeTestArgs, managedPair: true, env: {
    CTX_RELEASE_METADATA_URL: "https://example.test/exact/metadata.env",
  } });
  try {
    assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
    assert.equal(readCtxCommands(fixture.commandLogPath).filter((command) => command.startsWith("--ctx-core-managed-pair-apply-v1")).length, 1);
  } finally { fixture.cleanup(); }
});

for (const version of ["1.3.1", "1.3.2-rc.1", "01.3.2", "version-99.0.0"]) {
  test(`stable signed target ${version} is refused before publication`, () => {
    const fixture = runRenderedCliInstaller({ args: bridgeTestArgs, managedPair: true, releaseVersion: version });
    try {
      assert.notEqual(fixture.result.status, 0);
      assert.match(fixture.result.stderr, /before 1\.3\.2 are unsupported|invalid release version/);
      assert.ok(!existsSync(path.join(fixture.installBin, "ctx")));
    } finally { fixture.cleanup(); }
  });
}

test("a newer managed installation never receives frozen B or a downgrade", () => {
  const fixture = runRenderedCliInstaller({ args: bridgeTestArgs, managedPair: true, releaseVersion: "1.3.3" });
  try {
    assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
    const executable = replaceWithCoreOnlyFixture(fixture, "1.4.0");
    const before = readFileSync(executable);
    const result = fixture.rerun();
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /refusing to downgrade/);
    assert.deepEqual(readFileSync(executable), before);
    assert.equal(readFileSync(fixture.commandLogPath, "utf8"), "");
    assert.ok(!readFileSync(fixture.downloadUrlLogPath, "utf8").includes(immutableBridgeUrl));
  } finally { fixture.cleanup(); }
});

for (const latest of ["1.3.2", "1.3.3"]) {
  test(`interrupted old helper at Core-only B resumes ordinary final owner at ${latest}`, () => {
    const fixture = runRenderedCliInstaller({ args: bridgeTestArgs, managedPair: true, releaseVersion: latest });
    try {
      assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
      const executable = replaceWithCoreOnlyFixture(fixture, "1.2.2");
      const retainedHistory = path.join(fixture.dataRoot, "retained-history");
      mkdirSync(fixture.dataRoot, { recursive: true });
      writeFileSync(retainedHistory, "original provider history\n");
      const interrupted = fixture.rerun(undefined, { CTX_FAKE_FAIL_AT_B: "1" });
      assert.notEqual(interrupted.status, 0);
      assert.equal(JSON.parse(readFileSync(`${executable}.install.json`, "utf8")).version, "1.3.2");
      assert.equal(existsSync(path.join(fixture.installBin, "..", "libexec", "ctx-pro")), false);
      writeFileSync(fixture.commandLogPath, "");
      writeFileSync(fixture.downloadUrlLogPath, "");
      const resumed = fixture.rerun([...bridgeTestArgs, "--semantic"], { CTX_FAKE_SEMANTIC_REQUIRES_PAIR: "1" });
      assert.equal(resumed.status, 0, installerOutput(resumed));
      const commands = readCtxCommands(fixture.commandLogPath);
      assert.ok(!commands.some((command) => command.startsWith("--ctx-core-managed-pair-apply-v1")));
      assert.ok(!readFileSync(fixture.downloadUrlLogPath, "utf8").includes(immutableBridgeUrl));
      assert.equal(commands.filter((command) => command === "upgrade --channel stable --format=json").length, 2);
      assert.equal(JSON.parse(readFileSync(`${executable}.install.json`, "utf8")).version, latest);
      assert.equal(readFileSync(retainedHistory, "utf8"), "original provider history\n");
      assert.ok(existsSync(path.join(fixture.semanticRoot, "runtime.installed")));
    } finally { fixture.cleanup(); }
  });
}

test("unauthenticated frozen bridge leaves the old managed executable and history intact", () => {
  const fixture = runRenderedCliInstaller({ args: bridgeTestArgs, managedPair: true, releaseVersion: "1.3.3" });
  try {
    assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
    const executable = replaceWithCoreOnlyFixture(fixture, "0.25.0");
    const before = [readFileSync(executable), readFileSync(`${executable}.install.json`)];
    writeFileSync(fixture.childEnv.CTX_FAKE_BRIDGE_METADATA_SIGNATURE, "AAAA\n");
    const result = fixture.rerun();
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /metadata signature/);
    assert.deepEqual([readFileSync(executable), readFileSync(`${executable}.install.json`)], before);
    assert.equal(readFileSync(fixture.commandLogPath, "utf8"), "");
  } finally { fixture.cleanup(); }
});

for (const partialPublication of [false, true]) {
  test(`retained B pair completes then advances to L with ${partialPublication ? "partially published" : "intact old"} Core`, () => {
    const fixture = runRenderedCliInstaller({ args: bridgeTestArgs, managedPair: true, releaseVersion: "1.3.3" });
    try {
      assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
      const executable = replaceWithCoreOnlyFixture(fixture, "0.25.0");
      if (partialPublication) writeExecutable(executable, readFileSync(fixture.bridgeArtifactPath));
      const pending = path.join(fixture.installBin, ".ctx.upgrade-install-transaction.json");
      writeFileSync(pending, '{"fixture":"retained signed B inputs"}\n');
      const result = fixture.rerun(undefined, { CTX_FAKE_RETAINED_B: "1" });
      assert.equal(result.status, 0, installerOutput(result));
      assert.equal(existsSync(pending), false);
      assert.equal(JSON.parse(readFileSync(`${executable}.install.json`, "utf8")).version, "1.3.3");
      const commands = readCtxCommands(fixture.commandLogPath);
      assert.equal(commands.filter((command) => command.startsWith("--ctx-core-managed-pair-apply-v1")).length, 1);
      assert.match(commands.find((command) => command.startsWith("--ctx-core-managed-pair-apply-v1")), partialPublication ? /\/final\/managed-pair-envelope/ : /\/bridge\/managed-pair-envelope/);
      assert.equal(commands.filter((command) => command === "upgrade --channel stable --format=json").length, 1);
      assert.equal(readFileSync(fixture.downloadUrlLogPath, "utf8").includes(immutableBridgeUrl), !partialPublication);
    } finally { fixture.cleanup(); }
  });
}

for (const mode of [0o755, 0o775]) {
  test(`fresh managed pair preserves shared bin mode ${mode.toString(8)}`, () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--no-man", "--no-skill", "--no-modify-path"],
      managedPair: true,
      prepareInstall({ installBin }) {
        chmodSync(installBin, mode);
        writeFileSync(path.join(installBin, "unrelated-tool"), "keep");
      },
    });
    try {
      assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
      assert.equal(statSync(fixture.installBin).mode & 0o777, mode);
      assert.equal(readFileSync(path.join(fixture.installBin, "unrelated-tool"), "utf8"), "keep");
    } finally {
      fixture.cleanup();
    }
  });
}

test("managed-pair failure stops TTY progress before displaying the error", {
  skip: utilLinuxScriptCommand ? false : "util-linux script is not installed",
}, () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man", "--no-skill", "--no-modify-path"],
    managedPair: true, releaseVersion: "1.4.12",
    ttyInput: Buffer.alloc(0),
    unsetNoColor: true,
    env: { CTX_FAKE_PAIR_INSTALL_ERROR: "directory allows other accounts to write", CTX_FAKE_PAIR_INSTALL_STATUS: "70" },
    prepareInstall({ installBin }) { chmodSync(installBin, 0o775); },
  });
  try {
    assert.notEqual(fixture.result.status, 0);
    const output = installerOutput(fixture.result);
    assert.match(output, /\nctx child output:/u);
    assert.doesNotMatch(output.slice(output.indexOf("ctx child output:")), /Installing ctx/u);
    assert.equal(statSync(fixture.installBin).mode & 0o777, 0o775);
  } finally {
    fixture.cleanup();
  }
});

for (const partial of ["missing", "mismatch"]) {
  test(`1.5 feed recovers retained 1.4 pair with ${partial} Core before validation`, () => {
    let witnesses;
    const fixture = runRenderedCliInstaller({ releaseVersion: "1.5.0", managedPair: true,
      args: ["--no-setup", "--no-man", "--no-skill", "--no-modify-path"],
      env: { CTX_FAKE_RETAINED_14: "1" },
      prepareInstall: ({ installBin, sandbox, homeDir }) => {
        const install = path.join(installBin, "ctx");
        const root = path.dirname(installBin);
        const retained = path.join(root, "share/ctx/.managed-pair-apply-v1");
        const oldBytes = readFileSync(path.join(sandbox, "ctx-artifact"), "utf8").replaceAll("1.5.0", "1.4.12");
        for (const dir of ["bin", "libexec", "share/ctx"]) mkdirSync(path.join(retained, dir), { recursive: true });
        writeExecutable(path.join(retained, "bin/ctx"), oldBytes);
        const marker = JSON.stringify({ schema_version: 1, manager: "ctx-hosted-installer", install_path: install,
          platform: "linux-x64", channel: "stable", version: "1.4.12", sha256: sha256(oldBytes), managed_pair: true }, null, 2) + "\n";
        writeFileSync(path.join(retained, "bin/ctx.install.json"), marker);
        writeFileSync(install + ".install.json", marker);
        writeFileSync(path.join(retained, "libexec/ctx-pro"), "authored inert old companion\n");
        writeFileSync(path.join(retained, "share/ctx/managed-pair-envelope.json"), "authored native-owner fixture\n");
        writeFileSync(path.join(installBin, ".ctx.upgrade-install-transaction.json"), "authored pending transaction\n");
        if (partial === "mismatch") writeExecutable(install, "#!/bin/sh\nexit 88\n");
        witnesses = ["history/record", "search/attribution/segment", "pro/key"].map((name) => path.join(homeDir, ".ctx", name));
        for (const file of witnesses) { mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, "authored witness\n"); }
      },
    });
    try {
      assert.equal(fixture.result.status, 0, installerOutput(fixture.result));
      const commands = readCtxCommands(fixture.commandLogPath);
      const recovery = commands.findIndex((command) => command.startsWith("--ctx-core-managed-pair-apply-v1"));
      const upgrade = commands.findIndex((command) => command === "upgrade --channel stable --format=json");
      assert.ok(recovery >= 0 && upgrade > recovery, commands.join("\n"));
      assert.match(commands[recovery], /share\/ctx\/\.managed-pair-apply-v1\/share\/ctx\/managed-pair-envelope.json/);
      assert.doesNotMatch(readFileSync(fixture.downloadUrlLogPath, "utf8"), /ctx-pro|managed-pair-envelope/);
      assert.equal(JSON.parse(readFileSync(path.join(fixture.installBin, "ctx.install.json"), "utf8")).version, "1.5.0");
      for (const file of witnesses) assert.equal(readFileSync(file, "utf8"), "authored witness\n");
    } finally { fixture.cleanup(); }
  });
}
