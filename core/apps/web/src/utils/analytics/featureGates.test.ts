import { beforeEach, describe, expect, it, vi } from "vitest";

const {
  evaluateFeatureGateMock,
  subscribeFeatureFlagsMock,
  trackFeatureGateEvaluatedMock,
  trackExperimentExposureMock,
  hasTrackedExperimentExposureMock,
  markExperimentExposureTrackedMock,
} = vi.hoisted(() => ({
  evaluateFeatureGateMock: vi.fn(),
  subscribeFeatureFlagsMock: vi.fn(() => () => {}),
  trackFeatureGateEvaluatedMock: vi.fn(),
  trackExperimentExposureMock: vi.fn(),
  hasTrackedExperimentExposureMock: vi.fn(() => false),
  markExperimentExposureTrackedMock: vi.fn(),
}));

vi.mock("./client", () => ({
  evaluateFeatureGate: evaluateFeatureGateMock,
  subscribeFeatureFlags: subscribeFeatureFlagsMock,
}));

vi.mock("./activity", () => ({
  trackFeatureGateEvaluated: trackFeatureGateEvaluatedMock,
  trackExperimentExposure: trackExperimentExposureMock,
}));

vi.mock("./experimentExposureDedup", () => ({
  hasTrackedExperimentExposure: hasTrackedExperimentExposureMock,
  markExperimentExposureTracked: markExperimentExposureTrackedMock,
}));

import { getFeatureGate } from "./featureGates";

describe("feature gate analytics", () => {
  beforeEach(() => {
    evaluateFeatureGateMock.mockReset();
    subscribeFeatureFlagsMock.mockReset();
    trackFeatureGateEvaluatedMock.mockReset();
    trackFeatureGateEvaluatedMock.mockReturnValue(true);
    trackExperimentExposureMock.mockReset();
    trackExperimentExposureMock.mockReturnValue(true);
    hasTrackedExperimentExposureMock.mockReset();
    hasTrackedExperimentExposureMock.mockReturnValue(false);
    markExperimentExposureTrackedMock.mockReset();
    subscribeFeatureFlagsMock.mockImplementation(() => () => {});
  });

  it("tracks gate evaluation and exposure once per gate/variant", () => {
    evaluateFeatureGateMock.mockReturnValue({ value: true, reason: "posthog" });
    hasTrackedExperimentExposureMock.mockImplementationOnce(() => false).mockImplementationOnce(() => true);

    expect(getFeatureGate("queued_messages_enabled", false)).toBe(true);
    expect(getFeatureGate("queued_messages_enabled", false)).toBe(true);

    expect(trackFeatureGateEvaluatedMock).toHaveBeenCalledTimes(1);
    expect(trackFeatureGateEvaluatedMock).toHaveBeenCalledWith({
      gateKey: "queued_messages_enabled",
      result: true,
      reason: "posthog",
    });
    expect(trackExperimentExposureMock).toHaveBeenCalledTimes(1);
    expect(trackExperimentExposureMock).toHaveBeenCalledWith({
      experimentKey: "queued_messages_enabled",
      variant: "enabled",
      assignmentUnit: "install_id",
    });
    expect(markExperimentExposureTrackedMock).toHaveBeenCalledTimes(1);
  });

  it("does not emit experiment exposure for fallback evaluations", () => {
    evaluateFeatureGateMock.mockReturnValue({ value: false, reason: "fallback" });

    expect(getFeatureGate("future_flag", false)).toBe(false);

    expect(trackFeatureGateEvaluatedMock).toHaveBeenCalledTimes(1);
    expect(trackExperimentExposureMock).not.toHaveBeenCalled();
    expect(markExperimentExposureTrackedMock).not.toHaveBeenCalled();
  });

  it("retries exposure emission when previous attempt was not accepted", () => {
    evaluateFeatureGateMock.mockReturnValue({ value: true, reason: "posthog" });
    trackExperimentExposureMock.mockReturnValueOnce(false).mockReturnValueOnce(true);
    hasTrackedExperimentExposureMock.mockReturnValue(false);

    expect(getFeatureGate("queued_messages_enabled", false)).toBe(true);
    expect(getFeatureGate("queued_messages_enabled", false)).toBe(true);

    expect(trackExperimentExposureMock).toHaveBeenCalledTimes(2);
    expect(markExperimentExposureTrackedMock).toHaveBeenCalledTimes(1);
  });

  it("retries gate evaluation telemetry when previous attempt was not accepted", () => {
    evaluateFeatureGateMock.mockReturnValue({ value: true, reason: "posthog" });
    hasTrackedExperimentExposureMock.mockReturnValue(true);
    trackFeatureGateEvaluatedMock.mockReturnValueOnce(false).mockReturnValueOnce(true);

    expect(getFeatureGate("queued_messages_eval_retry", false)).toBe(true);
    expect(getFeatureGate("queued_messages_eval_retry", false)).toBe(true);

    expect(trackFeatureGateEvaluatedMock).toHaveBeenCalledTimes(2);
  });
});
