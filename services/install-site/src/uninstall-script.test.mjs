import test from "node:test";
import assert from "node:assert/strict";
import {
  existsSync,
  linkSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import { renderUninstallScript } from "./uninstall-script.js";
import {
  INSTALL_STAGE_EVENT_NAME,
  INSTALL_STAGE_EVENT_VERSION,
  INSTALL_STAGE_PAYLOAD_KEYS,
} from "./install-stage-contract.js";
import {
  VERIFIED_DAEMON_UNINSTALL_RESULT,
  daemonUninstallResult,
} from "./test/daemon-uninstall-result-fixture.mjs";
import {
  makeTempDir,
  runUninstaller,
  sha256,
  writeExecutable,
} from "./test/uninstall-script-harness.mjs";

const readStageReports = (filePath) => {
  if (!existsSync(filePath)) return [];
  return readFileSync(filePath, "utf8")
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
};

const readLifecycle = (filePath) => {
  if (!existsSync(filePath)) return [];
  return readFileSync(filePath, "utf8").trim().split("\n").filter(Boolean);
};

test("renderUninstallScript delegates Pro lifecycle to the native CLI", () => {
  const script = renderUninstallScript();
  assert.match(script, /^#!\/bin\/sh/m);
  assert.match(script, /prepare_core_daemon_uninstall/);
  assert.match(
    script,
    /--data-root "\$data_dir" daemon disable\s+--prepare-uninstall --format=json/,
  );
  for (const field of Object.keys(VERIFIED_DAEMON_UNINSTALL_RESULT)) {
    assert.match(script, new RegExp(field));
  }
  for (const [field, value] of Object.entries(VERIFIED_DAEMON_UNINSTALL_RESULT)) {
    if ([
      "requested_data_root",
      "canonical_data_root",
      "quiesced_roots",
      "quiesced_root_count",
    ].includes(field)) {
      continue;
    }
    assert.match(script, new RegExp(`expected\\["${field}"\\] = `));
    assert.match(script, new RegExp(String(value)));
  }
  assert.match(script, /uninstall_pro_with_native_cli/);
  assert.match(script, /pro uninstall --delete-data/);
  assert.match(script, /pro uninstall --keep-data/);
  assert.match(script, /noninteractive uninstall requires --delete-data or --keep-data/);
  assert.match(script, /canonical_analytics_disabled/);
  assert.match(script, /CTX_ANALYTICS_ENABLED/);
  for (const alias of ["CTX_ANALYTICS_OFF", "CTX_DISABLE_ANALYTICS", "CTX_INSTALL_DIAGNOSTICS_OFF"]) {
    assert.match(script, new RegExp(alias));
  }
  assert.match(script, /report_install_stage "uninstall" "started"/);
  assert.match(script, /report_install_stage "uninstall" "completed"/);
  assert.match(script, /report_install_stage "uninstall" "failed"/);
  assert.match(script, /install_stage_delivery_enabled=1/);
  assert.match(script, /--connect-timeout 1 --max-time 1/);
  assert.match(script, /install_stage_delivery_enabled=0/);
  assert.match(script, /install-attempt/);
  assert.match(script, /installed_cli_has_pro_lifecycle/);
  assert.match(script, /version_minor.*-le 25/);
  assert.match(script, /CTX_DATA_ROOT/);
  assert.doesNotMatch(script, /CTX_DATA_DIR/);
  assert.doesNotMatch(script, /rm -rf/);
  assert.doesNotMatch(script, /remove_file "\$data_dir"/);
  assert.doesNotMatch(script, /(?:^|\n)[ \t]*(?:kill|killall|pkill)[ \t]/);
  assert.doesNotMatch(script, /Proceed\?/);
  assert.ok(
    script.indexOf("prepare_core_daemon_uninstall") <
      script.lastIndexOf("uninstall_pro_with_native_cli"),
  );
});

test("shell uninstall directs Windows users to the managed PowerShell lifecycle", () => {
  const result = runUninstaller({
    os: "MINGW64_NT-10.0",
    args: ["--keep-data"],
  });

  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Windows requires the hosted PowerShell uninstaller/);
    assert.match(result.stderr, /https:\/\/ctx\.rs\/uninstall\.ps1/);
    assert.equal(existsSync(result.installPath), true);
    assert.equal(existsSync(result.markerPath), true);
  } finally {
    result.cleanup();
  }
});

for (const choice of [[], ["--delete-data"], ["--keep-data"]]) {
  const label = choice[0] ?? "interactive-choice-not-needed";
  test(`hosted uninstall bypasses unavailable Pro lifecycle for released 0.25 (${label})`, () => {
    const result = runUninstaller({
      os: "Linux",
      args: choice,
      nativeVersion: "0.25.0",
      supportsProLifecycle: false,
    });

    try {
      assert.equal(result.status, 0, result.stderr);
      assert.equal(existsSync(result.installPath), false);
      assert.equal(existsSync(result.markerPath), false);
      assert.equal(existsSync(result.manPath), false);
      assert.equal(existsSync(result.nativeLog), false);
      assert.equal(
        readFileSync(path.join(result.dataDir, "work.sqlite"), "utf8"),
        "canonical-history\n",
      );
      assert.match(result.stderr, /ctx 0\.25\.0 predates Local Pro/);
    } finally {
      result.cleanup();
    }
  });
}

test("hosted uninstall fails closed when a 0.26+ CLI lacks Pro lifecycle capability", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    nativeVersion: "0.26.0",
    supportsProLifecycle: false,
  });

  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /does not expose the required Pro uninstall capability/);
    assert.equal(existsSync(result.installPath), true);
    assert.equal(existsSync(result.markerPath), true);
    assert.equal(existsSync(result.manPath), true);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall fails closed on a malformed installed CLI version", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    nativeVersion: "0.25",
    supportsProLifecycle: false,
  });

  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /returned an invalid version/);
    assert.equal(existsSync(result.installPath), true);
    assert.equal(existsSync(result.markerPath), true);
    assert.equal(existsSync(result.manPath), true);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall refuses noninteractive execution without a data choice", () => {
  const result = runUninstaller({ os: "Linux" });

  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /noninteractive uninstall requires --delete-data or --keep-data/);
    assert.equal(existsSync(result.installPath), true);
    assert.equal(existsSync(result.dataDir), true);
    assert.deepEqual(readStageReports(result.installStageLog).map(({ stage, status }) => [stage, status]), [
      ["uninstall", "started"],
      ["uninstall", "failed"],
    ]);
  } finally {
    result.cleanup();
  }
});

for (const choice of ["--delete-data", "--keep-data"]) {
  test(`hosted uninstall delegates ${choice} and preserves canonical history`, () => {
    const result = runUninstaller({ os: "Linux", args: [choice] });

    try {
      assert.equal(result.status, 0, result.stderr);
      assert.equal(existsSync(result.installPath), false);
      assert.equal(existsSync(result.markerPath), false);
      assert.equal(existsSync(result.manPath), false);
      assert.equal(existsSync(result.dataDir), true);
      assert.equal(
        readFileSync(path.join(result.dataDir, "work.sqlite"), "utf8"),
        "canonical-history\n",
      );
      assert.deepEqual(readFileSync(result.nativeLog, "utf8").trim().split("\n"), [
        "--data-root",
        result.dataDir,
        "pro",
        "uninstall",
        choice,
      ]);
      assert.match(result.stderr, /ctx uninstall complete\. Local ctx history was preserved\./);
      const reports = readStageReports(result.installStageLog);
      assert.deepEqual(reports.map(({ stage, status }) => [stage, status]), [
        ["uninstall", "started"],
        ["uninstall", "completed"],
      ]);
      for (const report of reports) {
        assert.deepEqual(Object.keys(report).sort(), INSTALL_STAGE_PAYLOAD_KEYS);
        assert.equal(report.event_name, INSTALL_STAGE_EVENT_NAME);
        assert.equal(report.event_version, INSTALL_STAGE_EVENT_VERSION);
        assert.equal(report.install_attempt_id, "ia_uninstall_test");
        assert.equal(report.platform, "linux");
        assert.equal(report.arch, "x64");
        assert.equal(report.script_family, "posix");
      }
      assert.doesNotMatch(JSON.stringify(reports), /delete-data|keep-data|work\.sqlite|data_dir|path|command|error|user/i);
    } finally {
      result.cleanup();
    }
  });
}

for (const testCase of [
  {
    name: "legacy schema",
    env: { CTX_TEST_HOSTED_UNINSTALL_RECEIPT_SCHEMA: "1" },
  },
  {
    name: "false daemon-admission fence",
    env: { CTX_TEST_HOSTED_UNINSTALL_DAEMON_FENCED: "false" },
  },
]) {
  test(`hosted uninstall rejects ${testCase.name} transaction proof before lifecycle mutation`, () => {
    const result = runUninstaller({
      os: "Linux",
      args: ["--delete-data"],
      env: testCase.env,
    });

    try {
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /invalid hosted uninstall transaction proof/);
      assert.equal(existsSync(result.installPath), true);
      assert.equal(existsSync(result.markerPath), true);
      assert.equal(existsSync(result.helperPath), true);
      assert.equal(existsSync(result.transactionPath), true);
      assert.equal(existsSync(result.lifecycleLog), false);
    } finally {
      result.cleanup();
    }
  });
}

test("hosted uninstall tears down a live Core daemon before Pro cleanup and binary deletion", () => {
  const result = runUninstaller({ os: "Linux", args: ["--keep-data"] });

  try {
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(readLifecycle(result.lifecycleLog), ["daemon", "pro"]);
    assert.equal(existsSync(result.installPath), false);
    assert.equal(existsSync(result.integrationsPath), false);
    for (const statePath of Object.values(result.daemonStatePaths)) {
      assert.equal(existsSync(statePath), false, statePath);
    }
  } finally {
    result.cleanup();
  }
});

test("custom-root uninstall requires installation-wide proof including the canonical root", () => {
  const sandbox = makeTempDir("ctx-uninstall-custom-root-parent-");
  const customRoot = path.join(sandbox, "custom-root");
  const canonicalRoot = path.join(sandbox, "canonical-root");
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    canonicalDataDir: canonicalRoot,
    paths: { dataDir: customRoot },
  });

  try {
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(readLifecycle(result.lifecycleLog), ["daemon", "pro"]);
    assert.equal(existsSync(result.installPath), false);
    assert.match(
      readFileSync(result.nativeLog, "utf8"),
      new RegExp(customRoot.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")),
    );
  } finally {
    result.cleanup();
    rmSync(sandbox, { recursive: true, force: true });
  }
});

test("hosted uninstall retains the binary when Core daemon teardown is interrupted", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    daemonStatus: 89,
  });

  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Core daemon teardown failed with status 89/);
    assert.deepEqual(readLifecycle(result.lifecycleLog), ["daemon"]);
    assert.equal(existsSync(result.nativeLog), false);
    assert.equal(existsSync(result.installPath), true);
    assert.equal(existsSync(result.markerPath), true);
    for (const statePath of Object.values(result.daemonStatePaths)) {
      assert.equal(existsSync(statePath), true, statePath);
    }
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall retries Core teardown safely after an interrupted attempt", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--delete-data"],
    daemonStatus: 89,
  });

  try {
    assert.notEqual(result.status, 0);
    const retried = result.rerun(
      ["--delete-data"],
      {
        CTX_TEST_DAEMON_RESULT: daemonUninstallResult({}, {
          requestedDataRoot: result.dataDir,
          canonicalDataRoot: result.dataDir,
        }),
        CTX_TEST_DAEMON_STATUS: "0",
      },
    );
    assert.equal(retried.status, 0, retried.stderr);
    assert.deepEqual(readLifecycle(result.lifecycleLog), ["daemon", "daemon", "pro"]);
    assert.equal(existsSync(result.installPath), false);
    assert.equal(existsSync(result.markerPath), false);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall retries every durable leaf-transaction phase", () => {
  const phases = [
    "journal_prepared",
    "helper_staged",
    "armed",
    "removing_binary",
    "binary_removed",
    "binary_removed_recorded",
    "removing_ownership",
    "ownership_removed",
    "ownership_removed_recorded",
    "removing_marker",
    "marker_removed",
    "committed",
  ];
  for (const phase of phases) {
    const result = runUninstaller({
      os: "Linux",
      args: ["--keep-data"],
      env: { CTX_TEST_HOSTED_UNINSTALL_FAULT: phase },
    });
    try {
      assert.notEqual(result.status, 0, phase);
      assert.equal(existsSync(result.transactionPath), true, phase);
      const retried = result.rerun(["--keep-data"], {
        CTX_TEST_HOSTED_UNINSTALL_FAULT: "",
      });
      assert.equal(retried.status, 0, `${phase}: ${retried.stderr}`);
      assert.equal(existsSync(result.installPath), false, phase);
      assert.equal(existsSync(result.markerPath), false, phase);
      assert.equal(existsSync(result.transactionPath), false, phase);
      assert.equal(existsSync(result.helperPath), false, phase);
    } finally {
      result.cleanup();
    }
  }
});

test("hosted uninstall rejects marker-only state without a recorded transaction", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    mutateAfterOwnership: ({ installPath }) => rmSync(installPath),
  });
  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /no recorded executable helper/);
    assert.equal(existsSync(result.markerPath), true);
    assert.equal(existsSync(result.transactionPath), false);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall accepts verified teardown when no Core daemon is running", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    daemonInitiallyRunning: false,
  });

  try {
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(readLifecycle(result.lifecycleLog), ["daemon", "pro"]);
    assert.equal(existsSync(result.installPath), false);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall rejects typed Core success while supervisor residue remains", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    daemonRemovesState: false,
    daemonResult: daemonUninstallResult({ supervisor_removed: false }),
  });

  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Core daemon teardown did not prove complete cleanup/);
    assert.deepEqual(readLifecycle(result.lifecycleLog), ["daemon"]);
    assert.equal(existsSync(result.nativeLog), false);
    assert.equal(existsSync(result.installPath), true);
    assert.equal(existsSync(result.daemonStatePaths.supervisor), true);
  } finally {
    result.cleanup();
  }
});

for (const [field, value] of [
  ["installation_quiescent", false],
  ["owner_lock_released", false],
  ["endpoint_released", false],
  ["binary_retained", false],
]) {
  test(`hosted uninstall retains the image when ${field} reports lifecycle residue`, () => {
    const result = runUninstaller({
      os: "Linux",
      args: ["--keep-data"],
      daemonResult: daemonUninstallResult({ [field]: value }),
    });

    try {
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /did not prove complete cleanup/);
      assert.deepEqual(readLifecycle(result.lifecycleLog), ["daemon"]);
      assert.equal(existsSync(result.installPath), true);
      assert.equal(existsSync(result.markerPath), true);
      assert.equal(existsSync(result.nativeLog), false);
    } finally {
      result.cleanup();
    }
  });
}

test("hosted uninstall rejects untyped Core daemon success", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    daemonResult: daemonUninstallResult({ ok: "true" }),
  });

  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Core daemon teardown did not prove complete cleanup/);
    assert.deepEqual(readLifecycle(result.lifecycleLog), ["daemon"]);
    assert.equal(existsSync(result.installPath), true);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall never removes installer files after native lifecycle failure", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--delete-data"],
    nativeStatus: 9,
  });

  try {
    assert.equal(result.status, 9);
    assert.equal(existsSync(result.installPath), true);
    assert.equal(existsSync(result.markerPath), true);
    assert.equal(existsSync(result.manPath), true);
    assert.equal(existsSync(result.dataDir), true);
    assert.deepEqual(readStageReports(result.installStageLog).map(({ stage, status }) => [stage, status]), [
      ["uninstall", "started"],
      ["uninstall", "failed"],
    ]);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall honors canonical and legacy analytics opt-outs", () => {
  for (const env of [
    { CTX_ANALYTICS_ENABLED: " false " },
    { CTX_ANALYTICS_ENABLED: "true", CTX_ANALYTICS_OFF: " yes " },
    { CTX_DISABLE_ANALYTICS: "ON" },
    { CTX_INSTALL_DIAGNOSTICS_OFF: "1" },
  ]) {
    const result = runUninstaller({ os: "Linux", args: ["--keep-data"], env });
    try {
      assert.equal(result.status, 0, result.stderr);
      assert.equal(existsSync(result.installStageLog), false);
      assert.equal(readFileSync(result.nativeEnvLog, "utf8").trim(), "false");
    } finally {
      result.cleanup();
    }
  }
});

test("hosted uninstall latches diagnostics off without changing success", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    env: { CTX_TEST_INSTALL_STAGE_STATUS: "66" },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(readStageReports(result.installStageLog).map(({ stage, status }) => [stage, status]), [
      ["uninstall", "started"],
    ]);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall never sends an invalid attempt override", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    env: { CTX_INSTALL_ATTEMPT_ID: "ia_/home/alice/private path" },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    const reports = readStageReports(result.installStageLog);
    assert.ok(reports.every((report) => report.install_attempt_id === "ia_uninstall_test"));
    assert.doesNotMatch(JSON.stringify(reports), /alice|private path/);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall requires exactly one explicit noninteractive choice", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--delete-data", "--keep-data"],
  });

  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /choose exactly one of --delete-data or --keep-data/);
    assert.equal(existsSync(result.installPath), true);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall supports explicit path overrides through native delegation", () => {
  const sandboxRoot = makeTempDir("ctx-uninstall-custom-");
  const paths = {
    installPath: path.join(sandboxRoot, "custom", "ctx"),
    manPath: path.join(sandboxRoot, "custom", "ctx.1"),
    dataDir: path.join(sandboxRoot, "custom-data"),
  };
  paths.markerPath = `${paths.installPath}.install.json`;
  const result = runUninstaller({ os: "Darwin", args: ["--keep-data"], paths });

  try {
    assert.equal(result.status, 0, result.stderr);
    assert.equal(existsSync(paths.installPath), false);
    assert.equal(existsSync(paths.markerPath), false);
    assert.equal(existsSync(paths.manPath), false);
    assert.equal(existsSync(paths.dataDir), true);
  } finally {
    result.cleanup();
    rmSync(sandboxRoot, { recursive: true, force: true });
  }
});

test("hosted uninstall honors CTX_DATA_ROOT and ignores CTX_DATA_DIR", () => {
  const ignoredRoot = makeTempDir("ctx-uninstall-ignored-data-dir-");
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    env: { CTX_DATA_DIR: ignoredRoot },
  });

  try {
    assert.equal(result.status, 0, result.stderr);
    const nativeArgs = readFileSync(result.nativeLog, "utf8").trim().split("\n");
    assert.deepEqual(nativeArgs.slice(0, 2), ["--data-root", result.dataDir]);
    assert.notEqual(result.dataDir, ignoredRoot);
  } finally {
    result.cleanup();
    rmSync(ignoredRoot, { recursive: true, force: true });
  }
});

for (const testCase of [
  {
    name: "unsupported marker schema",
    markerPatch: { schema_version: 2 },
    error: /unsupported schema/,
  },
  {
    name: "foreign marker manager",
    markerPatch: { manager: "homebrew" },
    error: /does not identify the ctx hosted installer/,
  },
  {
    name: "marker path substitution",
    markerPatch: { install_path: "/tmp/not-the-managed-ctx" },
    error: /does not own the requested executable/,
  },
  {
    name: "cross-platform marker",
    markerPatch: { platform: "macos-x64" },
    error: /platform does not match this host/,
  },
  {
    name: "invalid marker digest",
    markerPatch: { sha256: "not-a-sha256" },
    error: /invalid SHA-256 identity/,
  },
  {
    name: "replaced managed binary",
    mutateAfterOwnership: ({ installPath }) => {
      writeExecutable(installPath, "#!/bin/sh\nexit 99\n");
    },
    error: /differs from its managed install marker/,
  },
  {
    name: "missing package-manager marker",
    mutateAfterOwnership: ({ markerPath }) => {
      rmSync(markerPath);
    },
    error: /package-manager and unmanaged binaries/,
  },
  {
    name: "hard-linked managed marker",
    mutateAfterOwnership: ({ markerPath }) => {
      linkSync(markerPath, `${markerPath}.second-link`);
    },
    error: /marker must not be hard-linked/,
  },
]) {
  test(`hosted uninstall fails closed before execution for ${testCase.name}`, () => {
    const result = runUninstaller({
      os: "Linux",
      args: ["--keep-data"],
      markerPatch: testCase.markerPatch,
      mutateAfterOwnership: testCase.mutateAfterOwnership,
    });
    try {
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, testCase.error);
      assert.equal(existsSync(result.installPath), true);
      assert.equal(existsSync(result.manPath), true);
      assert.equal(existsSync(result.nativeLog), false);
    } finally {
      result.cleanup();
    }
  });
}

test("hosted uninstall rejects malicious integration paths before native execution", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    prepareOwnedArtifacts: ({ homeDir }) => [{
      kind: "man",
      digest: "a".repeat(64),
      target: path.join(homeDir, ".local", "share", "man", "man1", "..", "victim", "ctx-evil.1"),
    }],
  });
  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /invalid record|unsafe man-page path/);
    assert.equal(existsSync(result.installPath), true);
    assert.equal(existsSync(result.nativeLog), false);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall rejects malformed and marker-mismatched integration digests", () => {
  const malformed = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    prepareOwnedArtifacts: ({ homeDir }) => [{
      kind: "man",
      digest: "not-a-digest",
      target: path.join(homeDir, ".local", "share", "man", "man1", "ctx-evil.1"),
    }],
  });
  const replaced = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    markerPatch: { integrations_sha256: "0".repeat(64) },
  });
  try {
    assert.notEqual(malformed.status, 0);
    assert.match(malformed.stderr, /invalid record/);
    assert.equal(existsSync(malformed.nativeLog), false);
    assert.notEqual(replaced.status, 0);
    assert.match(replaced.stderr, /integration ownership differs from its marker/);
    assert.equal(existsSync(replaced.nativeLog), false);
  } finally {
    malformed.cleanup();
    replaced.cleanup();
  }
});

test("hosted uninstall safely accepts a marker-bound digest generation", () => {
  let generationPath;
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    mutateAfterOwnership: ({ integrationsPath, markerPath }) => {
      const body = readFileSync(integrationsPath);
      generationPath = `${integrationsPath}.${sha256(body)}`;
      renameSync(integrationsPath, generationPath);
      const marker = JSON.parse(readFileSync(markerPath, "utf8"));
      marker.integrations_path = generationPath;
      writeFileSync(markerPath, `${JSON.stringify(marker, null, 2)}\n`);
    },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.equal(
      existsSync(generationPath),
      false,
      "Core removes the exact active marker-bound generation",
    );
    assert.equal(existsSync(result.installPath), false);
    assert.equal(existsSync(result.markerPath), false);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall removes exact owned integrations and preserves modified or unowned files", () => {
  let profilePath;
  let skillPath;
  let extraSkillPath;
  const profileBlock = [
    "# >>> ctx installer PATH setup >>>",
    'export PATH="/owned/bin:${PATH}"',
    "# <<< ctx installer PATH setup <<<",
    "",
  ].join("\n");
  const skillBody = "# installed skill\n";
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    prepareOwnedArtifacts: ({ homeDir }) => {
      profilePath = path.join(homeDir, ".bashrc");
      writeFileSync(profilePath, `# user prefix\n${profileBlock}# user suffix\n`);
      skillPath = path.join(homeDir, ".agents", "skills", "ctx-agent-history-search");
      mkdirSync(skillPath, { recursive: true });
      writeFileSync(path.join(skillPath, "SKILL.md"), skillBody);
      const skillMarkerBody = `${JSON.stringify({
        schema_version: 1,
        installer: "ctx-cli",
        skill_name: "ctx-agent-history-search",
        skill_hash: `sha256:${sha256(skillBody)}`,
      }, null, 2)}\n`;
      writeFileSync(path.join(skillPath, ".ctx-skill.json"), skillMarkerBody);
      extraSkillPath = path.join(skillPath, "user-notes.txt");
      writeFileSync(extraSkillPath, "keep me\n");
      return [
        { kind: "profile-block", digest: sha256(profileBlock), target: profilePath },
        { kind: "skill", digest: sha256(skillBody + skillMarkerBody), target: skillPath },
      ];
    },
    mutateAfterOwnership: ({ manPath }) => {
      writeFileSync(manPath, ".TH user-modified-ctx 1\n");
    },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.equal(readFileSync(result.manPath, "utf8"), ".TH user-modified-ctx 1\n");
    assert.equal(existsSync(result.secondManPath), false);
    assert.equal(readFileSync(profilePath, "utf8"), "# user prefix\n# user suffix\n");
    assert.equal(existsSync(path.join(skillPath, "SKILL.md")), false);
    assert.equal(existsSync(path.join(skillPath, ".ctx-skill.json")), false);
    assert.equal(readFileSync(extraSkillPath, "utf8"), "keep me\n");
    assert.match(result.stderr, /Preserved modified ctx man page/);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall preserves modified profile blocks and skill copies", () => {
  let profilePath;
  let skillPath;
  const profileBlock = [
    "# >>> ctx installer PATH setup >>>",
    'export PATH="/owned/bin:${PATH}"',
    "# <<< ctx installer PATH setup <<<",
    "",
  ].join("\n");
  const skillBody = "# installed skill\n";
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    prepareOwnedArtifacts: ({ homeDir }) => {
      profilePath = path.join(homeDir, ".profile");
      writeFileSync(profilePath, profileBlock);
      skillPath = path.join(homeDir, ".agents", "skills", "ctx-agent-history-search");
      mkdirSync(skillPath, { recursive: true });
      writeFileSync(path.join(skillPath, "SKILL.md"), skillBody);
      const skillMarkerBody = `${JSON.stringify({
        schema_version: 1,
        installer: "ctx-cli",
        skill_name: "ctx-agent-history-search",
        skill_hash: `sha256:${sha256(skillBody)}`,
      }, null, 2)}\n`;
      writeFileSync(path.join(skillPath, ".ctx-skill.json"), skillMarkerBody);
      return [
        { kind: "profile-block", digest: sha256(profileBlock), target: profilePath },
        { kind: "skill", digest: sha256(skillBody + skillMarkerBody), target: skillPath },
      ];
    },
    mutateAfterOwnership: () => {
      writeFileSync(profilePath, profileBlock.replace("/owned/bin", "/user/bin"));
      writeFileSync(path.join(skillPath, "SKILL.md"), "# user-modified skill\n");
    },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.match(readFileSync(profilePath, "utf8"), /user\/bin/);
    assert.equal(existsSync(path.join(skillPath, ".ctx-skill.json")), true);
    assert.equal(existsSync(path.join(skillPath, "SKILL.md")), true);
    assert.match(result.stderr, /Preserved modified installer PATH profile block/);
    assert.match(result.stderr, /Preserved modified or unowned ctx skill/);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall is idempotent after a complete Core and Pro cleanup", () => {
  const result = runUninstaller({ os: "Linux", args: ["--delete-data"] });
  try {
    assert.equal(result.status, 0, result.stderr);
    const second = result.rerun(["--delete-data"]);
    assert.equal(second.status, 0, second.stderr);
    assert.match(second.stderr, /ctx is already uninstalled/);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall retains integration cleanup after a managed upgrade rewrites the marker", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    markerPatch: {
      integrations_path: undefined,
      integrations_sha256: undefined,
    },
  });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.equal(existsSync(result.manPath), false);
    assert.equal(existsSync(result.secondManPath), false);
    assert.equal(existsSync(result.integrationsPath), false);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall fails closed on a replaced post-upgrade integration ledger", () => {
  const result = runUninstaller({
    os: "Linux",
    args: ["--keep-data"],
    markerPatch: {
      integrations_path: undefined,
      integrations_sha256: undefined,
    },
    mutateAfterOwnership: ({ integrationsPath }) => {
      const body = readFileSync(integrationsPath, "utf8");
      writeFileSync(integrationsPath, body.replace("\nman\t", "\nskill\t"));
    },
  });
  try {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /records digest does not match/);
    assert.equal(existsSync(result.nativeLog), false);
    assert.equal(existsSync(result.installPath), true);
  } finally {
    result.cleanup();
  }
});

test("hosted uninstall supports canonical installer paths containing spaces and quotes", () => {
  const sandboxRoot = makeTempDir("ctx-uninstall-quoted-");
  const installPath = path.join(sandboxRoot, 'owned " install', "ctx");
  const paths = {
    installPath,
    markerPath: `${installPath}.install.json`,
    manPath: path.join(sandboxRoot, 'manual " pages', "ctx.1"),
    dataDir: path.join(sandboxRoot, "data root"),
  };
  const result = runUninstaller({ os: "Linux", args: ["--keep-data"], paths });
  try {
    assert.equal(result.status, 0, result.stderr);
    assert.equal(existsSync(paths.installPath), false);
    assert.equal(existsSync(paths.markerPath), false);
    assert.equal(existsSync(paths.manPath), false);
  } finally {
    result.cleanup();
    rmSync(sandboxRoot, { recursive: true, force: true });
  }
});
