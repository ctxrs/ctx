import { useEffect, useState } from "react";
import { evaluateFeatureGate, subscribeFeatureFlags } from "./client";
import {
  hasTrackedExperimentExposure,
  markExperimentExposureTracked,
} from "./experimentExposureDedup";
import { trackExperimentExposure, trackFeatureGateEvaluated } from "./activity";

const trackedGateEvaluations = new Set<string>();

export const getFeatureGate = (gate: string, fallback = false): boolean => {
  const evaluated = evaluateFeatureGate(gate, fallback);
  const trackingKey = `${gate}:${evaluated.reason}:${evaluated.value ? "1" : "0"}`;
  if (!trackedGateEvaluations.has(trackingKey)) {
    const accepted = trackFeatureGateEvaluated({
      gateKey: gate,
      result: evaluated.value,
      reason: evaluated.reason,
    });
    if (accepted) {
      if (trackedGateEvaluations.size > 256) trackedGateEvaluations.clear();
      trackedGateEvaluations.add(trackingKey);
    }
  }
  if (evaluated.reason !== "fallback") {
    const variant = evaluated.value ? "enabled" : "disabled";
    if (!hasTrackedExperimentExposure(gate, variant)) {
      const accepted = trackExperimentExposure({
        experimentKey: gate,
        variant,
        assignmentUnit: "install_id",
      });
      if (accepted) {
        markExperimentExposureTracked(gate, variant);
      }
    }
  }
  return evaluated.value;
};

export const useFeatureGate = (gate: string, fallback = false): boolean => {
  const [value, setValue] = useState<boolean>(() => getFeatureGate(gate, fallback));

  useEffect(() => {
    setValue(getFeatureGate(gate, fallback));
    return subscribeFeatureFlags(() => {
      setValue(getFeatureGate(gate, fallback));
    });
  }, [gate, fallback]);

  return value;
};
