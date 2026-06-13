import { useEffect, useMemo, useRef, useCallback } from "react";
import { buildModelCatalog, parseModelId } from "../../utils/modelEffort";
import { isReadyVisibleHarnessProviderStatus } from "../../utils/providerInventory";
import {
  buildModelsForProvider,
  deriveFullModelIdForBase,
  modelIdFromProviderOptions,
  nextAutoSeededModelId,
  normalizeModelDisplayNamesForProvider,
  shouldShowLoadingProviderModels,
} from "./WorkbenchComposer.utils";
import type { ActiveSessionProps, NewSessionProps, WorkbenchComposerProps } from "./WorkbenchComposer.types";

function activeSessionProviderId(activeProps: ActiveSessionProps): string {
  const providerId = activeProps.providerId?.trim();
  if (providerId) return providerId;
  return activeProps.harnessLabel.trim().toLowerCase() === "codex" ? "codex" : "";
}

export function useWorkbenchComposerModelState({
  props,
  variant,
  newSession,
}: {
  props: WorkbenchComposerProps;
  variant: WorkbenchComposerProps["variant"];
  newSession: NewSessionProps | null;
}) {
  const autoSeededModelIdByProviderRef = useRef<Record<string, string>>({});

  const activeModelData = useMemo(() => {
    if (variant === "activeSession") {
      const activeProps = props as ActiveSessionProps;
      const providerId = activeSessionProviderId(activeProps);
      const models = normalizeModelDisplayNamesForProvider(providerId, activeProps.availableModels);
      const catalog = buildModelCatalog(models);
      const parsed = parseModelId(activeProps.currentModelId, catalog);
      return { models, catalog, parsed, loading: false, fromProviderOptions: false };
    }

    const primary = newSession?.draftHarness ?? null;
    if (!primary) return { models: [], catalog: buildModelCatalog([]), parsed: parseModelId(""), loading: false, fromProviderOptions: true };
    const opts = newSession?.providerOptions[primary.providerId];
    const models = buildModelsForProvider(primary.providerId, opts);
    const catalog = buildModelCatalog(models);
    const parsed = parseModelId(primary.modelId, catalog);
    const loading = shouldShowLoadingProviderModels(primary.providerId, opts);
    return { models, catalog, parsed, loading, fromProviderOptions: true };
  }, [newSession, props, variant]);

  const providerIdsToEnsure = useMemo(() => {
    if (!newSession) return [];
    return [newSession.draftHarness?.providerId ?? newSession.defaultProviderId].filter(Boolean);
  }, [newSession?.defaultProviderId, newSession?.draftHarness]);

  useEffect(() => {
    if (!newSession) return;
    for (const providerId of providerIdsToEnsure) {
      const status = newSession.providersById[providerId];
      if (!isReadyVisibleHarnessProviderStatus(status)) continue;
      newSession.ensureProviderAuthSummary(providerId).catch(() => {});
    }
  }, [newSession?.ensureProviderAuthSummary, newSession?.providerOptions, newSession?.providersById, providerIdsToEnsure]);

  useEffect(() => {
    if (!newSession) return;
    const primary = newSession.draftHarness ?? null;
    if (!primary) return;
    if (primary.preferenceExplicit) return;
    const providerId = primary.providerId;
    const opts = newSession.providerOptions[providerId];
    const next = modelIdFromProviderOptions(opts);
    const previousAutoSeed = autoSeededModelIdByProviderRef.current[providerId] ?? null;
    const nextSeededModelId = nextAutoSeededModelId(primary.modelId, next, previousAutoSeed);
    if (!nextSeededModelId) return;
    autoSeededModelIdByProviderRef.current[providerId] = nextSeededModelId;
    newSession.setDraftHarness((prev) =>
      prev && prev.providerId === providerId ? { ...prev, modelId: nextSeededModelId } : prev,
    );
  }, [newSession?.draftHarness, newSession?.providerOptions, newSession?.setDraftHarness]);

  const showModelEffort = useMemo(() => {
    if (variant === "newSession") {
      return !!newSession?.draftHarness;
    }
    return true;
  }, [newSession?.draftHarness, variant]);

  const currentBase = activeModelData.parsed.base || activeModelData.catalog.baseIds[0] || "";
  const currentEffort = activeModelData.parsed.effort;
  const effortOptions = activeModelData.catalog.effortsByBase[currentBase] ?? [];
  const currentModelLabel = useMemo(() => {
    if (variant === "activeSession") {
      const activeProps = props as ActiveSessionProps;
      const displayLabel = activeProps.currentModelDisplayLabel?.trim();
      const isCodex = activeSessionProviderId(activeProps).trim().toLowerCase() === "codex";
      if (displayLabel && !isCodex) return displayLabel;
    }
    if (currentBase) {
      return activeModelData.catalog.displayNameByBase[currentBase] ?? currentBase;
    }
    return "Model";
  }, [activeModelData.catalog.displayNameByBase, currentBase, props, variant]);

  const setActiveModelId = useCallback(
    (nextFullId: string) => {
      if (variant === "activeSession") {
        (props as ActiveSessionProps).onSetModelId(nextFullId);
        return;
      }
      const newSessionProps = props as NewSessionProps;
      newSessionProps.setDraftHarness((prev) =>
        prev ? { ...prev, modelId: nextFullId, preferenceExplicit: true } : prev,
      );
    },
    [props, variant],
  );

  const harnessControl = useMemo(() => {
    if (variant === "activeSession") {
      const activeProps = props as ActiveSessionProps;
      return {
        label: activeProps.harnessLabel,
        logoSrc: activeProps.harnessLogoSrc,
        invertInDark: activeProps.harnessLogoInvert,
        invertInLight: activeProps.harnessLogoInvertInLight,
        locked: true,
      };
    }

    const newSessionProps = props as NewSessionProps;
    const primary = newSessionProps.draftHarness ?? null;
    if (!primary) {
      return {
        label: "Select agent",
        logoSrc: "",
        invertInDark: false,
        invertInLight: false,
        locked: false,
      };
    }
    const providerId = primary.providerId;
    const info = newSessionProps.harnessCatalog.find((harness) => harness.id === providerId);
    const label = info?.label ?? providerId;
    return {
      label,
      logoSrc: info?.logoSrc,
      invertInDark: info?.invertInDark,
      invertInLight: info?.invertInLight,
      locked: false,
    };
  }, [props, variant]);

  return {
    activeModelData,
    currentBase,
    currentEffort,
    currentModelLabel,
    effortOptions,
    harnessControl,
    setActiveModelId,
    showModelEffort,
    deriveFullModelIdForBase,
  };
}
