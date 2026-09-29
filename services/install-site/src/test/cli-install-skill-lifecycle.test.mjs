import {
  assert, existsSync, mkdirSync, mkdtempSync, path, readFileSync, readOwnershipRecords,
  rmSync, runHostedUninstallForInstallerFixture, runRenderedCliInstaller, sha256,
  test, tmpdir, writeFileSync,
} from "./cli-install-test-helpers.mjs";

for (const name of ["ctx", "ctx-agent-history-search"]) {
  for (const scenario of ["exact", "modified", "wrong_name"]) {
    test(`hosted skill lifecycle ${name}: ${scenario}`, () => {
      const root = mkdtempSync(path.join(tmpdir(), "ctx-owned-skill-"));
      const skillPath = path.join(root, scenario === "exact" ? ".agents" : ".claude", "skills", name);
      const bodyPath = path.join(skillPath, "SKILL.md");
      const markerPath = path.join(skillPath, ".ctx-skill.json");
      const body = "# Synthetic installed skill\n";
      const marker = JSON.stringify({
        schema_version: 1, installer: "ctx-cli",
        skill_name: scenario === "wrong_name" ? "unrelated" : name,
        skill_hash: `sha256:${sha256(body)}`,
      }, null, 2) + "\n";
      const fixture = runRenderedCliInstaller({
        args: ["--no-setup", "--no-man"],
        env: { CTX_FAKE_SKILL_PATH: skillPath, CTX_FAKE_SKILL_PRESERVE_EXISTING: "1" },
        prepareInstall: () => {
          mkdirSync(skillPath, { recursive: true });
          writeFileSync(bodyPath, body);
          writeFileSync(markerPath, marker);
        },
      });
      try {
        assert.equal(fixture.result.status, 0, fixture.result.stderr);
        const records = readOwnershipRecords(fixture.installBin).filter(r => r.kind === "skill");
        assert.deepEqual(records, scenario === "wrong_name" ? [] : [
          { kind: "skill", digest: sha256(body + marker), target: skillPath },
        ]);
        if (scenario === "modified") writeFileSync(bodyPath, body + "User customization\n");
        if (scenario === "exact") {
          // Ownership must also survive the prior-receipt validation on reinstall.
          const retry = fixture.rerun(["--no-setup", "--no-man", "--no-skill"]);
          assert.equal(retry.status, 0, retry.stderr);
          assert.deepEqual(readOwnershipRecords(fixture.installBin).filter(r => r.kind === "skill"), records);
        }
        const uninstall = runHostedUninstallForInstallerFixture(fixture);
        assert.equal(uninstall.status, 0, uninstall.stderr);
        assert.equal(existsSync(bodyPath), scenario !== "exact");
        assert.equal(existsSync(markerPath), scenario !== "exact");
        if (scenario !== "exact") {
          assert.equal(readFileSync(bodyPath, "utf8"), body + (scenario === "modified" ? "User customization\n" : ""));
          assert.equal(readFileSync(markerPath, "utf8"), marker);
        }
      } finally {
        fixture.cleanup();
        rmSync(root, { recursive: true, force: true });
      }
    });
  }
}
