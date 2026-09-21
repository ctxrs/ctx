// POSIX rendering, platform, ownership, and receipt contracts.
import "./cli-install-shell-resource-tests.mjs";
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
import {
  assertRuntimeRepairUsesVerifiedMetadata,
  readStageReports,
} from "./cli-install-report-helpers.mjs";
import {
  registerCliInstallShellManagedUpgradeTests,
} from "./cli-install-shell-managed-upgrade-tests.mjs";
import {
  registerCliInstallShellPathManTests,
} from "./cli-install-shell-path-man-tests.mjs";
import {
  validSetupReceipt,
  setupSchemaReaderCases,
} from "./cli-install-setup-receipt-fixture.mjs";

test("rendered CLI installer is POSIX shell syntax", () => {
  const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-script-"));
  try {
    const scriptPath = path.join(sandbox, "install.sh");
    writeFileSync(scriptPath, renderCliInstallScript());
    const result = spawnSync("sh", ["-n", scriptPath], {
      encoding: "utf8",
    });
    assert.equal(result.status, 0, result.stderr);
  } finally {
    rmSync(sandbox, { recursive: true, force: true });
  }
});

test("rendered CLI installer directs FreeBSD hosts to the source build", () => {
  const body = renderCliInstallScript();
  const freebsdGuard = 'if [ "$host_os" = "FreeBSD" ]; then';
  const sourceOnlyFailure = 'fail "FreeBSD has no prebuilt ctx binary; build ctx from source: https://github.com/ctxrs/ctx/blob/main/docs/unmanaged-installs.md#source-builds"';
  const genericFailure = 'fail "cannot detect this host platform; set CTX_PLATFORM"';

  assert.match(body, /host_os="\$\(uname -s 2>\/dev\/null \|\| printf unknown\)"/);
  assert.ok(body.indexOf(freebsdGuard) >= 0);
  assert.ok(body.indexOf(sourceOnlyFailure) > body.indexOf(freebsdGuard));
  assert.ok(body.indexOf(sourceOnlyFailure) < body.indexOf(genericFailure));
  assert.doesNotMatch(body, /FreeBSD:[^\n]*printf 'freebsd-x64'/);
});

test("rendered CLI installer defaults to cli.ctx.rs release metadata", () => {
  const body = renderCliInstallScript();
  assert.ok(body.includes(
    'release_functions_base="${CTX_UPGRADE_FUNCTIONS_BASE:-https://cli.ctx.rs/functions/v2}"',
  ));
  assert.ok(body.includes(
    'install_telemetry_endpoint="https://cli.ctx.rs/functions/v1/install-attempt"',
  ));
  assert.match(body, /ctx-release-metadata\.env/);
  assert.match(body, /CTX_BIN_DIR/);
  assert.match(body, /--no-setup/);
  assert.match(body, /--no-daemon/);
  assert.match(body, /--pro-trial/);
  assert.match(body, /--no-skill/);
  assert.match(body, /--skill-agent/);
  assert.match(body, /--all-skill-agents/);
  assert.match(body, /--no-modify-path/);
  assert.match(body, /CTX_INSTALL_NO_SETUP/);
  assert.match(body, /CTX_INSTALL_NO_DAEMON/);
  assert.match(body, /CTX_INSTALL_NO_SKILL/);
  assert.match(body, /CTX_INSTALL_SKILL_AGENTS/);
  assert.match(body, /CTX_INSTALL_ALL_SKILL_AGENTS/);
  assert.match(body, /CTX_INSTALL_NO_MODIFY_PATH/);
  assert.match(body, /CTX_INSTALL_NO_MAN/);
  assert.match(body, /--semantic/);
  assert.match(body, /CTX_INSTALL_SEMANTIC/);
  assert.match(body, /CTX_SEARCH_SEMANTIC=1/);
  assert.match(body, /load_persisted_config_controls\(\)/);
  assert.match(body, /function valid_utf8\(value,/);
  assert.match(body, /if \(!valid_utf8\(\$0\)\)/);
  assert.doesNotMatch(body, /iconv/);
  assert.match(body, /full_key == "search\.semantic"/);
  assert.match(body, /full_key == "daemon\.enabled"/);
  assert.match(body, /duplicate config key `.*` at line/);
  assert.match(body, /CTX_DAEMON_ENABLED=false/);
  assert.match(body, /canonical_daemon_disabled\(\)/);
  assert.match(body, /has_controlling_tty\(\)/);
  assert.match(body, /\( : <\/dev\/tty \) 2>\/dev\/null/);
  assert.doesNotMatch(body, /pro_trial_answer|Please answer Y or n/);
  assert.match(
    body,
    /if \[ "\$\{CTX_SEARCH_SEMANTIC\+x\}" = "x" \]/,
  );
  assert.match(body, /case "\$semantic_search_control_value" in\s+""\)\s+;;/);
  assert.match(body, /semantic_search_control=false/);
  assert.match(
    body,
    /1\|\[Tt\]\[Rr\]\[Uu\]\[Ee\]\|\[Yy\]\[Ee\]\[Ss\]\|\[Oo\]\[Nn\]\) semantic_enabled=1/,
  );
  const semanticPreflight = body.indexOf(
    'fail "Semantic installation requires an enabled daemon;',
  );
  assert.ok(semanticPreflight >= 0);
  assert.ok(
    semanticPreflight < body.indexOf('tmp_dir="$(mktemp -d'),
    "POSIX Semantic/no-daemon preflight must run before temporary/download activity",
  );
  const persistedConfigPreflight = body.indexOf("\nload_persisted_config_controls\n");
  assert.ok(persistedConfigPreflight >= 0);
  assert.ok(
    persistedConfigPreflight < body.indexOf('tmp_dir="$(mktemp -d'),
    "POSIX persisted-config preflight must run before temporary/download activity",
  );
  assert.match(body, /CTX_ANALYTICS_ENABLED=false/);
  assert.match(body, /CTX_UPGRADE_FUNCTIONS_BASE/);
  assert.match(body, /CTX_UPGRADE_CHANNEL/);
  assert.match(body, /--hosted-transaction install/);
  assert.match(body, /--marker-source "\$marker_tmp_path"/);
  assert.match(body, /--ownership-source "\$integration_manifest_tmp"/);
  assert.match(body, /--binary-sha256 "\$actual_checksum"/);
  assert.match(body, /hosted_install_transaction/);
  for (const retired of ["CTX_CHANNEL", "CTX_FUNCTIONS_BASE"]) {
    assert.doesNotMatch(body, new RegExp(retired));
  }
  for (const deprecated of [
    "CTX_ANALYTICS_OFF",
    "CTX_DISABLE_ANALYTICS",
    "CTX_INSTALL_DIAGNOSTICS_OFF",
    "CTX_DAEMON_OFF",
    "CTX_DISABLE_DAEMON",
    "CTX_UPGRADE_OFF",
    "CTX_DISABLE_AUTO_UPGRADE",
  ]) {
    assert.match(body, new RegExp(deprecated));
  }
  assert.match(body, /canonical_analytics_disabled\(\)/);
  assert.match(body, /analytics_value="\$\{CTX_ANALYTICS_ENABLED-\}"/);
  assert.match(
    body,
    /0\|\[Ff\]\[Aa\]\[Ll\]\[Ss\]\[Ee\]\|\[Nn\]\[Oo\]\|\[Oo\]\[Ff\]\[Ff\]\) return 0/,
  );
  assert.match(body, /canonical_analytics_disabled && return 0/);
  assert.match(body, /install_attempt_id="ia_[A-Za-z0-9_-]{8,128}"/);
  assert.match(body, /valid_install_attempt_id "\$CTX_INSTALL_ATTEMPT_ID"/);
  assert.match(body, /install-attempt/);
  assert.match(body, /case "\$install_telemetry_endpoint" in\s+https:\/\/\*\) ;;/);
  assert.match(body, /CTX_ALLOW_CUSTOM_RELEASE_BASE_URL/);
  assert.match(body, /CTX_RELEASE_METADATA_SIGNATURE_URL/);
  assert.match(body, /Linux:aarch64\|Linux:arm64\) printf 'linux-aarch64'/);
  assert.match(body, /linux-x64\|linux-aarch64\|macos-arm64\|macos-x64/);
  assert.doesNotMatch(body, /freebsd-x64/);
  assert.match(body, /openssl dgst -sha256 -verify/);
  assert.doesNotMatch(body, /pkeyutl/);
  assert.doesNotMatch(body, /minisign/);
  assert.match(body, /cli\.ctx\.rs\/storage\/v1\/object\/public\/releases\/artifacts/);
  assert.match(body, /checksum for \$platform is a placeholder/);
  assert.match(body, /gzip -dc "\$gzip_dest"/);
  assert.doesNotMatch(body, /Downloaded gzip-compressed artifact/);
  assert.match(body, /\$install_path" docs man --out "\$generated_man_dir" >"\$tmp_dir\/man-install\.out" 2>&1/);
  assert.match(body, /set -- integrations install skills/);
  assert.match(body, /set -- "\$@" --format=json/);
  assert.match(body, /set -- setup/);
  assert.match(body, /set -- setup --quiet --format json/);
  assert.match(body, /set -- "\$@" --semantic/);
  assert.match(body, /set -- "\$@" --progress "\$setup_progress"/);
  assert.match(body, />"\$tmp_dir\/setup-receipt\.json"/);
  assert.doesNotMatch(body, /status --format=json|pro setup --trial-only/);
  assert.match(body, /setup_schema_version.*schema_version/);
  assert.match(body, /setup_mode.*mode/);
  assert.doesNotMatch(body, /inventory_units|cataloged_sessions|inventory_source_bytes/);
  assert.match(body, /\[ ! -L "\$secure_bin_dir" \]/);
  assert.match(body, /path_owner_uid "\$secure_bin_dir"/);
  assert.doesNotMatch(body, /chmod go-w/);
  assert.match(body, /CTX_RELEASE_METADATA_URL="\$repair_metadata_uri"/);
  assert.match(body, /CTX_RELEASE_METADATA_SIGNATURE_URL="\$repair_metadata_signature_uri"/);
  assert.ok(
    body.indexOf('"$install_path" upgrade --channel "$channel" --format=json')
      < body.lastIndexOf('CTX_HOSTED_INSTALLER_SETUP=1 "$install_path" "$@"'),
  );
  assert.equal(body.match(/CTX_HOSTED_INSTALLER_SETUP=1 "\$install_path" "\$@"/g)?.length, 1);
  assert.match(body, /ctx installer PATH setup/);
  assert.doesNotMatch(body, /AppImage/);
  assert.doesNotMatch(body, /"staging_dogfood":/);
});

test("rendered CLI installer keeps release metadata and telemetry routes separate", () => {
  const releaseFunctionsBase =
    "https://release.example.test/functions/v1";
  const installTelemetryEndpoint =
    "https://telemetry.example.test/functions/v1/install-attempt";
  const body = renderCliInstallScript({
    installTelemetryEndpoint,
    releaseFunctionsBase,
  });

  assert.ok(body.includes(
    `release_functions_base="\${CTX_UPGRADE_FUNCTIONS_BASE:-${releaseFunctionsBase}}"`,
  ));
  assert.ok(body.includes(
    'metadata_url="${CTX_RELEASE_METADATA_URL:-${release_functions_base%/}/releases/$channel/ctx-release-metadata.env}"',
  ));
  assert.ok(body.includes(
    `install_telemetry_endpoint="${installTelemetryEndpoint}"`,
  ));
  assert.ok(body.includes(
    'curl -fsS --connect-timeout 1 --max-time 1 -H "content-type: application/json" -X POST --data "$payload" "$install_telemetry_endpoint"',
  ));
  assert.doesNotMatch(
    body,
    /release\.example\.test\/functions\/v1\/install-attempt/u,
  );
});

test("rendered CLI installer requires an explicit staging dogfood marker option", () => {
  assert.doesNotMatch(
    renderCliInstallScript({ stagingDogfood: false }),
    /"staging_dogfood":/,
  );
  assert.match(
    renderCliInstallScript({ stagingDogfood: true }),
    /"staging_dogfood": true,/,
  );
  assert.throws(
    () => renderCliInstallScript({ stagingDogfood: "true" }),
    /stagingDogfood must be a boolean/u,
  );
});

test("rendered CLI installer gives fresh signed-pair publication to the candidate Core", () => {
  const body = renderCliInstallScript();
  assert.match(body, /CTX_RELEASE_MANAGED_PAIR_ENVELOPE_\$platform_key/u);
  assert.match(body, /CTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_\$platform_key/u);
  assert.match(body, /CTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_\$platform_key/u);
  assert.match(
    body,
    /"\$artifact_path" --ctx-core-managed-pair-apply-v1 "\$pair_install_root" - "\$pair_envelope_path" "\$artifact_path" "\$companion_artifact_path" "\$pair_apply_marker"/u,
  );
  assert.match(body, /apply_managed_pair_candidate "\$marker_tmp_path" 1/u);
  assert.equal(body.match(/--ctx-core-managed-pair-apply-v1/gu)?.length, 1);
  assert.doesNotMatch(body, /--ctx-core-hosted-pair-install-v1/u);
  assert.doesNotMatch(body, /installer-support|install-managed-pair\.py|python3 -I/u);
  const pairPublication = body.indexOf(
    'if [ -n "$pair_envelope_artifact" ]; then\n  actual_companion_checksum=',
  );
  const candidateBridge = body.indexOf(
    "    if managed_pair_requires_candidate_apply; then",
    pairPublication,
  );
  const bridgedPairPublication = body.indexOf(
    'apply_managed_pair_candidate "$marker_tmp_path" 1',
    candidateBridge,
  );
  const managedCoreUpgrade = body.indexOf(
    "      run_managed_core_upgrade",
    bridgedPairPublication,
  );
  const freshPairPublication = body.indexOf(
    'apply_managed_pair_candidate "$marker_tmp_path" 1',
    managedCoreUpgrade,
  );
  const coreOnlyPublication = body.indexOf(
    "    publish_fresh_or_legacy_binary",
    freshPairPublication,
  );
  assert.ok(
    pairPublication >= 0
      && candidateBridge > pairPublication
      && bridgedPairPublication > candidateBridge
      && managedCoreUpgrade > bridgedPairPublication
      && freshPairPublication > managedCoreUpgrade
      && coreOnlyPublication > freshPairPublication,
    "candidate bridges, paired reruns, and fresh apply must precede the non-pair publisher",
  );
  assert.match(
    body,
    /managed-pair installation did not complete; resolve the error above before retrying/u,
  );
  assert.match(body, /managed_pair_success_receipt "\$pair_apply_receipt" managed_pair_apply/u);
  assert.match(body, /path_size_bytes "\$receipt_path"[\s\S]*-le 512/u);
  const commandHints = body.indexOf(`log '  Search:    ctx search "test failure"'`);
  assert.ok(commandHints > freshPairPublication, "command hints follow installation");
  assert.doesNotMatch(body, /managed_setup/u);
  assert.doesNotMatch(body, /managed-pair staging install requires a clean install root/u);
});

test("semantic repair file URLs preserve standards encoding across supported path forms", () => {
  const posix = "file:///tmp/semantic%20repair/%E8%AF%AD%E4%B9%89/metadata.env";
  assert.equal(
    fileURLToPath(posix),
    "/tmp/semantic repair/语义/metadata.env",
  );

  const windows = "file:///C:/Program%20Files/ctx/%E8%AF%AD%E4%B9%89/metadata.env";
  assert.equal(
    fileURLToPath(windows, { windows: true }),
    "C:\\Program Files\\ctx\\语义\\metadata.env",
  );

  const shell = renderCliInstallScript();
  assert.match(shell, /absolute_path_to_file_uri\(\)/);
  assert.match(shell, /printf "%%%s", toupper\(\$i\)/);
  const powershell = renderCliInstallPowerShellScript();
  assert.match(
    powershell,
    /\[System\.Uri\]::new\(\[System\.IO\.Path\]::GetFullPath\(\$metadataFile\)\)/,
  );
  assert.match(powershell, /\)\.AbsoluteUri/);
});

test("rendered CLI installer supports linux-aarch64 metadata and markers", () => {
  const { result, cleanup, installStageLogPath, installBin } = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
    platform: "linux-aarch64",
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stderr, /Installing ctx 9.9.9\.\.\./);
    const marker = JSON.parse(readFileSync(path.join(installBin, "ctx.install.json"), "utf8"));
    assert.equal(marker.platform, "linux-aarch64");
    assert.equal(marker.artifact_url, "https://example.test/releases/ctx-linux-aarch64");
    const reports = readStageReports(installStageLogPath);
    assert.ok(reports.length > 0);
    assert.ok(reports.every((report) => report.platform === "linux"));
    assert.ok(reports.every((report) => report.arch === "arm64"));
  } finally {
    cleanup();
  }
});

test("rendered CLI installer help does not require OpenSSL first", () => {
  const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-help-"));
  try {
    const scriptPath = path.join(sandbox, "install.sh");
    writeExecutable(path.join(sandbox, "cat"), `#!/bin/sh\n/bin/cat "$@"\n`);
    writeFileSync(scriptPath, renderCliInstallScript());
    const result = spawnSync("/bin/sh", [scriptPath, "--help"], {
      encoding: "utf8",
      env: {
        ...process.env,
        PATH: sandbox,
      },
    });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /Prerequisites:/);
    assert.match(result.stdout, /OpenSSL/);
  } finally {
    rmSync(sandbox, { recursive: true, force: true });
  }
});

test("rendered CLI installer runs setup by default after installing ctx", () => {
  const { result, cleanup, setupArgsPath, installStageLogPath, installBin, manDir } = runRenderedCliInstaller();
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stderr, /Installing ctx 9.9.9\.\.\./);
    assert.doesNotMatch(result.stderr, /  skill:/);
    assert.doesNotMatch(result.stderr, /Installed ctx binary/);
    assert.doesNotMatch(result.stderr, /Downloaded gzip-compressed artifact/);
    assert.doesNotMatch(result.stderr, /verified release metadata signature/);
    assert.doesNotMatch(result.stderr, /wrote ctx managed install marker/);
    assert.doesNotMatch(result.stderr, /installed ctx man pages/);
    assert.doesNotMatch(result.stderr, /wrote ctx man pages/);
    assert.doesNotMatch(result.stderr, /Indexing local agent history/);
    assert.match(result.stderr, /\nInstalled and verified\n/);
    assert.match(
      result.stderr,
      /\nFound 3,800 sessions\nIndex ready\n/,
    );
    assert.doesNotMatch(result.stderr, /Core is ready|;/);
    assert.match(
      readFileSync(setupArgsPath, "utf8"),
      /^docs\nman\n--out\n.*\/generated-man\nintegrations\ninstall\nskills\n--format=json\nsetup\n--quiet\n--format\njson\n--wait\n--progress\nnone\n$/u,
    );
    const marker = JSON.parse(readFileSync(path.join(installBin, "ctx.install.json"), "utf8"));
    assert.equal(marker.schema_version, 1);
    assert.equal(marker.manager, "ctx-hosted-installer");
    assert.equal(Object.hasOwn(marker, "staging_dogfood"), false);
    assert.match(marker.install_attempt_id, /^ia_[A-Za-z0-9_-]{8,128}$/);
    assert.equal(marker.version, "9.9.9");
    assert.equal(marker.platform, "linux-x64");
    assert.equal(marker.channel, "stable");
    assert.equal(marker.source_commit, "abc123");
    assert.ok(Number.isFinite(Date.parse(marker.installed_at)));
    const reports = readStageReports(installStageLogPath);
    assert.deepEqual(reports.map((report) => report.stage), [
      "installer",
      "artifact_download",
      "artifact_download",
      "binary_install",
      "skill_install",
      "skill_install",
      "setup",
      "setup",
      "installer",
    ]);
    assert.deepEqual(reports.map((report) => report.status), [
      "started",
      "started",
      "completed",
      "completed",
      "started",
      "completed",
      "started",
      "completed",
      "completed",
    ]);
    assert.equal(new Set(reports.map((report) => report.install_attempt_id)).size, 1);
    assert.equal(reports[0].install_attempt_id, marker.install_attempt_id);
    assert.ok(reports.every((report) => report.event_name === INSTALL_STAGE_EVENT_NAME));
    assert.ok(reports.every((report) => report.event_version === INSTALL_STAGE_EVENT_VERSION));
    assert.ok(reports.every((report) => report.platform === "linux"));
    assert.ok(reports.every((report) => report.arch === "x64"));
    assert.ok(reports.every((report) => report.script_family === "posix"));
  } finally {
    cleanup();
  }
});

test("rendered CLI installer exposes the hosted marker only to the native setup child", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--pro-trial"],
    env: {
      CTX_FAKE_PERSIST_HOSTED_SEMANTIC: "1",
      CTX_SEARCH_SEMANTIC: "true",
    },
  });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    const childEnvironment = readFileSync(fixture.hostedSetupEnvPath, "utf8")
      .trim()
      .split("\n")
      .map((line) => {
        const [command, hostedSetup, semantic] = line.split("|");
        return { command, hostedSetup, semantic };
      });
    assert.deepEqual(
      childEnvironment.map(({ command, hostedSetup }) => ({ command, hostedSetup })),
      [
        { command: "upgrade --channel stable --format=json", hostedSetup: "" },
        {
          command: childEnvironment[1].command,
          hostedSetup: "",
        },
        {
          command: "integrations install skills --format=json",
          hostedSetup: "",
        },
        {
          command: "setup --quiet --format json --wait --semantic --progress none",
          hostedSetup: "1",
        },
      ],
    );
    assert.match(childEnvironment[1].command, /^docs man --out .*\/generated-man$/u);
    assert.equal(childEnvironment[3].semantic, "true");
    assert.equal(
      readFileSync(fixture.configPath, "utf8"),
      "[search]\nsemantic = true\n",
    );
  } finally {
    fixture.cleanup();
  }
});

registerCliInstallShellPathManTests();

test("managed installer upgrade carries profile-file, man, and skill ownership through uninstall", () => {
  const fixture = runRenderedCliInstaller();
  const profilePath = path.join(fixture.homeDir, ".bashrc");
  const skillPath = path.join(
    fixture.homeDir,
    ".agents",
    "skills",
    "ctx-agent-history-search",
  );
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    const upgrade = fixture.rerun();
    assert.equal(upgrade.status, 0, upgrade.stderr);
    const records = readOwnershipRecords(fixture.installBin);
    assert.deepEqual(
      records.map(({ kind }) => kind).sort(),
      ["man", "man", "profile-file", "skill"],
    );
    assert.equal(new Set(records.map(({ target }) => target)).size, records.length);

    const uninstall = runHostedUninstallForInstallerFixture(fixture);
    assert.equal(uninstall.status, 0, uninstall.stderr);
    assert.equal(existsSync(path.join(fixture.manDir, "ctx.1")), false);
    assert.equal(existsSync(path.join(fixture.manDir, "ctx-search.1")), false);
    assert.equal(existsSync(profilePath), false);
    assert.equal(existsSync(path.join(skillPath, "SKILL.md")), false);
    assert.equal(existsSync(path.join(skillPath, ".ctx-skill.json")), false);
  } finally {
    fixture.cleanup();
  }
});

registerCliInstallShellManagedUpgradeTests();

test("managed installer upgrade carries an exact profile block without adopting user content", () => {
  const initialProfileContents = "# user-owned profile\n";
  const fixture = runRenderedCliInstaller({ initialProfileContents });
  const profilePath = path.join(fixture.homeDir, ".bashrc");
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    assert.equal(fixture.rerun().status, 0);
    const records = readOwnershipRecords(fixture.installBin);
    assert.equal(records.filter(({ kind }) => kind === "profile-block").length, 1);
    assert.equal(records.filter(({ target }) => target === profilePath).length, 1);

    const uninstall = runHostedUninstallForInstallerFixture(fixture);
    assert.equal(uninstall.status, 0, uninstall.stderr);
    assert.equal(readFileSync(profilePath, "utf8"), initialProfileContents);
  } finally {
    fixture.cleanup();
  }
});

test("managed installer upgrade rejects stale ownership and preserves modified man, profile, and skill files", () => {
  const fixture = runRenderedCliInstaller();
  const profilePath = path.join(fixture.homeDir, ".bashrc");
  const manPath = path.join(fixture.manDir, "ctx.1");
  const secondManPath = path.join(fixture.manDir, "ctx-search.1");
  const skillPath = path.join(
    fixture.homeDir,
    ".agents",
    "skills",
    "ctx-agent-history-search",
  );
  const modifiedMan = ".TH user-modified-ctx 1\n";
  const modifiedProfile = `${readFileSync(profilePath, "utf8")}# user customization\n`;
  const modifiedSkill = "# user-modified skill\n";
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    writeFileSync(manPath, modifiedMan);
    writeFileSync(profilePath, modifiedProfile);
    writeFileSync(path.join(skillPath, "SKILL.md"), modifiedSkill);

    const upgrade = fixture.rerun([], { CTX_FAKE_SKILL_PRESERVE_EXISTING: "1" });
    assert.equal(upgrade.status, 0, upgrade.stderr);
    const records = readOwnershipRecords(fixture.installBin);
    assert.deepEqual(records.map(({ target }) => target), [secondManPath]);

    const uninstall = runHostedUninstallForInstallerFixture(fixture);
    assert.equal(uninstall.status, 0, uninstall.stderr);
    assert.equal(readFileSync(manPath, "utf8"), modifiedMan);
    assert.equal(existsSync(secondManPath), false);
    assert.equal(readFileSync(profilePath, "utf8"), modifiedProfile);
    assert.equal(readFileSync(path.join(skillPath, "SKILL.md"), "utf8"), modifiedSkill);
    assert.equal(existsSync(path.join(skillPath, ".ctx-skill.json")), true);
  } finally {
    fixture.cleanup();
  }
});

for (const corruption of [
  "unknown-record",
  "duplicate-target",
  "blank-record",
  "carriage-return-path",
]) {
  test(`managed installer upgrade rejects ${corruption} in prior bound ownership before replacement`, () => {
    const fixture = runRenderedCliInstaller();
    const binaryPath = path.join(fixture.installBin, "ctx");
    const markerPath = `${binaryPath}.install.json`;
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const binaryBefore = readFileSync(binaryPath);
      const marker = JSON.parse(readFileSync(markerPath, "utf8"));
      const manifestLines = readFileSync(marker.integrations_path, "utf8").trimEnd().split("\n");
      const recordLines = manifestLines.slice(2);
      if (corruption === "unknown-record") {
        recordLines[0] = recordLines[0].replace(/^man\t/u, "unknown\t");
      } else if (corruption === "duplicate-target") {
        recordLines.push(recordLines[0]);
      } else if (corruption === "blank-record") {
        recordLines.splice(1, 0, "");
      } else {
        recordLines[0] = `${recordLines[0]}\r`;
      }
      const recordsBody = `${recordLines.join("\n")}\n`;
      const manifestBody = [
        "CTX_INSTALL_INTEGRATIONS_V1",
        `records_sha256\t${sha256(recordsBody)}`,
        recordsBody,
      ].join("\n");
      writeFileSync(marker.integrations_path, manifestBody);
      marker.integrations_sha256 = sha256(manifestBody);
      writeFileSync(markerPath, `${JSON.stringify(marker, null, 2)}\n`);

      const upgrade = fixture.rerun();
      assert.notEqual(upgrade.status, 0);
      assert.match(
        upgrade.stderr,
        corruption === "unknown-record"
          ? /unknown record type/
          : corruption === "duplicate-target"
            ? /duplicate targets/
            : corruption === "blank-record"
              ? /noncanonical blank record/
              : /invalid record/,
      );
      assert.deepEqual(readFileSync(binaryPath), binaryBefore);
      assert.equal(readFileSync(marker.integrations_path, "utf8"), manifestBody);
    } finally {
      fixture.cleanup();
    }
  });
}

for (const staleCase of [
  {
    name: "stale marker digest",
    mutate: ({ marker, markerPath }) => {
      marker.integrations_sha256 = "0".repeat(64);
      writeFileSync(markerPath, `${JSON.stringify(marker, null, 2)}\n`);
    },
    error: /integration ownership differs from its marker/,
  },
  {
    name: "missing bound manifest",
    mutate: ({ marker }) => rmSync(marker.integrations_path),
    error: /integration ownership is absent/,
  },
  {
    name: "hard-linked manifest",
    mutate: ({ marker }) => linkSync(marker.integrations_path, `${marker.integrations_path}.alias`),
    error: /must not be hard-linked/,
  },
  {
    name: "symlinked manifest",
    mutate: ({ marker }) => {
      const manifestBody = readFileSync(marker.integrations_path);
      const movedManifest = `${marker.integrations_path}.moved`;
      writeFileSync(movedManifest, manifestBody);
      rmSync(marker.integrations_path);
      symlinkSync(movedManifest, marker.integrations_path);
    },
    error: /cannot replace invalid prior managed integration ownership/,
  },
]) {
  test(`managed installer upgrade rejects ${staleCase.name} before replacement`, () => {
    const fixture = runRenderedCliInstaller();
    const binaryPath = path.join(fixture.installBin, "ctx");
    const markerPath = `${binaryPath}.install.json`;
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const binaryBefore = readFileSync(binaryPath);
      const marker = JSON.parse(readFileSync(markerPath, "utf8"));
      staleCase.mutate({ marker, markerPath });

      const upgrade = fixture.rerun();
      assert.notEqual(upgrade.status, 0);
      assert.match(upgrade.stderr, staleCase.error);
      assert.deepEqual(readFileSync(binaryPath), binaryBefore);
    } finally {
      fixture.cleanup();
    }
  });
}

test("post-publication installer failure retains carried ownership for safe uninstall", () => {
  const fixture = runRenderedCliInstaller();
  const profilePath = path.join(fixture.homeDir, ".bashrc");
  const skillPath = path.join(
    fixture.homeDir,
    ".agents",
    "skills",
    "ctx-agent-history-search",
  );
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    const failedUpgrade = fixture.rerun(
      ["--semantic"],
      { CTX_FAKE_RUNTIME_REPAIR_STATUS: "77" },
    );
    assert.notEqual(failedUpgrade.status, 0);
    assert.match(failedUpgrade.stderr, /ctx Semantic runtime repair failed/);
    assert.deepEqual(
      readOwnershipRecords(fixture.installBin).map(({ kind }) => kind).sort(),
      ["man", "man", "profile-file", "skill"],
    );

    const uninstall = runHostedUninstallForInstallerFixture(fixture);
    assert.equal(uninstall.status, 0, uninstall.stderr);
    assert.equal(existsSync(path.join(fixture.manDir, "ctx.1")), false);
    assert.equal(existsSync(profilePath), false);
    assert.equal(existsSync(path.join(skillPath, "SKILL.md")), false);
  } finally {
    fixture.cleanup();
  }
});

test("explicit staging dogfood rendering writes the exact managed install marker", () => {
  const { result, cleanup, installBin } = runRenderedCliInstaller({
    args: ["--no-setup"],
    stagingDogfood: true,
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    const marker = JSON.parse(
      readFileSync(path.join(installBin, "ctx.install.json"), "utf8"),
    );
    assert.equal(marker.schema_version, 1);
    assert.equal(marker.manager, "ctx-hosted-installer");
    assert.equal(marker.staging_dogfood, true);
  } finally {
    cleanup();
  }
});

test("rendered CLI installer uses indexed items when no sessions are indexed", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-pro-trial", "--no-skill", "--no-man"],
    env: {
      CTX_FAKE_INDEXED_SESSIONS: "0",
      CTX_FAKE_INDEXED_ITEMS: "42",
    },
    installDirOnPath: true,
  });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    assert.match(fixture.result.stderr, /\nFound 42 records\n/);
    assert.doesNotMatch(fixture.result.stderr, /Found 42 sessions/);
  } finally {
    fixture.cleanup();
  }
});

test("rendered CLI installer accepts an empty but initialized index", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-pro-trial", "--no-skill", "--no-man"],
    env: {
      CTX_FAKE_INDEXED_SESSIONS: "0",
      CTX_FAKE_INDEXED_ITEMS: "0",
    },
    installDirOnPath: true,
  });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    assert.match(fixture.result.stderr, /\nFound 0 records\nIndex ready\n/);
    assert.doesNotMatch(fixture.result.stderr, /sessions|GiB|about [0-9]+ minutes?/);
  } finally {
    fixture.cleanup();
  }
});

test("rendered CLI installer consumes one native Core receipt in CI", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-skill", "--no-man", "--no-modify-path"],
    env: { CI: "true" },
    installDirOnPath: true,
  });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    assert.deepEqual(readCtxCommands(fixture.commandLogPath), [
      "setup --quiet --format json --wait --progress none",
    ]);
    assert.doesNotMatch(fixture.result.stderr, /ctx pro trial started/);
  } finally {
    fixture.cleanup();
  }
});

test("rendered CLI installer accepts native setup schemas 2 and 3 with unchanged validation", () => {
  for (const { name, receipt, accepted } of setupSchemaReaderCases()) {
    const fixture = runRenderedCliInstaller({
      args: ["--no-pro-trial", "--no-skill", "--no-man", "--no-modify-path"],
      env: { CTX_FAKE_SETUP_RECEIPT: receipt },
      installDirOnPath: true,
    });
    try {
      assert.equal(fixture.result.status, accepted ? 0 : 1, `${name}: ${installerOutput(fixture.result)}`);
      assert.deepEqual(readCtxCommands(fixture.commandLogPath), [
        "setup --quiet --format json --wait --progress none",
      ]);
      if (accepted) assert.match(fixture.result.stderr, /Found 3 sessions\nIndex ready/);
      else assert.match(fixture.result.stderr, /warning: Setup failed\. Retry: ctx setup/);
    } finally {
      fixture.cleanup();
    }
  }
});

test("rendered CLI installer atomically repairs a permissive managed marker", () => {
  const fixture = runRenderedCliInstaller({
    args: ["--no-setup", "--no-man"],
    installDirOnPath: true,
  });
  const markerPath = path.join(fixture.installBin, "ctx.install.json");
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    chmodSync(markerPath, 0o664);

    const upgrade = fixture.rerun();
    assert.equal(upgrade.status, 0, upgrade.stderr);
    assert.equal(statSync(markerPath).mode & 0o777, 0o600);
    assert.equal(JSON.parse(readFileSync(markerPath, "utf8")).manager, "ctx-hosted-installer");
    assert.deepEqual(
      readdirSync(fixture.installBin).filter((name) =>
        name.startsWith("ctx.install.json.tmp.")
      ),
      [],
    );

    const rendered = renderCliInstallScript({
      installAttemptId: "ia_atomic_marker_mode",
    });
    const siblingPattern = rendered.indexOf(
      'stage_install_marker "$marker_path.tmp.XXXXXX"',
    );
    const temporaryCreate = rendered.indexOf(
      'marker_tmp_path="$(mktemp "$marker_tmp_pattern")"',
    );
    const modeProtection = rendered.indexOf(
      'chmod 0600 "$marker_tmp_path"',
      temporaryCreate,
    );
    const markerWrite = rendered.indexOf(
      'cat >"$marker_tmp_path"',
      modeProtection,
    );
    const destinationValidation = rendered.indexOf(
      '[ -L "$marker_path" ]',
      markerWrite,
    );
    const atomicRename = rendered.indexOf(
      'mv -f "$marker_tmp_path" "$marker_path"',
      destinationValidation,
    );
    assert.ok(
      siblingPattern >= 0 &&
        temporaryCreate >= 0 &&
        temporaryCreate < modeProtection &&
        modeProtection < markerWrite &&
        markerWrite < destinationValidation &&
        destinationValidation < atomicRename,
      "marker must be owner-safe and reject non-files before atomic publication",
    );
    assert.doesNotMatch(rendered, /\$marker_path\.\$\$/);
  } finally {
    fixture.cleanup();
  }
});

test("rendered CLI installer rejects non-regular managed marker destinations", () => {
  const markerCases = [
    {
      name: "directory",
      create(markerPath) {
        mkdirSync(markerPath);
      },
      verify({ markerPath }) {
        assert.deepEqual(readdirSync(markerPath), []);
      },
    },
    {
      name: "symlink",
      create(markerPath, sandbox) {
        const target = path.join(sandbox, "hostile-marker-target");
        writeFileSync(target, "hostile target");
        symlinkSync(target, markerPath);
        return target;
      },
      verify({ target }) {
        assert.equal(readFileSync(target, "utf8"), "hostile target");
      },
    },
  ];
  if (spawnSync("sh", ["-c", "command -v mkfifo >/dev/null 2>&1"]).status === 0) {
    markerCases.push({
      name: "FIFO",
      create(markerPath) {
        const result = spawnSync("mkfifo", [markerPath], { encoding: "utf8" });
        assert.equal(result.status, 0, result.stderr);
      },
      verify() {},
    });
  }

  for (const markerCase of markerCases) {
    let target;
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--no-man"],
      installDirOnPath: true,
      prepareInstall({ installBin, sandbox }) {
        target = markerCase.create(path.join(installBin, "ctx.install.json"), sandbox);
      },
    });
    const markerPath = path.join(fixture.installBin, "ctx.install.json");
    try {
      assert.notEqual(fixture.result.status, 0, markerCase.name);
      assert.match(
        fixture.result.stderr,
        /managed install marker destination is not a regular file/,
        markerCase.name,
      );
      markerCase.verify({ markerPath, target });
      assert.deepEqual(
        readdirSync(fixture.installBin).filter((name) =>
          name.startsWith("ctx.install.json.tmp.")
        ),
        [],
        markerCase.name,
      );
    } finally {
      fixture.cleanup();
    }
  }
});
