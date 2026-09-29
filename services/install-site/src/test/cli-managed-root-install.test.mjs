import {
  assert, existsSync, mkdirSync, path, readCtxCommands, readFileSync,
  runRenderedCliInstaller, runHostedUninstallForInstallerFixture, test, writeFileSync,
} from "./cli-install-test-helpers.mjs";

function relocatedFixture(options = {}) {
  return runRenderedCliInstaller({
    args: ["--no-skill", "--no-man", "--no-modify-path"],
    env: { CTX_DATA_ROOT: "", ...options.env },
    prepareInstall({ homeDir }) {
      mkdirSync(path.join(homeDir, ".ctx-control"), { mode: 0o700 });
      const root = path.join(homeDir, "relocated data");
      mkdirSync(root, { mode: 0o700 });
      writeFileSync(path.join(homeDir, ".ctx-control/data-root.json"), JSON.stringify({
        schema_version: 1, path: root, install_id: "63e7b607-52b5-47b5-8479-2f0c5c3aac75",
      }));
      writeFileSync(path.join(root, "config.toml"),
        options.config ?? '[indexing]\nmode = "manual"\n');
      // A retained backup must not determine the relocated installation's policy.
      mkdirSync(path.join(homeDir, ".ctx"));
      writeFileSync(path.join(homeDir, ".ctx/config.toml"), '[search]\nsemantic = true\n');
    },
  });
}

test("fresh reinstall obtains relocated policy from the verified candidate before publication", () => {
  const fixture = relocatedFixture();
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stdout + fixture.result.stderr);
    const calls = readCtxCommands(fixture.commandLogPath);
    assert.equal(calls[0], "data-root show");
    assert.ok(existsSync(path.join(fixture.installBin, "ctx")));
    assert.ok(!existsSync(fixture.runtimeRepairLogPath));
    assert.equal(readFileSync(path.join(fixture.homeDir, "relocated data/config.toml"), "utf8"),
      '[indexing]\nmode = "manual"\n');
  } finally { fixture.cleanup(); }
});

test("relocated semantic preference still triggers runtime repair", () => {
  const fixture = relocatedFixture({ config: '[search]\nsemantic = true\n' });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stdout + fixture.result.stderr);
    assert.ok(existsSync(fixture.runtimeRepairLogPath));
  } finally { fixture.cleanup(); }
});

test("unavailable relocated root prevents installer publication", () => {
  const fixture = relocatedFixture({ env: { CTX_FAKE_MANAGED_ROOT_UNAVAILABLE: "1" } });
  try {
    assert.notEqual(fixture.result.status, 0);
    assert.match(fixture.result.stderr, /could not resolve the managed data root/);
    assert.ok(!existsSync(path.join(fixture.installBin, "ctx")));
    assert.ok(!existsSync(path.join(fixture.installBin, "ctx.install.json")));
  } finally { fixture.cleanup(); }
});

test("malformed relocated config fails before installer publication", () => {
  const fixture = relocatedFixture({ config: '[indexing]\nmode = "invalid"\n' });
  try {
    assert.notEqual(fixture.result.status, 0);
    assert.ok(!existsSync(path.join(fixture.installBin, "ctx")));
  } finally { fixture.cleanup(); }
});


test("uninstall resolves the relocated root without a shell override and preserves history", () => {
  const fixture = relocatedFixture();
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stdout + fixture.result.stderr);
    const result = runHostedUninstallForInstallerFixture(fixture, ["--keep-data"], { CTX_DATA_ROOT: "" });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.ok(readCtxCommands(fixture.commandLogPath).includes(
      `--data-root ${fixture.homeDir}/relocated data daemon disable --prepare-uninstall --format=json`));
    assert.ok(existsSync(path.join(fixture.homeDir, "relocated data/config.toml")));
    assert.ok(!existsSync(path.join(fixture.installBin, "ctx")));
  } finally { fixture.cleanup(); }
});

test("lock-only control directory remains compatible with an older installer and uninstaller", () => {
  const fixture = runRenderedCliInstaller({
    releaseVersion: "2.0.5",
    args: ["--no-skill", "--no-man", "--no-modify-path"],
    env: { CTX_DATA_ROOT: "", CTX_FAKE_MANAGED_ROOT_UNAVAILABLE: "1" },
    prepareInstall({ homeDir }) {
      mkdirSync(path.join(homeDir, ".ctx-control"), { mode: 0o700 });
      writeFileSync(path.join(homeDir, ".ctx-control/admission.lock"), "");
      mkdirSync(path.join(homeDir, ".ctx"));
      writeFileSync(path.join(homeDir, ".ctx/config.toml"), '[indexing]\nmode = "manual"\n');
    },
  });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stdout + fixture.result.stderr);
    const result = runHostedUninstallForInstallerFixture(fixture, ["--keep-data"], { CTX_DATA_ROOT: "" });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.ok(!readCtxCommands(fixture.commandLogPath).includes("data-root show"));
    assert.ok(existsSync(path.join(fixture.homeDir, ".ctx/config.toml")));
    assert.ok(!existsSync(path.join(fixture.installBin, "ctx")));
  } finally { fixture.cleanup(); }
});

test("unresolved existing locator prevents uninstall from selecting the old default", () => {
  const fixture = relocatedFixture();
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stdout + fixture.result.stderr);
    const result = runHostedUninstallForInstallerFixture(fixture, ["--keep-data"], {
      CTX_DATA_ROOT: "", CTX_FAKE_MANAGED_ROOT_UNAVAILABLE: "1",
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /could not resolve the managed data root/);
    assert.ok(existsSync(path.join(fixture.installBin, "ctx")));
    assert.ok(existsSync(path.join(fixture.homeDir, "relocated data/config.toml")));
  } finally { fixture.cleanup(); }
});
