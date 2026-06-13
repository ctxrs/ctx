import { describe, expect, it } from "vitest";
import type { ExecutionLaunchSnapshot } from "../../api/client";
import {
  currentLaunchStepLabel,
  formatLaunchRemaining,
  formatLaunchTime,
  launchElapsedMs,
  launchEtaRemainingMs,
  mergeLaunchLogs,
  stabilizeLaunchEtaRemainingMs,
  workspaceSetupProvisioningRemainingMs,
} from "./launchProgress";

const baseSnapshot = (): ExecutionLaunchSnapshot => ({
  job_id: "job_123",
  workspace_id: "ws_123",
  kind: "workspace_launch",
  state: "running",
  created_at: "2026-03-10T00:00:00.000Z",
  started_at: "2026-03-10T00:00:00.000Z",
  updated_at: "2026-03-10T00:00:04.000Z",
  current_phase: "artifact_download",
  current_step_label: "downloading required artifacts",
  progress_pct: 12,
  eta_ms: 245000,
  active_download: {
    artifact: "Required artifacts",
    downloaded_bytes: 412 * 1024 * 1024,
    total_bytes: 951 * 1024 * 1024,
    bytes_per_sec: 21 * 1024 * 1024,
  },
  phases: [],
  logs: [],
  error: null,
});

describe("launchProgress", () => {
  it("preformats and appends monotonic launch-log batches", () => {
    const merged = mergeLaunchLogs([], [
      {
        seq: 10,
        ts: "2026-03-09T19:10:24Z",
        phase: "machine_check",
        level: "info",
        message: "checking container runtime",
      },
      {
        seq: 11,
        ts: "2026-03-09T19:10:25Z",
        phase: "machine_start_or_init",
        level: "info",
        message: "starting machine",
      },
    ]);

    expect(merged.map((line) => line.seq)).toEqual([10, 11]);
    expect(merged[0].phaseLabel).toBe("Machine check");
    expect(merged[0].timeLabel).toBe(formatLaunchTime("2026-03-09T19:10:24Z"));
    expect(merged[1].phaseLabel).toBe("Machine start/init");
    expect(merged[1].timeLabel).toBe(formatLaunchTime("2026-03-09T19:10:25Z"));
  });

  it("dedupes and resorts overlapping snapshot batches", () => {
    const current = mergeLaunchLogs([], [
      {
        seq: 2,
        ts: "2026-03-09T19:10:25Z",
        phase: "machine_start_or_init",
        level: "info",
        message: "starting machine",
      },
      {
        seq: 3,
        ts: "2026-03-09T19:10:26Z",
        phase: "machine_start_or_init",
        level: "info",
        message: "machine ready",
      },
    ]);

    const merged = mergeLaunchLogs(current, [
      {
        seq: 1,
        ts: "2026-03-09T19:10:24Z",
        phase: "machine_check",
        level: "info",
        message: "checking container runtime",
      },
      {
        seq: 3,
        ts: "2026-03-09T19:10:26Z",
        phase: "machine_start_or_init",
        level: "warn",
        message: "machine ready (retry)",
      },
    ]);

    expect(merged.map((line) => [line.seq, line.level, line.message])).toEqual([
      [1, "info", "checking container runtime"],
      [2, "info", "starting machine"],
      [3, "warn", "machine ready (retry)"],
    ]);
  });

  it("trims launch logs to the most recent 400 lines", () => {
    const incoming = Array.from({ length: 405 }, (_, index) => ({
      seq: index + 1,
      ts: "2026-03-09T19:10:24Z",
      phase: "machine_check" as const,
      level: "info" as const,
      message: `line ${index + 1}`,
    }));

    const merged = mergeLaunchLogs([], incoming);

    expect(merged).toHaveLength(400);
    expect(merged[0]?.seq).toBe(6);
    expect(merged.at(-1)?.seq).toBe(405);
  });

  it("prefers the current step label over the coarse phase label", () => {
    expect(currentLaunchStepLabel(baseSnapshot())).toBe("Downloading required artifacts");
  });

  it("uses launch started_at for elapsed time so the timer does not reset per phase", () => {
    const snapshot = {
      ...baseSnapshot(),
      started_at: "2026-03-10T00:00:00.000Z",
      current_phase: "machine_start_or_init" as const,
      phases: [
        {
          phase: "artifact_download" as const,
          status: "completed" as const,
          started_at: "2026-03-10T00:00:00.000Z",
          completed_at: "2026-03-10T00:00:20.000Z",
          elapsed_ms: 20000,
        },
        {
          phase: "machine_start_or_init" as const,
          status: "running" as const,
          started_at: "2026-03-10T00:00:20.000Z",
          completed_at: null,
          elapsed_ms: 5000,
        },
      ],
    };

    expect(launchElapsedMs(snapshot, Date.parse("2026-03-10T00:00:25.000Z"))).toBe(25000);
  });

  it("returns null when launch-level start timestamps are unavailable", () => {
    const snapshot = {
      ...baseSnapshot(),
      started_at: "",
      created_at: "",
      current_phase: "machine_start_or_init" as const,
      phases: [
        {
          phase: "machine_start_or_init" as const,
          status: "running" as const,
          started_at: "2026-03-10T00:00:20.000Z",
          completed_at: null,
          elapsed_ms: 5000,
        },
      ],
    };

    expect(launchElapsedMs(snapshot, Date.parse("2026-03-10T00:00:25.000Z"))).toBeNull();
  });

  it("uses aggregate bucket budgets for artifact preparation without live download telemetry", () => {
    const remainingMs = launchEtaRemainingMs(
      {
        ...baseSnapshot(),
        active_download: null,
        phases: [
          {
            phase: "artifact_download" as const,
            started_at: "2026-03-10T00:00:00.000Z",
            finished_at: null,
            elapsed_ms: null,
          },
        ],
      },
      Date.parse("2026-03-10T00:00:09.000Z"),
    );
    expect(remainingMs).toBe(62_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("1m 2s est. remaining");
  });

  it("keeps artifact bucket elapsed time across artifact_download to machine_check", () => {
    const snapshot = {
      ...baseSnapshot(),
      current_phase: "machine_check" as const,
      current_step_label: "checking AVF Linux workspace VM state",
      active_download: null,
      phases: [
        {
          phase: "artifact_download" as const,
          started_at: "2026-03-10T00:00:00.000Z",
          finished_at: "2026-03-10T00:00:20.000Z",
          elapsed_ms: 20000,
        },
        {
          phase: "machine_check" as const,
          started_at: "2026-03-10T00:00:20.000Z",
          finished_at: null,
          elapsed_ms: null,
        },
      ],
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:00:23.000Z"),
    );
    expect(remainingMs).toBe(48_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("48s est. remaining");
  });

  it("uses live download telemetry plus downstream buckets while a download is active", () => {
    const snapshot = {
      ...baseSnapshot(),
      updated_at: "2026-03-10T00:00:04.000Z",
      active_download: {
        artifact: "Ubuntu guest runtime",
        downloaded_bytes: 40,
        total_bytes: 100,
        bytes_per_sec: 10,
      },
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:00:06.000Z"),
    );
    expect(remainingMs).toBe(35_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("35s est. remaining");
  });

  it("does not skip artifact preparation just because the current phase reports shared VM startup", () => {
    const snapshot = {
      ...baseSnapshot(),
      current_phase: "machine_start_or_init" as const,
      current_step_label: "starting AVF Linux workspace VM",
      active_download: null,
      phases: [],
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:00:25.000Z"),
    );
    expect(remainingMs).toBe(71_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("1m 11s est. remaining");
  });

  it("keeps charging the artifact bucket until artifact preparation is actually complete", () => {
    const snapshot = {
      ...baseSnapshot(),
      current_phase: "machine_start_or_init" as const,
      current_step_label: "starting AVF Linux workspace VM",
      active_download: null,
      phases: [
        {
          phase: "artifact_download" as const,
          started_at: "2026-03-10T00:00:00.000Z",
          finished_at: null,
          elapsed_ms: null,
        },
        {
          phase: "machine_start_or_init" as const,
          started_at: "2026-03-10T00:00:20.000Z",
          finished_at: null,
          elapsed_ms: null,
        },
      ],
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:00:25.000Z"),
    );
    expect(remainingMs).toBe(46_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("46s est. remaining");
  });

  it("switches to shared VM startup only after artifact preparation completes", () => {
    const snapshot = {
      ...baseSnapshot(),
      current_phase: "machine_start_or_init" as const,
      current_step_label: "starting AVF Linux workspace VM",
      active_download: null,
      phases: [
        {
          phase: "artifact_download" as const,
          started_at: "2026-03-10T00:00:00.000Z",
          finished_at: "2026-03-10T00:00:20.000Z",
          elapsed_ms: 20_000,
        },
        {
          phase: "machine_start_or_init" as const,
          started_at: "2026-03-10T00:00:20.000Z",
          finished_at: null,
          elapsed_ms: null,
        },
      ],
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:00:25.000Z"),
    );
    expect(remainingMs).toBe(26_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("26s est. remaining");
  });

  it("treats a completed legacy phase without a finish timestamp as complete", () => {
    const snapshot = {
      ...baseSnapshot(),
      current_phase: "machine_start_or_init" as const,
      current_step_label: "starting AVF Linux workspace VM",
      active_download: null,
      phases: [
        {
          phase: "artifact_download" as const,
          status: "completed" as const,
          started_at: "2026-03-10T00:00:00.000Z",
          completed_at: null,
          elapsed_ms: 20_000,
        },
        {
          phase: "machine_start_or_init" as const,
          status: "running" as const,
          started_at: "2026-03-10T00:00:20.000Z",
          completed_at: null,
          elapsed_ms: null,
        },
      ],
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:00:25.000Z"),
    );
    expect(remainingMs).toBe(26_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("26s est. remaining");
  });

  it("treats a completed phase with an invalid legacy finish timestamp as complete", () => {
    const snapshot = {
      ...baseSnapshot(),
      current_phase: "machine_start_or_init" as const,
      current_step_label: "starting AVF Linux workspace VM",
      active_download: null,
      phases: [
        {
          phase: "artifact_download" as const,
          status: "completed" as const,
          started_at: "2026-03-10T00:00:00.000Z",
          completed_at: "not-a-timestamp",
          finished_at: null,
          elapsed_ms: 20_000,
        },
        {
          phase: "machine_start_or_init" as const,
          status: "running" as const,
          started_at: "2026-03-10T00:00:20.000Z",
          completed_at: null,
          finished_at: null,
          elapsed_ms: null,
        },
      ],
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:00:25.000Z"),
    );
    expect(remainingMs).toBe(26_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("26s est. remaining");
  });

  it("keeps a bucket incomplete when the legacy phase status is still running", () => {
    const snapshot = {
      ...baseSnapshot(),
      current_phase: "machine_start_or_init" as const,
      current_step_label: "starting AVF Linux workspace VM",
      active_download: null,
      phases: [
        {
          phase: "artifact_download" as const,
          status: "running" as const,
          started_at: "2026-03-10T00:00:00.000Z",
          completed_at: null,
          elapsed_ms: null,
        },
        {
          phase: "machine_start_or_init" as const,
          status: "running" as const,
          started_at: "2026-03-10T00:00:20.000Z",
          completed_at: null,
          elapsed_ms: null,
        },
      ],
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:00:25.000Z"),
    );
    expect(remainingMs).toBe(46_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("46s est. remaining");
  });

  it("uses total launch budget when a running snapshot has no current phase", () => {
    const snapshot = {
      ...baseSnapshot(),
      current_phase: null,
      current_step_label: "",
      active_download: null,
      phases: [],
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:00:05.000Z"),
    );
    expect(remainingMs).toBe(66_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("1m 6s est. remaining");
  });

  it("uses total launch budget when a running snapshot has an unmapped phase", () => {
    const snapshot = {
      ...baseSnapshot(),
      current_phase: "ready" as const,
      state: "running" as const,
      current_step_label: "performing future backend phase",
      active_download: null,
      phases: [],
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:00:10.000Z"),
    );
    expect(remainingMs).toBe(61_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("1m 1s est. remaining");
  });

  it("still uses live download telemetry when phase is absent but download progress is active", () => {
    const snapshot = {
      ...baseSnapshot(),
      current_phase: null,
      current_step_label: "",
      active_download: {
        artifact: "Ubuntu guest runtime",
        downloaded_bytes: 40,
        total_bytes: 100,
        bytes_per_sec: 10,
      },
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:00:06.000Z"),
    );
    expect(remainingMs).toBe(35_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("35s est. remaining");
  });

  it("uses aggregate remaining for sandbox setup across grouped phases", () => {
    const snapshot = {
      ...baseSnapshot(),
      current_phase: "image_load" as const,
      current_step_label: "loading harness image into local sandbox runtime",
      active_download: null,
      phases: [
        {
          phase: "image_check" as const,
          started_at: "2026-03-10T00:00:50.000Z",
          finished_at: "2026-03-10T00:00:52.000Z",
          elapsed_ms: 2000,
        },
        {
          phase: "image_load" as const,
          started_at: "2026-03-10T00:00:52.000Z",
          finished_at: null,
          elapsed_ms: null,
        },
      ],
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:00:59.000Z"),
    );
    expect(remainingMs).toBe(3_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("3s est. remaining");
  });

  it("shows finishing up once the aggregate budget is exhausted but launch is still running", () => {
    const snapshot = {
      ...baseSnapshot(),
      current_phase: "runtime_network_setup" as const,
      current_step_label: "finalizing local sandbox runtime network",
      active_download: null,
      phases: [
        {
          phase: "image_check" as const,
          started_at: "2026-03-10T00:00:50.000Z",
          finished_at: "2026-03-10T00:00:52.000Z",
          elapsed_ms: 2000,
        },
        {
          phase: "image_load" as const,
          started_at: "2026-03-10T00:00:52.000Z",
          finished_at: "2026-03-10T00:00:58.000Z",
          elapsed_ms: 6000,
        },
        {
          phase: "runtime_network_setup" as const,
          started_at: "2026-03-10T00:00:58.000Z",
          finished_at: null,
          elapsed_ms: null,
        },
      ],
    };
    const remainingMs = launchEtaRemainingMs(
      snapshot,
      Date.parse("2026-03-10T00:01:05.000Z"),
    );
    expect(remainingMs).toBe(0);
    expect(formatLaunchRemaining(remainingMs)).toBe("Finishing up…");
  });

  it("models clone setup as one cumulative ETA through sandbox launch and bootstrap", () => {
    const remainingMs = workspaceSetupProvisioningRemainingMs({
      phase: "clone_repo",
      source: "clone",
      executionMode: "sandbox",
      phaseStartedAtMs: Date.parse("2026-03-10T00:00:10.000Z"),
      nowMs: Date.parse("2026-03-10T00:00:25.000Z"),
    });
    expect(remainingMs).toBe(114_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("1m 54s est. remaining");
  });

  it("models host setup remaining without sandbox launch phases", () => {
    const remainingMs = workspaceSetupProvisioningRemainingMs({
      phase: "configure_workspace",
      source: "new",
      executionMode: "host",
      phaseStartedAtMs: Date.parse("2026-03-10T00:00:20.000Z"),
      nowMs: Date.parse("2026-03-10T00:00:22.000Z"),
    });
    expect(remainingMs).toBe(8_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("8s est. remaining");
  });

  it("includes repo initialization time for imported non-repo folders", () => {
    const remainingMs = workspaceSetupProvisioningRemainingMs({
      phase: "init_repo",
      source: "import",
      executionMode: "sandbox",
      phaseStartedAtMs: Date.parse("2026-03-10T00:00:20.000Z"),
      nowMs: Date.parse("2026-03-10T00:00:22.000Z"),
    });
    expect(remainingMs).toBe(88_000);
    expect(formatLaunchRemaining(remainingMs)).toBe("1m 28s est. remaining");
  });

  it("recalibrates upward in bounded steps when a slow phase runs over budget", () => {
    const firstTick = stabilizeLaunchEtaRemainingMs({
      previousRemainingMs: 18_000,
      previousRawRemainingMs: 18_000,
      previousRecalibrationTargetMs: null,
      previousNowMs: Date.parse("2026-03-10T00:00:10.000Z"),
      nowMs: Date.parse("2026-03-10T00:00:11.000Z"),
      rawRemainingMs: 31_000,
    });
    const secondTick = stabilizeLaunchEtaRemainingMs({
      previousRemainingMs: firstTick.remainingMs,
      previousRawRemainingMs: 31_000,
      previousRecalibrationTargetMs: firstTick.recalibrationTargetMs,
      previousNowMs: Date.parse("2026-03-10T00:00:11.000Z"),
      nowMs: Date.parse("2026-03-10T00:00:12.000Z"),
      rawRemainingMs: 31_000,
    });

    expect(firstTick).toEqual({
      recalibrationTargetMs: 31_000,
      remainingMs: 22_600,
    });
    expect(secondTick).toEqual({
      recalibrationTargetMs: 31_000,
      remainingMs: 25_360,
    });
  });

  it("keeps decaying when the raw ETA is stuck at the same value", () => {
    const firstTick = stabilizeLaunchEtaRemainingMs({
      previousRemainingMs: 31_000,
      previousRawRemainingMs: 31_000,
      previousRecalibrationTargetMs: null,
      previousNowMs: Date.parse("2026-03-10T00:00:10.000Z"),
      nowMs: Date.parse("2026-03-10T00:00:11.000Z"),
      rawRemainingMs: 31_000,
    });
    const secondTick = stabilizeLaunchEtaRemainingMs({
      previousRemainingMs: firstTick.remainingMs,
      previousRawRemainingMs: 31_000,
      previousRecalibrationTargetMs: firstTick.recalibrationTargetMs,
      previousNowMs: Date.parse("2026-03-10T00:00:11.000Z"),
      nowMs: Date.parse("2026-03-10T00:00:12.000Z"),
      rawRemainingMs: 31_000,
    });
    expect(firstTick).toEqual({
      recalibrationTargetMs: null,
      remainingMs: 30_000,
    });
    expect(secondTick).toEqual({
      recalibrationTargetMs: null,
      remainingMs: 29_000,
    });
  });
});
