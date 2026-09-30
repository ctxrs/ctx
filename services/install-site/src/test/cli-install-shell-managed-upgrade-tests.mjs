import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import { truncateSync } from "node:fs";

import {
  assert,
  chmodSync,
  MANAGED_PAIR_RECONCILE_COMMAND,
  path,
  readCtxCommands,
  readOrderedCtxCommands,
  readFileSync,
  readOwnershipRecords,
  readdirSync,
  renderCliInstallScript,
  runRenderedCliInstaller,
  signMetadataBase64,
  sha256,
  statSync,
  test,
  writeFileSync,
} from "./cli-install-test-helpers.mjs";

function stageUnifiedTarget(fixture, priorVersion, version, size = null) {
  const priorDigest = sha256(readFileSync(fixture.artifactPath));
  writeFileSync(fixture.artifactPath, readFileSync(fixture.artifactPath, "utf8")
    .replace(`fixture_version=${priorVersion}`, `fixture_version=${version}`));
  if (size !== null) truncateSync(fixture.artifactPath, size);
  const digest = execFileSync("sha256sum", [fixture.artifactPath], {
    encoding: "utf8",
  }).split(" ")[0];
  const metadata = readFileSync(fixture.childEnv.CTX_FAKE_METADATA, "utf8")
    .replace(`CTX_RELEASE_VERSION=${priorVersion}`, `CTX_RELEASE_VERSION=${version}`)
    .replaceAll(priorDigest, digest);
  writeFileSync(fixture.childEnv.CTX_FAKE_METADATA, metadata);
  writeFileSync(fixture.childEnv.CTX_FAKE_METADATA_SIGNATURE,
    `${signMetadataBase64(metadata, fixture.metadataPrivateKeyPem)}\n`);
  return digest;
}

export function registerCliInstallShellManagedUpgradeTests() {
  test("verified modern candidate recovers the complete wrong-target 2.0.2 lifecycle receipt", () => {
    const fixture = runRenderedCliInstaller({
      releaseVersion: "2.0.2",
      compressedArtifact: "missing",
      rawConfig: '[indexing]\nmode = "manual"\n[analytics]\nenabled = false\n[upgrade]\nauto = false\n[unknown]\nkeep = "value"\n',
    });
    const binaryPath = path.join(fixture.installBin, "ctx");
    const markerPath = `${binaryPath}.install.json`;
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const priorBinary = readFileSync(binaryPath);
      const priorMarker = readFileSync(markerPath);
      const priorRecords = readOwnershipRecords(fixture.installBin);
      const priorConfig = readFileSync(fixture.configPath);
      const priorProfile = readFileSync(path.join(fixture.homeDir, ".bashrc"));
      const targetDigest = stageUnifiedTarget(fixture, "2.0.2", "2.2.1");
      assert.ok(statSync(fixture.artifactPath).size < 134217728);
      // Model the complete released v2 receipt, including its frozen latest
      // version. Malformed JSON does not reproduce the discovery mismatch.
      const frozenReceipt = {
        schema_version: 1, command: "upgrade", ok: true, status: "up_to_date",
        message: "ctx is up to date", current_version: "2.0.2", latest_version: "1.6.5",
        update_available: false, update_was_available: false, channel: "stable",
        platform: "linux-x64",
        metadata_url: "https://cli.ctx.rs/functions/v2/releases/stable/ctx-release-metadata.env",
        artifact_url: "https://example.test/releases/ctx-linux-x64", install_path: binaryPath,
        managed: true, applied: false, dry_run: false, warnings: [], upgrade_attempt_id: null,
      };
      const env = { CTX_FAKE_MANAGED_UPGRADE_RESULT: JSON.stringify(frozenReceipt) };
      const oldAttempt = spawnSync(binaryPath, ["upgrade", "--channel", "stable", "--format=json"], {
        encoding: "utf8", env: { ...fixture.childEnv, ...env },
      });
      assert.equal(oldAttempt.status, 0, oldAttempt.stderr);
      assert.deepEqual(JSON.parse(oldAttempt.stdout), frozenReceipt);
      assert.deepEqual(readFileSync(binaryPath), priorBinary);
      assert.deepEqual(readFileSync(markerPath), priorMarker);
      const commandsBefore = readOrderedCtxCommands(fixture).length;
      const signaturePath = fixture.childEnv.CTX_FAKE_METADATA_SIGNATURE;
      const signature = readFileSync(signaturePath);
      writeFileSync(signaturePath, "AAAA\n");
      const unauthenticated = fixture.rerun(["--no-setup", "--no-skill", "--no-modify-path"], env);
      assert.notEqual(unauthenticated.status, 0);
      assert.match(unauthenticated.stderr, /signature/u);
      assert.deepEqual(readFileSync(binaryPath), priorBinary);
      assert.deepEqual(readFileSync(markerPath), priorMarker);
      assert.equal(readOrderedCtxCommands(fixture).length, commandsBefore,
        "neither candidate nor installed owner may run before authentication");
      writeFileSync(signaturePath, signature);
      const recovered = fixture.rerun(["--no-setup", "--no-skill", "--no-modify-path"], {
        ...env, CTX_FAKE_LOG_MUTATIONS: "1", CTX_INSTALL_NO_DAEMON: "1",
      });
      assert.equal(recovered.status, 0, recovered.stderr);
      assert.equal(sha256(readFileSync(binaryPath)), targetDigest);
      const marker = JSON.parse(readFileSync(markerPath, "utf8"));
      assert.equal(marker.version, "2.2.1");
      assert.equal(marker.sha256, targetDigest);
      assert.deepEqual(marker.man_pages, JSON.parse(priorMarker).man_pages);
      assert.deepEqual(readOwnershipRecords(fixture.installBin), priorRecords);
      // This authored CLI does not migrate schemas; real-binary checks compare
      // effective opt-outs and unknown values rather than config byte identity.
      assert.deepEqual(readFileSync(fixture.configPath), priorConfig);
      assert.deepEqual(readFileSync(path.join(fixture.homeDir, ".bashrc")), priorProfile);
      const commands = readOrderedCtxCommands(fixture).slice(commandsBefore);
      assert.match(commands[0], /^upgrade --hosted-transaction migrate --install-path /u);
      assert.deepEqual(commands.slice(1), [MANAGED_PAIR_RECONCILE_COMMAND]);
    } finally {
      fixture.cleanup();
    }
  });

  test("modern candidate stages --no-man only for an existing receipt", () => {
    for (const priorReceipt of ["installed", "disabled", "absent"]) {
      const fixture = runRenderedCliInstaller({ releaseVersion: "2.0.5", compressedArtifact: "missing" });
      const binaryPath = path.join(fixture.installBin, "ctx");
      const markerPath = `${binaryPath}.install.json`;
      try {
        assert.equal(fixture.result.status, 0, fixture.result.stderr);
        const marker = JSON.parse(readFileSync(markerPath, "utf8"));
        const pagePath = path.join(fixture.manDir, "ctx.1");
        writeFileSync(pagePath, "user-edited manual page\n");
        if (priorReceipt === "absent") delete marker.man_pages;
        if (priorReceipt === "disabled") marker.man_pages = { schema_version: 1, status: "disabled" };
        writeFileSync(markerPath, `${JSON.stringify(marker, null, 2)}\n`);
        stageUnifiedTarget(fixture, "2.0.5", "2.2.1");
        const commandsBefore = readOrderedCtxCommands(fixture).length;
        const recovered = fixture.rerun(["--no-man", "--no-setup", "--no-skill", "--no-modify-path"], {
          CTX_FAKE_LOG_MUTATIONS: "1",
          CTX_FAKE_MANAGED_UPGRADE_STATUS: "73",
        });
        assert.equal(recovered.status, 0, recovered.stderr);
        const after = JSON.parse(readFileSync(markerPath, "utf8"));
        assert.deepEqual(after.man_pages, priorReceipt === "absent"
          ? undefined : { schema_version: 1, status: "disabled" });
        assert.equal(readFileSync(pagePath, "utf8"), "user-edited manual page\n");
        const commands = readOrderedCtxCommands(fixture).slice(commandsBefore);
        assert.match(commands[0], /^upgrade --hosted-transaction migrate --install-path /u);
        assert.deepEqual(commands.slice(1), [MANAGED_PAIR_RECONCILE_COMMAND]);
      } finally {
        fixture.cleanup();
      }
    }
  });

  test("installer retry retains prior integration ownership after binary publication fault", () => {
    const fixture = runRenderedCliInstaller({
      releaseVersion: "1.6.3",
      compressedArtifact: "missing",
    });
    const binaryPath = path.join(fixture.installBin, "ctx");
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const priorRecords = readOwnershipRecords(fixture.installBin);
      assert.ok(priorRecords.length > 0);
      const smallDigest = sha256(readFileSync(fixture.artifactPath));
      const largeDigest = stageUnifiedTarget(fixture, "1.6.3", "2.2.1", 134217729);
      assert.ok(statSync(fixture.artifactPath).size > 134217728);

      const args = ["--no-setup", "--no-skill", "--no-man"];
      const fault = fixture.rerun(args, {
        CTX_FAKE_HOSTED_PARTIAL_FAILURE: "1",
        CTX_INSTALL_NO_DAEMON: "1",
      });
      assert.notEqual(fault.status, 0);
      assert.equal(sha256(readFileSync(binaryPath)), largeDigest);
      assert.equal(JSON.parse(readFileSync(`${binaryPath}.install.json`, "utf8")).sha256,
        smallDigest);
      assert.equal(JSON.parse(readFileSync(`${binaryPath}.install.json`, "utf8")).man_pages.status,
        "installed", "--no-man must remain staged until the transaction commits");
      assert.ok(statSync(path.join(fixture.installBin,
        ".ctx.hosted-install-transaction.json")).isFile());
      const retry = fixture.rerun(["--no-setup", "--no-skill"], { CTX_INSTALL_NO_DAEMON: "1" });
      assert.equal(retry.status, 0, retry.stderr);
      assert.equal(JSON.parse(readFileSync(`${binaryPath}.install.json`, "utf8")).sha256,
        largeDigest);
      assert.deepEqual(readOwnershipRecords(fixture.installBin), priorRecords);
      assert.equal(JSON.parse(readFileSync(`${binaryPath}.install.json`, "utf8")).man_pages.status,
        "disabled", "retry retains the original attempt's opt-out");
    } finally {
      fixture.cleanup();
    }
  });

  test("shipped 128 MiB updater reaches a larger signed target through installer migration", () => {
    const fixture = runRenderedCliInstaller({
      releaseVersion: "1.6.3",
      compressedArtifact: "missing",
    });
    const binaryPath = path.join(fixture.installBin, "ctx");
    const markerPath = `${binaryPath}.install.json`;
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const oldBinary = readFileSync(binaryPath);
      const oldMarker = readFileSync(markerPath);
      const largeDigest = stageUnifiedTarget(fixture, "1.6.3", "2.2.1", 134217729);

      const oldAttempt = spawnSync(binaryPath,
        ["upgrade", "--channel", "stable", "--format=json"], {
          encoding: "utf8",
          env: { ...fixture.childEnv, CTX_FAKE_ENFORCE_OLD_CAP: "1" },
        });
      assert.equal(oldAttempt.status, 74, oldAttempt.stderr);
      assert.match(oldAttempt.stderr, /above 128 MiB/u);
      assert.deepEqual(readFileSync(binaryPath), oldBinary);
      assert.deepEqual(readFileSync(markerPath), oldMarker);

      const migrated = fixture.rerun([], {
        CTX_FAKE_ENFORCE_OLD_CAP: "1",
        CTX_FAKE_LOG_MUTATIONS: "1",
      });
      assert.equal(migrated.status, 0, migrated.stderr);
      assert.equal(statSync(binaryPath).size, 134217729);
      assert.equal(JSON.parse(readFileSync(markerPath, "utf8")).sha256, largeDigest);
      assert.match(readFileSync(fixture.commandLogPath, "utf8"),
        /upgrade --hosted-transaction migrate --install-path/u);
    } finally {
      fixture.cleanup();
    }
  });

  test("managed installer rerun delegates replacement without mutating an up-to-date image", () => {
    const fixture = runRenderedCliInstaller();
    const binaryPath = path.join(fixture.installBin, "ctx");
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const binaryBefore = readFileSync(binaryPath);
      const inodeBefore = statSync(binaryPath).ino;
      const markerPath = `${binaryPath}.install.json`;
      const markerBefore = JSON.parse(readFileSync(markerPath, "utf8"));
      markerBefore.core_transaction_identity = "preserve-me";
      writeFileSync(markerPath, `${JSON.stringify(markerBefore, null, 2)}\n`);

      const rerun = fixture.rerun();
      assert.equal(rerun.status, 0, rerun.stderr);
      assert.doesNotMatch(rerun.stderr, /ctx pro|trial/u);
      assert.deepEqual(readFileSync(binaryPath), binaryBefore);
      assert.equal(statSync(binaryPath).ino, inodeBefore);
      const markerAfter = JSON.parse(readFileSync(markerPath, "utf8"));
      assert.equal(
        markerAfter.install_attempt_id,
        markerBefore.install_attempt_id,
        "hosted augmentation must preserve Core install attribution",
      );
      assert.equal(
        markerAfter.core_transaction_identity,
        "preserve-me",
        "hosted augmentation must preserve Core marker fields",
      );
      assert.equal(
        markerAfter.integrations_path,
        `${binaryPath}.install-integrations.${markerAfter.integrations_sha256}`,
      );
      assert.deepEqual(markerAfter.man_pages, markerBefore.man_pages);
      assert.equal(markerAfter.man_pages.status, "installed");
      assert.deepEqual(markerAfter.man_pages.files.map(({ name }) => name), [
        "ctx-search.1",
        "ctx.1",
      ]);
      const rerunCommands = readOrderedCtxCommands(fixture).slice(-4);
      assert.doesNotMatch(
        readFileSync(fixture.commandLogPath, "utf8"),
        /upgrade --channel stable --format=json\ndocs man --out /u,
      );
      assert.deepEqual(rerunCommands, [
        "upgrade --channel stable --format=json",
        "integrations install skills --format=json",
        "setup --quiet --format json --wait --progress none",
        MANAGED_PAIR_RECONCILE_COMMAND,
      ]);
      assert.deepEqual(
        readdirSync(fixture.installBin).filter((name) => name.startsWith("ctx.tmp.")),
        [],
      );
    } finally {
      fixture.cleanup();
    }
  });

  test("managed installer rerun with Semantic enabled consumes typed upgrade proof", () => {
    const fixture = runRenderedCliInstaller({
      env: { CTX_SEARCH_SEMANTIC: "1" },
    });
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);

      const rerun = fixture.rerun();
      assert.equal(rerun.status, 0, rerun.stderr);
      const rerunCommands = readOrderedCtxCommands(fixture).slice(-5);
      assert.deepEqual(rerunCommands, [
        "upgrade --channel stable --format=json",
        "upgrade --channel stable --format=json",
        "integrations install skills --format=json",
        "setup --quiet --format json --wait --semantic --progress none",
        MANAGED_PAIR_RECONCILE_COMMAND,
      ]);
    } finally {
      fixture.cleanup();
    }
  });

  test("managed installer rerun with --no-man disables future reconciliation without replacing pages", () => {
    const fixture = runRenderedCliInstaller();
    const binaryPath = path.join(fixture.installBin, "ctx");
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const markerBefore = JSON.parse(readFileSync(`${binaryPath}.install.json`, "utf8"));
      const pagesBefore = new Map(markerBefore.man_pages.files.map(({ name }) => [
        name,
        readFileSync(path.join(fixture.manDir, name)),
      ]));
      // Core writes marker extensions as pretty nested JSON.
      writeFileSync(
        `${binaryPath}.install.json`,
        `${JSON.stringify(markerBefore, null, 2)}\n`,
      );

      const rerun = fixture.rerun(["--no-man"]);
      assert.equal(rerun.status, 0, rerun.stderr);
      const markerText = readFileSync(`${binaryPath}.install.json`, "utf8");
      const markerAfter = JSON.parse(markerText);
      assert.deepEqual(markerAfter.man_pages, {
        schema_version: 1,
        status: "disabled",
      });
      for (const [name, bytes] of pagesBefore) {
        assert.deepEqual(readFileSync(path.join(fixture.manDir, name)), bytes);
      }
      assert.deepEqual(readOrderedCtxCommands(fixture).slice(-5), [
        "--ctx-core-disable-managed-man-pages-v1",
        "upgrade --channel stable --format=json",
        "integrations install skills --format=json",
        "setup --quiet --format json --wait --progress none",
        MANAGED_PAIR_RECONCILE_COMMAND,
      ]);
      const rendered = renderCliInstallScript({ installAttemptId: "ia_managed_no_man_order" });
      assert.ok(
        rendered.lastIndexOf("disable_core_man_pages_before_upgrade") <
          rendered.lastIndexOf("run_managed_core_upgrade"),
        "managed --no-man must persist the locked opt-out before Core starts the upgrade",
      );
      assert.match(rendered, /"\$install_path" --ctx-core-disable-managed-man-pages-v1/u);
      assert.doesNotMatch(
        rendered.match(/disable_core_man_pages_before_upgrade\(\) \{[\s\S]*?^\}/m)?.[0] ?? "",
        /mktemp|mv -f|man_pages_json/u,
      );
      assert.doesNotMatch(rendered, /hosted-marker-finalize|marker_finalize/u);
    } finally {
      fixture.cleanup();
    }
  });

  test("managed reruns leave a legacy marker receipt-less", () => {
    const fixture = runRenderedCliInstaller();
    const binaryPath = path.join(fixture.installBin, "ctx");
    const markerPath = `${binaryPath}.install.json`;
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const marker = JSON.parse(readFileSync(markerPath, "utf8"));
      const pageNames = marker.man_pages.files.map(({ name }) => name);
      const pagesBefore = new Map(pageNames.map((name) => [
        name,
        readFileSync(path.join(fixture.manDir, name)),
      ]));
      delete marker.man_pages;
      writeFileSync(markerPath, `${JSON.stringify(marker, null, 2)}\n`);

      const rerun = fixture.rerun();
      assert.equal(rerun.status, 0, rerun.stderr);
      assert.equal(
        Object.hasOwn(JSON.parse(readFileSync(markerPath, "utf8")), "man_pages"),
        false,
      );

      const noManRerun = fixture.rerun(["--no-man"]);
      assert.equal(noManRerun.status, 0, noManRerun.stderr);
      assert.equal(
        Object.hasOwn(JSON.parse(readFileSync(markerPath, "utf8")), "man_pages"),
        false,
      );
      for (const [name, bytes] of pagesBefore) {
        assert.deepEqual(readFileSync(path.join(fixture.manDir, name)), bytes);
      }
    } finally {
      fixture.cleanup();
    }
  });

  test("pre-daemon managed bridge retains absent page ownership", () => {
    const fixture = runRenderedCliInstaller({ managedPair: true, releaseVersion: "1.4.12" });
    const binaryPath = path.join(fixture.installBin, "ctx");
    const markerPath = `${binaryPath}.install.json`;
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const marker = JSON.parse(readFileSync(markerPath, "utf8"));
      marker.version = "0.25.0";
      delete marker.man_pages;
      writeFileSync(markerPath, `${JSON.stringify(marker, null, 2)}\n`);

      const rerun = fixture.rerun(undefined, {
        CTX_FAKE_MAN_PAGE_SUFFIX: " updated",
      });
      assert.equal(rerun.status, 0, rerun.stderr);
      const markerAfter = JSON.parse(readFileSync(markerPath, "utf8"));
      assert.equal(Object.hasOwn(markerAfter, "man_pages"), false);
      assert.equal(
        readFileSync(path.join(fixture.manDir, "ctx.1"), "utf8"),
        ".TH ctx 1\n",
      );
      assert.equal(
        readFileSync(path.join(fixture.manDir, "ctx-search.1"), "utf8"),
        ".TH ctx-search 1\n",
      );
    } finally {
      fixture.cleanup();
    }
  });

  test("staging dogfood rerun verifies and retains the immutable installed image", () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-setup", "--no-skill", "--no-man"],
      stagingDogfood: true,
    });
    const binaryPath = path.join(fixture.installBin, "ctx");
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const binaryBefore = readFileSync(binaryPath);
      const inodeBefore = statSync(binaryPath).ino;
      const markerPath = `${binaryPath}.install.json`;
      const markerBefore = JSON.parse(readFileSync(markerPath, "utf8"));

      const rerun = fixture.rerun();
      assert.equal(rerun.status, 0, rerun.stderr);
      assert.deepEqual(readFileSync(binaryPath), binaryBefore);
      assert.equal(statSync(binaryPath).ino, inodeBefore);
      const markerAfter = JSON.parse(readFileSync(markerPath, "utf8"));
      const {
        integrations_path: priorIntegrationPath,
        integrations_sha256: priorIntegrationSha256,
        ...identityBefore
      } = markerBefore;
      const {
        integrations_path: integrationPath,
        integrations_sha256: integrationSha256,
        ...identityAfter
      } = markerAfter;
      assert.deepEqual(identityAfter, identityBefore);
      assert.equal(priorIntegrationPath, `${binaryPath}.install-integrations`);
      assert.match(priorIntegrationSha256, /^[0-9a-f]{64}$/u);
      assert.equal(
        integrationPath,
        `${binaryPath}.install-integrations.${integrationSha256}`,
      );
      assert.equal(
        sha256(readFileSync(integrationPath)),
        integrationSha256,
      );
      assert.deepEqual(readOrderedCtxCommands(fixture), [
        MANAGED_PAIR_RECONCILE_COMMAND,
      ]);
    } finally {
      fixture.cleanup();
    }
  });

  test("managed-pair rerun preserves man-page sidecar ownership", () => {
    const fixture = runRenderedCliInstaller({ managedPair: true, releaseVersion: "1.4.12", stagingDogfood: true });
    const binaryPath = path.join(fixture.installBin, "ctx");
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const markerBefore = JSON.parse(readFileSync(`${binaryPath}.install.json`, "utf8"));
      assert.equal(Object.hasOwn(markerBefore, "man_pages"), false);
      const ownedManBefore = readOwnershipRecords(fixture.installBin).filter(
        ({ kind }) => kind === "man",
      );
      assert.deepEqual(
        ownedManBefore.map(({ target }) => target),
        ["ctx-search.1", "ctx.1"].map((name) => path.join(fixture.manDir, name)),
      );

      const rerun = fixture.rerun();
      assert.equal(rerun.status, 0, rerun.stderr);
      const markerAfter = JSON.parse(readFileSync(`${binaryPath}.install.json`, "utf8"));
      assert.equal(Object.hasOwn(markerAfter, "man_pages"), false);
      assert.deepEqual(
        readOwnershipRecords(fixture.installBin).filter(({ kind }) => kind === "man"),
        ownedManBefore,
      );

      const disabled = fixture.rerun(["--no-man"]);
      assert.equal(disabled.status, 0, disabled.stderr);
      assert.equal(
        Object.hasOwn(
          JSON.parse(readFileSync(`${binaryPath}.install.json`, "utf8")),
          "man_pages",
        ),
        false,
      );
      assert.deepEqual(
        readOwnershipRecords(fixture.installBin).filter(({ kind }) => kind === "man"),
        ownedManBefore,
      );
    } finally {
      fixture.cleanup();
    }
  });

  test("managed-pair rerun leaves a legacy marker receipt-less", () => {
    const fixture = runRenderedCliInstaller({ managedPair: true, releaseVersion: "1.4.12", stagingDogfood: true });
    const binaryPath = path.join(fixture.installBin, "ctx");
    const markerPath = `${binaryPath}.install.json`;
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const marker = JSON.parse(readFileSync(markerPath, "utf8"));
      delete marker.man_pages;
      writeFileSync(markerPath, `${JSON.stringify(marker, null, 2)}\n`);

      const rerun = fixture.rerun();
      assert.equal(rerun.status, 0, rerun.stderr);
      const markerAfter = JSON.parse(readFileSync(markerPath, "utf8"));
      assert.equal(Object.hasOwn(markerAfter, "man_pages"), false);
    } finally {
      fixture.cleanup();
    }
  });

  test("staging dogfood rerun rejects every non-exact installed identity", () => {
    const mutations = [
      ["missing staging marker", (marker) => { delete marker.staging_dogfood; }],
      ["false staging marker", (marker) => { marker.staging_dogfood = false; }],
      ["malformed staging marker", (marker) => { marker.staging_dogfood = "true"; }],
      ["different channel", (marker) => { marker.channel = "dogfood-other"; }],
      ["different version", (marker) => { marker.version = "1.4.11"; }],
      ["different binary digest", (marker, binaryPath) => {
        writeFileSync(binaryPath, Buffer.concat([
          readFileSync(binaryPath),
          Buffer.from("\n# different staging candidate\n"),
        ]));
        chmodSync(binaryPath, 0o700);
        marker.sha256 = createHash("sha256")
          .update(readFileSync(binaryPath))
          .digest("hex");
      }],
    ];

    for (const [label, mutate] of mutations) {
      const fixture = runRenderedCliInstaller({
        args: ["--no-setup", "--no-skill", "--no-man"],
        stagingDogfood: true,
      });
      const binaryPath = path.join(fixture.installBin, "ctx");
      const markerPath = `${binaryPath}.install.json`;
      try {
        assert.equal(fixture.result.status, 0, fixture.result.stderr);
        const marker = JSON.parse(readFileSync(markerPath, "utf8"));
        mutate(marker, binaryPath);
        writeFileSync(markerPath, `${JSON.stringify(marker, null, 2)}\n`);
        const binaryBefore = readFileSync(binaryPath);
        const markerBefore = readFileSync(markerPath);

        const rerun = fixture.rerun();
        assert.notEqual(rerun.status, 0, label);
        assert.match(rerun.stderr, /staging dogfood reinstall/u, label);
        assert.deepEqual(readFileSync(binaryPath), binaryBefore, label);
        assert.deepEqual(readFileSync(markerPath), markerBefore, label);
        assert.deepEqual(readCtxCommands(fixture.commandLogPath), [], label);
      } finally {
        fixture.cleanup();
      }
    }
  });

  test("managed installer rerun retains the exact image on handoff failure or malformed proof", () => {
    for (const env of [
      {
        CTX_FAKE_MANAGED_UPGRADE_STATUS: "73",
        CTX_FAKE_MANAGED_UPGRADE_ERROR: "managed-pair directory is not owner-safe: /fixture/bin\n"
          + "detail\n".repeat(1500) + "omitted-error-tail",
      },
      { CTX_FAKE_MANAGED_UPGRADE_RESULT: '{"schema_version":1,"ok":true}' },
    ]) {
      const fixture = runRenderedCliInstaller();
      const binaryPath = path.join(fixture.installBin, "ctx");
      const markerPath = `${binaryPath}.install.json`;
      try {
        assert.equal(fixture.result.status, 0, fixture.result.stderr);
        chmodSync(fixture.installBin, 0o775);
        const binaryBefore = readFileSync(binaryPath);
        const markerBefore = readFileSync(markerPath);

        const rerun = fixture.rerun([], env);
        assert.notEqual(rerun.status, 0);
        assert.match(
          rerun.stderr,
          /managed lifecycle handoff|typed lifecycle proof/,
        );
        if (env.CTX_FAKE_MANAGED_UPGRADE_STATUS) {
          assert.match(rerun.stderr, /ctx child output:[\s\S]*directory is not owner-safe/u);
          assert.match(rerun.stderr, /status 73.*resolve the reported error before retrying/u);
          assert.doesNotMatch(rerun.stderr, /omitted-error-tail|rerun this installer/u);
          assert.ok(rerun.stderr.length < 9000);
        }
        assert.equal(statSync(fixture.installBin).mode & 0o777, 0o775);
        assert.deepEqual(readFileSync(binaryPath), binaryBefore);
        assert.deepEqual(readFileSync(markerPath), markerBefore);
        assert.deepEqual(readCtxCommands(fixture.commandLogPath).slice(-1), [
          "upgrade --channel stable --format=json",
        ]);
      } finally {
        fixture.cleanup();
      }
    }
  });

  test("rendered managed rerun orders trusted Core handoff before any executable publication", () => {
    const rendered = renderCliInstallScript({
      installAttemptId: "ia_managed_rerun_order",
    });
    const priorOwnership = rendered.lastIndexOf("load_previous_integration_ownership");
    const handoff = rendered.lastIndexOf("run_managed_core_upgrade");
    const directPublication = rendered.lastIndexOf("publish_fresh_or_legacy_binary");
    assert.ok(
      priorOwnership >= 0 &&
        priorOwnership < handoff &&
        handoff < directPublication,
      "managed ownership validation and Core handoff must precede the disjoint fresh path",
    );
    assert.match(
      rendered,
      /if \[ "\$managed_reinstall" = "1" \][\s\S]*run_managed_core_upgrade[\s\S]*else[\s\S]*preserve_core_man_pages=0[\s\S]*stage_integration_ownership[\s\S]*stage_install_marker[\s\S]*publish_fresh_or_legacy_binary/,
    );
    const upgradeInvocation = rendered.indexOf(
      '"$install_path" upgrade --channel "$channel" --format=json',
    );
    const upgradeProof = rendered.indexOf(
      'validate_managed_upgrade_result "$managed_upgrade_output"',
      upgradeInvocation,
    );
    const targetProof = rendered.indexOf(
      "verify_installed_target_identity",
      upgradeProof,
    );
    assert.ok(
      upgradeInvocation >= 0 &&
        upgradeInvocation < upgradeProof &&
        upgradeProof < targetProof,
      "managed reruns must verify the Core receipt and final signed target",
    );
    const currentReceiptValidator = rendered.match(
      /validate_managed_upgrade_result\(\) \{[\s\S]*?^\}/m,
    )?.[0] ?? "";
    assert.match(currentReceiptValidator, /required_count = 19/);
    assert.doesNotMatch(currentReceiptValidator, /required\["path"\]/);
    assert.doesNotMatch(
      rendered.match(/run_managed_core_upgrade\(\) \{[\s\S]*?^\}/m)?.[0] ?? "",
      /(?:install -m 0755|mv -f .*"\$install_path"|cat >"\$install_path")/,
    );
  });

  test("managed installer upgrade preserves shared executable directory permissions", () => {
    const fixture = runRenderedCliInstaller();
    try {
      assert.equal(fixture.result.status, 0, fixture.result.stderr);
      const unrelated = path.join(fixture.installBin, "unrelated-tool");
      writeFileSync(unrelated, "keep");
      for (const mode of [0o755, 0o775]) {
        chmodSync(fixture.installBin, mode);
        const upgrade = fixture.rerun();
        assert.equal(upgrade.status, 0, upgrade.stderr);
        assert.equal(statSync(fixture.installBin).mode & 0o777, mode);
        assert.equal(readFileSync(unrelated, "utf8"), "keep");
      }
    } finally {
      fixture.cleanup();
    }
  });

}
