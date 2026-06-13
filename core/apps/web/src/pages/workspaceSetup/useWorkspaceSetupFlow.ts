import { useCallback, useEffect, useMemo, useReducer, useRef } from "react";
import { getSourceStepValidation } from "./WorkspaceSetupPage.logic";
import {
  buildWizardStepPath,
  resolveWizardCurrentStepKey,
  stepKeyOffset,
  type WizardRoutePlan,
  type WizardStepKey,
} from "./wizardFlow";
import {
  createInitialWizardFlowState,
  wizardFlowReducer,
} from "./wizardFlowReducer";
import type { WizardStep } from "./wizardTypes";
type UseWorkspaceSetupFlowArgs = {
  sourcePath: string;
  repoUrl: string;
};

const buildSteps = (stepKeys: WizardStepKey[]): WizardStep[] => {
  const stepMap: Record<WizardStepKey, WizardStep> = {
    "location": {
      key: "location",
      title: "Location",
      note: "Where will this workspace run?",
      options: [
        { id: "local", title: "Local", desc: "Agents run on this machine." },
        { id: "remote", title: "Remote", desc: "Agents run on your existing dev box (remote IDE experience)." },
      ],
    },
    "container": {
      key: "container",
      title: "Agent Sandbox Isolation",
      note: "Choose whether agents run directly on the host or in a sandbox for this workspace.",
      options: [
        {
          id: "sandbox",
          title: "Sandbox",
          desc: "Run agents in the standard isolated workspace sandbox. ctx mediates shells, files, git, and network policy.",
          badge: "Recommended",
        },
        {
          id: "host",
          title: "Host",
          desc: "Run directly on the host. Useful if this machine is already agent-safe (e.g. a dedicated dev box).",
        },
      ],
    },
    "harness-downloads": {
      key: "harness-downloads",
      title: "Harness Downloads",
      note: "Choose which harness providers to download now.",
    },
    "auth-import": {
      key: "auth-import",
      title: "Import Existing Auth",
      note: "Import existing provider credentials or add them later.",
    },
    "session-titling": {
      key: "session-titling",
      title: "Task Titling",
      note: "Choose an LLM source for generating task titles.",
    },
    "source": {
      key: "source",
      title: "Source",
      note: "How should we create the workspace?",
      options: [
        { id: "clone", title: "Clone repo", desc: "Git URL + optional branch." },
        { id: "import", title: "Import folder", desc: "Use an existing git repo folder path." },
        { id: "new", title: "New empty", desc: "Initialize a new git repo." },
      ],
    },
    "network": {
      key: "network",
      title: "Network Policy",
      note: "Restrict or permit agent network access (sandbox mode only).",
      options: [
        {
          id: "providers",
          title: "LLM providers only",
          desc: "Only allow validated LLM provider traffic. This blocks other outbound access.",
        },
        {
          id: "allowlist",
          title: "Allowlist",
          desc: "Allow only hosts you approve (one per line). Useful for known-safe sources.",
        },
        {
          id: "full",
          title: "Full access",
          desc: "Unrestricted outbound. Only use if you understand prompt-injection / data exfil risks.",
        },
      ],
    },
    "setup": {
      key: "setup",
      title: "Worktree Setup Hook",
      note: "Choose a single shell command to run on new worktree creation (e.g., install dependencies).",
      info: [
        "Each ctx task runs on its own git worktree. A worktree is a separate working directory attached to the same repository.",
        "This allows one or more agents to work on a task branch isolated from other tasks, so work can be done simultaneously.",
        "",
        "Depending on your project, you might want to do setup every time a new worktree is created (for example, installing dependencies).",
        "",
        "If you do not know what to put here, skip it and come back later. You can also ask an agent what the best setup hook is for your project.",
        "",
        'Example prompt: "You are in a freshly created git worktree in this project. Is there any setup that ought to have occurred? If so, is there a single setup command we can run as a worktree setup hook?"',
      ].join("\n"),
    },
    "merge-queue": {
      key: "merge-queue",
      title: "Merge Queue",
      note: "Branch to work from, plus an optional verification command.",
      info: [
        "A merge queue is a queue of pull requests waiting to be merged to a single branch. It helps ensure changes merge cleanly and pass checks.",
        "",
        "With stacked agent-driven changes, two PRs can pass individually but fail once combined. A personal merge queue helps you test stacked changes locally.",
        "",
        "In this setup step, choose a branch to work off of, and an optional verification command. Prefer a lightweight but robust command here (lint, format, build, unit tests), and defer expensive or flaky end-to-end tests to CI or on-demand runs.",
        "",
        "Advanced settings let you automatically push to a remote after a successful local merge.",
      ].join("\n"),
    },
    "confirm": {
      key: "confirm",
      title: "Confirm and create",
      note: "Review your choices before provisioning.",
    },
  };

  return stepKeys.map((key) => stepMap[key]);
};

export function useWorkspaceSetupFlow({
  sourcePath,
  repoUrl,
}: UseWorkspaceSetupFlowArgs) {
  const [flowState, dispatchFlow] = useReducer(
    wizardFlowReducer,
    undefined,
    createInitialWizardFlowState,
  );
  const {
    currentStepKey,
    selections,
    routePlan,
    routePlanningBusy,
  } = flowState;
  const previousStepIndexRef = useRef(0);
  const currentStepKeyRef = useRef<WizardStepKey>("location");

  const stepKeys = useMemo<WizardStepKey[]>(
    () => buildWizardStepPath({
      containerSelection: selections.container,
      routePlan,
      currentStepKey,
    }),
    [currentStepKey, routePlan, selections.container],
  );

  const steps = useMemo(() => buildSteps(stepKeys), [stepKeys]);
  const resolvedCurrentStepKey = useMemo<WizardStepKey>(
    () => resolveWizardCurrentStepKey(stepKeys, currentStepKey, previousStepIndexRef.current),
    [currentStepKey, stepKeys],
  );

  useEffect(() => {
    if (resolvedCurrentStepKey === currentStepKey) return;
    dispatchFlow({ type: "set_step", stepKey: resolvedCurrentStepKey });
  }, [currentStepKey, resolvedCurrentStepKey]);

  const stepIndex = Math.max(0, stepKeys.indexOf(resolvedCurrentStepKey));
  const step = steps[stepIndex];
  const isFirst = stepIndex === 0;
  const isLast = stepIndex === steps.length - 1;

  useEffect(() => {
    previousStepIndexRef.current = stepIndex;
  }, [stepIndex]);

  useEffect(() => {
    currentStepKeyRef.current = step.key;
  }, [step.key]);

  const goToStepKey = useCallback((key: WizardStepKey) => {
    dispatchFlow({ type: "set_step", stepKey: key });
  }, []);

  const goRelativeStep = useCallback((delta: number) => {
    const resolved = resolveWizardCurrentStepKey(stepKeys, currentStepKey, previousStepIndexRef.current);
    dispatchFlow({
      type: "set_step",
      stepKey: stepKeyOffset(stepKeys, resolved, delta),
    });
  }, [currentStepKey, stepKeys]);

  const requiresSelection = Boolean(step.options?.length);
  const hasSelection = Boolean(selections[step.key]);
  const mergeQueueSkipped = selections["merge-queue"] === "skip";
  const isSourceStep = step.key === "source";
  const useSandboxStaging =
    selections.container === "sandbox"
    && (selections.source === "clone" || selections.source === "new");
  const sourceStepValidation = getSourceStepValidation({
    source: selections.source,
    sourcePath,
    repoUrl,
    useSandboxStaging,
  });
  const needsSourcePath = isSourceStep && sourceStepValidation.needsSourcePath;

  const selectOption = (stepKey: string, optionId: string) => {
    dispatchFlow({ type: "select_option", stepKey, optionId });
  };

  const clearSelection = (stepKey: string) => {
    dispatchFlow({ type: "clear_selection", stepKey });
  };

  const invalidateRoutePlan = () => {
    dispatchFlow({ type: "invalidate_route_plan" });
  };

  const setRoutePlan = (nextRoutePlan: WizardRoutePlan | null) => {
    dispatchFlow({ type: "set_route_plan", routePlan: nextRoutePlan });
  };

  const setRoutePlanningBusy = (busy: boolean) => {
    dispatchFlow({ type: "set_route_planning_busy", busy });
  };

  return {
    currentStepKey,
    currentStepKeyRef,
    selections,
    routePlan,
    routePlanningBusy,
    stepKeys,
    steps,
    step,
    stepIndex,
    isFirst,
    isLast,
    requiresSelection,
    hasSelection,
    mergeQueueSkipped,
    useSandboxStaging,
    sourceStepValidation,
    needsSourcePath,
    goToStepKey,
    goRelativeStep,
    selectOption,
    clearSelection,
    invalidateRoutePlan,
    setRoutePlan,
    setRoutePlanningBusy,
  };
}
