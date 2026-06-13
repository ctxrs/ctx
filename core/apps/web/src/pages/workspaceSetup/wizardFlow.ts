export type WizardStepKey =
  | "location"
  | "container"
  | "harness-downloads"
  | "auth-import"
  | "session-titling"
  | "source"
  | "network"
  | "setup"
  | "merge-queue"
  | "confirm";

export type WizardRoutePlan = {
  targetKey: string;
  containerSelection: string;
  includeHarnessDownloads: boolean;
  includeAuthImport: boolean;
  includeTitling: boolean;
};

export type WizardStepPathInput = {
  containerSelection?: string | null;
  routePlan?: WizardRoutePlan | null;
  currentStepKey?: string | null;
};

const maybePush = (
  out: WizardStepKey[],
  key: WizardStepKey,
  include: boolean,
) => {
  if (!include) return;
  if (out.includes(key)) return;
  out.push(key);
};

export const buildWizardStepPath = ({
  containerSelection,
  routePlan,
  currentStepKey,
}: WizardStepPathInput): WizardStepKey[] => {
  const current = (currentStepKey ?? "").trim() as WizardStepKey | "";
  const path: WizardStepKey[] = ["location", "container"];

  maybePush(
    path,
    "harness-downloads",
    routePlan?.includeHarnessDownloads === true || current === "harness-downloads",
  );
  maybePush(
    path,
    "auth-import",
    routePlan?.includeAuthImport === true || current === "auth-import",
  );
  maybePush(
    path,
    "session-titling",
    routePlan?.includeTitling === true || current === "session-titling",
  );

  path.push("source");

  maybePush(
    path,
    "network",
    (containerSelection ?? "") !== "" && containerSelection !== "host"
      || current === "network",
  );

  path.push("setup", "merge-queue", "confirm");
  return path;
};

export const resolveWizardCurrentStepKey = (
  stepKeys: WizardStepKey[],
  currentKey: string,
  fallbackIndex = 0,
): WizardStepKey => {
  if (!stepKeys.length) return "location";
  if (stepKeys.includes(currentKey as WizardStepKey)) return currentKey as WizardStepKey;
  const clamped = Math.max(0, Math.min(stepKeys.length - 1, fallbackIndex));
  return stepKeys[clamped];
};

export const stepKeyOffset = (
  stepKeys: WizardStepKey[],
  currentKey: WizardStepKey,
  delta: number,
): WizardStepKey => {
  if (!stepKeys.length) return currentKey;
  const idx = stepKeys.indexOf(currentKey);
  const base = idx >= 0 ? idx : 0;
  const next = Math.max(0, Math.min(stepKeys.length - 1, base + delta));
  return stepKeys[next];
};

export const nextBoundaryStep = (plan: WizardRoutePlan | null): WizardStepKey => {
  if (plan?.includeHarnessDownloads) return "harness-downloads";
  if (plan?.includeAuthImport) return "auth-import";
  if (plan?.includeTitling) return "session-titling";
  return "source";
};

export const nextAfterHarnessDownloads = (plan: WizardRoutePlan | null): WizardStepKey => {
  if (plan?.includeAuthImport) return "auth-import";
  if (plan?.includeTitling) return "session-titling";
  return "source";
};

export const nextAfterAuthImport = (plan: WizardRoutePlan | null): WizardStepKey =>
  plan?.includeTitling ? "session-titling" : "source";
