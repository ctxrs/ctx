import {
  assert,
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  path,
  readFileSync,
  readOwnershipRecords,
  rmSync,
  runHostedUninstallForInstallerFixture,
  runRenderedCliInstaller,
  sha256,
  statSync,
  symlinkSync,
  test,
  tmpdir,
  writeFileSync,
  writeExecutable,
} from "./cli-install-test-helpers.mjs";

export function registerCliInstallShellPathManTests() {
  test("rendered CLI installer binds generated man pages, profile block, and skill digest ownership", () => {
    const fixture = runRenderedCliInstaller();
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const markerPath = path.join(fixture.installBin, "ctx.install.json");
      const marker = JSON.parse(readFileSync(markerPath, "utf8"));
      const ownership = readFileSync(marker.integrations_path, "utf8");
      assert.equal(marker.integrations_path, path.join(fixture.installBin, "ctx.install-integrations"));
      assert.equal(marker.integrations_sha256, sha256(ownership));
      assert.deepEqual(marker.man_pages, {
        schema_version: 1,
        status: "installed",
        directory: fixture.manDir,
        files: [
          { name: "ctx-search.1", sha256: sha256(".TH ctx-search 1\n") },
          { name: "ctx.1", sha256: sha256(".TH ctx 1\n") },
        ],
        binary_sha256: sha256(readFileSync(path.join(fixture.installBin, "ctx"))),
      });
      assert.ok(
        readFileSync(markerPath, "utf8").includes(
          `  "man_pages": ${JSON.stringify(marker.man_pages)},\n`,
        ),
        "the installed receipt must stay compact",
      );
      assert.match(ownership, new RegExp(`man\\t[0-9a-f]{64}\\t${fixture.manDir}/ctx\\.1`));
      assert.match(ownership, new RegExp(`man\\t[0-9a-f]{64}\\t${fixture.manDir}/ctx-search\\.1`));
      assert.match(
        ownership,
        new RegExp(`skill\\t[0-9a-f]{64}\\t${fixture.homeDir}/\\.agents/skills/ctx-agent-history-search`),
      );
      assert.match(
        ownership,
        new RegExp(`profile-file\\t[0-9a-f]{64}\\t${fixture.homeDir}/\\.bashrc`),
      );
      assert.match(readFileSync(path.join(fixture.homeDir, ".bashrc"), "utf8"), /^# >>> ctx installer PATH setup >>>$/m);
      assert.match(readFileSync(path.join(fixture.homeDir, ".bashrc"), "utf8"), /^# <<< ctx installer PATH setup <<<$/m);
    } finally {
      fixture.cleanup();
    }
  });

  test("rendered CLI installer preserves quoting in skill ownership and prints a quoted PATH command", () => {
    const sandboxRoot = mkdtempSync(path.join(tmpdir(), "ctx-installer-output-"));
    const quotedBin = path.join(sandboxRoot, 'bin with "quote');
    const quotedSkill = path.join(sandboxRoot, 'agent skills "owned', "skills", "ctx-agent-history-search");
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--skill-agent", "codex", "--no-man"],
      env: {
        CTX_BIN_DIR: quotedBin,
        CTX_FAKE_SKILL_PATH: quotedSkill,
      },
    });
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const marker = JSON.parse(readFileSync(path.join(quotedBin, "ctx.install.json"), "utf8"));
      const ownership = readFileSync(marker.integrations_path, "utf8");
      assert.ok(ownership.includes(`\t${quotedSkill}\n`), ownership);
      const outputLines = fixture.result.stderr.trimEnd().split("\n");
      const heading = outputLines.indexOf("To use the newly installed ctx in this shell, run:");
      assert.ok(heading >= 0, fixture.result.stderr);
      assert.equal(outputLines[heading + 1], `  export PATH="${quotedBin.replaceAll('"', '\\"')}:$PATH"`);
      assert.match(
        fixture.result.stderr,
        /New terminal sessions will include it automatically\./,
      );
    } finally {
      fixture.cleanup();
      rmSync(sandboxRoot, { recursive: true, force: true });
    }
  });

  test("rendered CLI installer rejects control characters in a man directory", () => {
    const fixture = runRenderedCliInstaller({
      env: { CTX_MAN_DIR: `${tmpdir()}/ctx-man\u0001dir` },
    });
    try {
      assert.notEqual(fixture.result.status, 0);
      assert.match(fixture.result.stderr, /paths must not contain control characters/u);
    } finally {
      fixture.cleanup();
    }
  });

  test("rendered CLI installer is silent when bare ctx resolves to the installed executable", () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--no-skill", "--no-man"],
      installDirOnPath: true,
    });
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      assert.doesNotMatch(fixture.result.stderr, /newly installed ctx in this shell/);
      assert.equal(existsSync(path.join(fixture.homeDir, ".bashrc")), false);
      const markerPath = path.join(fixture.installBin, "ctx.install.json");
      const markerText = readFileSync(markerPath, "utf8");
      const marker = JSON.parse(markerText);
      assert.deepEqual(marker.man_pages, {
        schema_version: 1,
        status: "disabled",
      });
      assert.ok(
        markerText.includes('  "man_pages": {"schema_version":1,"status":"disabled"},\n'),
        "the disabled receipt must stay compact",
      );
      if (existsSync(fixture.commandLogPath)) {
        assert.doesNotMatch(readFileSync(fixture.commandLogPath, "utf8"), /^docs man /mu);
      }
    } finally {
      fixture.cleanup();
    }
  });

  test("rendered CLI installer uses the absolute new executable when an older ctx wins PATH", () => {
    let oldCtxInvoked;
    const fixture = runRenderedCliInstaller({
      args: ["--no-skill", "--no-man"],
      prepareInstall: ({ fakeBin, sandbox }) => {
        oldCtxInvoked = path.join(sandbox, "old-ctx-invoked");
        writeExecutable(
          path.join(fakeBin, "ctx"),
          `#!/bin/sh\nprintf old > '${oldCtxInvoked}'\nexit 99\n`,
        );
      },
    });
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      assert.equal(existsSync(oldCtxInvoked), false, "the older PATH ctx must never run");
      assert.deepEqual(
        readFileSync(fixture.commandLogPath, "utf8").trim().split("\n"),
        ["setup --quiet --format json --wait --progress none"],
      );
      assert.match(
        fixture.result.stderr,
        /To use the newly installed ctx in this shell, run:/,
      );
      assert.ok(
        fixture.result.stderr.includes(`  export PATH="${fixture.installBin}:$PATH"`),
        fixture.result.stderr,
      );
      assert.doesNotMatch(fixture.result.stderr, /warning:.*PATH/i);
      assert.match(
        readFileSync(path.join(fixture.homeDir, ".bashrc"), "utf8"),
        /case "\$\{PATH\}:" in\n  "[^"]+:"\*\) ;;/,
      );
    } finally {
      fixture.cleanup();
    }
  });

  test("rendered CLI installer reports the exact default PATH handoff before ctx commands", () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-skill", "--no-man"],
      installBinPath: ".local/bin",
    });
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const output = fixture.result.stderr;
      const pathMessage = `To use the newly installed ctx in this shell, run:
  export PATH="$HOME/.local/bin:$PATH"

New terminal sessions will include it automatically.`;
      assert.match(output, new RegExp(pathMessage.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
      assert.ok(
        output.indexOf(pathMessage) < output.indexOf('  Search:    ctx search "test failure"'),
        output,
      );
    } finally {
      fixture.cleanup();
    }
  });

  test("rendered CLI installer does not promise future PATH updates when profile persistence fails", () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--no-skill", "--no-man"],
      prepareInstall: ({ homeDir, sandbox }) => {
        const profileTarget = path.join(sandbox, "user-profile");
        writeFileSync(profileTarget, "# user profile\n");
        symlinkSync(profileTarget, path.join(homeDir, ".bashrc"));
      },
    });
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      assert.match(
        fixture.result.stderr,
        new RegExp(`To add it for future terminal sessions, add ${fixture.installBin} to your shell profile\\.`),
      );
      assert.doesNotMatch(fixture.result.stderr, /New terminal sessions will include it automatically\./);
    } finally {
      fixture.cleanup();
    }
  });

  test("rendered CLI installer keeps identical unmanaged man pages current and unowned", () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--no-skill"],
      prepareInstall: ({ manDir }) => {
        writeFileSync(path.join(manDir, "ctx.1"), ".TH ctx 1\n");
        writeFileSync(path.join(manDir, "ctx-search.1"), ".TH ctx-search 1\n");
      },
    });
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      assert.deepEqual(readOwnershipRecords(fixture.installBin), [
        {
          kind: "profile-file",
          digest: sha256(readFileSync(path.join(fixture.homeDir, ".bashrc"), "utf8")),
          target: path.join(fixture.homeDir, ".bashrc"),
        },
      ]);
      const marker = JSON.parse(readFileSync(path.join(fixture.installBin, "ctx.install.json"), "utf8"));
      assert.equal(Object.hasOwn(marker, "man_pages"), false);
      assert.doesNotMatch(fixture.result.stderr, /Man page/u);
      const uninstall = runHostedUninstallForInstallerFixture(fixture);
      assert.equal(uninstall.status, 0, uninstall.stderr);
      assert.equal(readFileSync(path.join(fixture.manDir, "ctx.1"), "utf8"), ".TH ctx 1\n");
      assert.equal(readFileSync(path.join(fixture.manDir, "ctx-search.1"), "utf8"), ".TH ctx-search 1\n");
    } finally {
      fixture.cleanup();
    }
  });

  test("rendered CLI installer keeps a partial man-page setup nonfatal and receipt-less", () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--no-skill"],
      prepareInstall: ({ manDir }) => {
        writeFileSync(path.join(manDir, "ctx.1"), ".TH user-ctx 1\n");
      },
    });
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      assert.doesNotMatch(fixture.result.stderr, /Man page/u);
      assert.equal(readFileSync(path.join(fixture.manDir, "ctx.1"), "utf8"), ".TH user-ctx 1\n");
      assert.equal(readFileSync(path.join(fixture.manDir, "ctx-search.1"), "utf8"), ".TH ctx-search 1\n");
      const ownershipRecords = readOwnershipRecords(fixture.installBin);
      assert.deepEqual(ownershipRecords.map(({ kind }) => kind), [
        "man",
        "profile-file",
      ]);
      assert.deepEqual(
        ownershipRecords
          .filter(({ kind }) => kind === "man")
          .map(({ target }) => target),
        [path.join(fixture.manDir, "ctx-search.1")],
      );
      const marker = JSON.parse(readFileSync(path.join(fixture.installBin, "ctx.install.json"), "utf8"));
      assert.equal(Object.hasOwn(marker, "man_pages"), false);
    } finally {
      fixture.cleanup();
    }
  });

  test("rendered CLI installer preserves symlinked and nonregular unmanaged man pages", () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--no-skill"],
      prepareInstall: ({ manDir, sandbox }) => {
        const userPage = path.join(sandbox, "user-ctx.1");
        writeFileSync(userPage, ".TH user-ctx 1\n");
        symlinkSync(userPage, path.join(manDir, "ctx.1"));
        mkdirSync(path.join(manDir, "ctx-search.1"));
      },
    });
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      assert.equal(readFileSync(path.join(fixture.manDir, "ctx.1"), "utf8"), ".TH user-ctx 1\n");
      assert.equal(statSync(path.join(fixture.manDir, "ctx-search.1")).isDirectory(), true);
      assert.deepEqual(readOwnershipRecords(fixture.installBin).map(({ kind }) => kind), ["profile-file"]);
      const marker = JSON.parse(readFileSync(path.join(fixture.installBin, "ctx.install.json"), "utf8"));
      assert.equal(Object.hasOwn(marker, "man_pages"), false);
      assert.doesNotMatch(fixture.result.stderr, /Man page/u);
    } finally {
      fixture.cleanup();
    }
  });

  test("rendered CLI installer does not claim a group-writable man directory", () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--no-skill"],
      prepareInstall: ({ manDir }) => chmodSync(manDir, 0o777),
    });
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      assert.doesNotMatch(fixture.result.stderr, /Man page/u);
      const marker = JSON.parse(readFileSync(path.join(fixture.installBin, "ctx.install.json"), "utf8"));
      assert.equal(Object.hasOwn(marker, "man_pages"), false);
      assert.equal(existsSync(path.join(fixture.manDir, "ctx.1")), false);
    } finally {
      fixture.cleanup();
    }
  });
}
