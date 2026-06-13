import { useEffect, useMemo, useState } from "react";
import type React from "react";
import { providerDetailFlag } from "../../utils/boolish";
import { PROVIDER_INSTALLS_ENABLED } from "../../utils/providerInstallGate";
import {
  isReadyVisibleHarnessProviderStatus,
  isVisibleHarnessProviderStatus,
} from "../../utils/providerInventory";
import { hasConfiguredHarnessAuth } from "../../utils/providerAuthStatus";
import { UNSUPPORTED_HARNESS_IDS } from "../../utils/harnessCatalog";
import { trackFeatureUsed, trackProviderSelected } from "../../utils/analytics";
import { installErrorSummary } from "../../utils/providerInstallUi";
import { TextInput } from "../ui/text-input";
import { MenuTitleRow } from "./WorkbenchComposerMenu";
import { MENU_DESCRIPTIONS } from "./WorkbenchComposer.utils";
import type { NewSessionProps } from "./WorkbenchComposer.types";

type WorkbenchComposerHarnessMenuProps = {
  logoClasses: (base: string, invertInDark?: boolean, invertInLight?: boolean) => string;
  menuRef: React.RefObject<HTMLDivElement | null>;
  menuStyle: React.CSSProperties | null;
  newSession: NewSessionProps;
  onClose: () => void;
};

type HarnessOption = NewSessionProps["harnessCatalog"][number];

function deriveVisibleHarnessOptions(newSession: NewSessionProps, harnessSearch: string) {
  const q = harnessSearch.trim().toLowerCase();
  const catalog = newSession.harnessCatalog.filter(
    (h) =>
      isVisibleHarnessProviderStatus(newSession.providersById[h.id])
      && !UNSUPPORTED_HARNESS_IDS.has(String(h.id)),
  );
  const order = new Map<string, number>(catalog.map((h, idx) => [h.id, idx]));
  const extras = Object.keys(newSession.providersById)
    .filter(
      (id) =>
        !order.has(id)
        && isVisibleHarnessProviderStatus(newSession.providersById[id])
        && !UNSUPPORTED_HARNESS_IDS.has(String(id)),
    )
    .map((id): HarnessOption => ({ id, label: id, logoSrc: "" }))
    .sort((a, b) => String(a.id).localeCompare(String(b.id)));

  const all = [...catalog, ...extras];
  const filtered = q
    ? all.filter(
        (h) =>
          String(h.id).toLowerCase().includes(q) || String(h.label).toLowerCase().includes(q),
      )
    : all;

  if (PROVIDER_INSTALLS_ENABLED) return filtered;
  return filtered.filter((h) => {
    const status = newSession.providersById[String(h.id)];
    return isReadyVisibleHarnessProviderStatus(status);
  });
}

function hasInstalledHarnessBinary(
  status: NewSessionProps["providersById"][string] | null | undefined,
): boolean {
  return isVisibleHarnessProviderStatus(status) && status.installed === true;
}

export function WorkbenchComposerHarnessMenu({
  logoClasses,
  menuRef,
  menuStyle,
  newSession,
  onClose,
}: WorkbenchComposerHarnessMenuProps) {
  const [harnessSearch, setHarnessSearch] = useState("");

  useEffect(() => {
    for (const [providerId, status] of Object.entries(newSession.providersById)) {
      if (!hasInstalledHarnessBinary(status)) continue;
      newSession.ensureProviderAuthSummary(providerId).catch(() => {});
    }
  }, [newSession]);

  const visibleHarnesses = useMemo(
    () => deriveVisibleHarnessOptions(newSession, harnessSearch),
    [newSession, harnessSearch],
  );

  const hasSupportedMissing = useMemo(
    () =>
      Object.values(newSession.providersById).some(
        (status) =>
          providerDetailFlag(status.details, "install_supported")
          && !hasInstalledHarnessBinary(status),
      ),
    [newSession.providersById],
  );

  const toggleHarness = (providerId: string) => {
    const status = newSession.providersById[providerId];
    const binaryInstalled = hasInstalledHarnessBinary(status);
    if (!binaryInstalled) return;

    const hasActiveAuth = hasConfiguredHarnessAuth(providerId, newSession.providerOptions[providerId]);
    if (!hasActiveAuth) {
      trackFeatureUsed("harness_auth_requested", {
        provider_id: providerId,
        entry_surface: "workbench_new_task",
      });
      newSession.onRequestHarnessAuth?.(providerId);
      onClose();
      return;
    }

    const installed = isReadyVisibleHarnessProviderStatus(status);
    if (!installed) return;

    if (newSession.draftHarness?.providerId !== providerId) {
      trackProviderSelected({
        providerId,
        source: "provider_switch",
      });
    }

    const authReadyVisibleHarnessCount = Object.keys(newSession.providersById).filter(
      (id) =>
        !UNSUPPORTED_HARNESS_IDS.has(id)
        && isReadyVisibleHarnessProviderStatus(newSession.providersById[id])
        && hasConfiguredHarnessAuth(id, newSession.providerOptions[id]),
    ).length;

    newSession.setDraftHarness((prev) => {
      if (prev?.providerId === providerId) {
        return authReadyVisibleHarnessCount <= 1 ? prev : null;
      }
      return { providerId, modelId: "", preferenceExplicit: false };
    });
    newSession.ensureProviderAuthSummary(providerId).catch(() => {});
    onClose();
  };

  return (
    <div className="wb-menu wb-harness-menu" role="menu" ref={menuRef} style={menuStyle ?? undefined}>
      <div className="wb-menu-top">
        <MenuTitleRow title="Agents" description={MENU_DESCRIPTIONS.harness} tooltipId="wb-menu-tooltip-harness" />
        <TextInput
          className="wb-menu-search"
          value={harnessSearch}
          onChange={(e) => setHarnessSearch(e.target.value)}
          placeholder="Search agents"
          aria-label="Search agents"
          autoFocus
        />

        {PROVIDER_INSTALLS_ENABLED ? (
          <button
            type="button"
            className="wb-harness-install-all"
            onClick={() => newSession.onInstallAllProviders()}
            disabled={!hasSupportedMissing || (newSession.installAllBusy ?? false)}
            title={hasSupportedMissing ? "Install all supported harnesses" : "No supported harnesses to install"}
          >
            {newSession.installAllBusy ? "Installing…" : "Install all"}
          </button>
        ) : null}
      </div>

      <div className="wb-harness-list">
        {visibleHarnesses.length === 0 ? <div className="wb-menu-empty">No matching agents.</div> : null}
        {visibleHarnesses.map((h) => {
          const id = String(h.id);
          const label = String(h.label ?? id);
          const providerStatus = newSession.providersById[id];
          const binaryInstalled = hasInstalledHarnessBinary(providerStatus);
          const installed = isReadyVisibleHarnessProviderStatus(providerStatus);
          const updateAvailable =
            providerDetailFlag(providerStatus?.details, "matrix_update_available")
            || providerDetailFlag(providerStatus?.details, "managed_dependency_update_available");
          const installSupported = providerDetailFlag(providerStatus?.details, "install_supported");
          const installUi = newSession.providerInstallsById[id];
          const installRunning =
            installUi?.state === "running" || providerDetailFlag(newSession.providersById[id]?.details, "install_running");
          const installFinishing = installUi?.state === "succeeded" && !binaryInstalled;
          const installBusy = installRunning || installFinishing;
          const installPct =
            installUi?.state === "running"
              ? (typeof installUi?.pct === "number"
                ? installUi.pct
                : null)
              : null;
          const installButtonLabel =
            installRunning
              ? `${Math.max(0, Math.min(100, installPct ?? 0))}%`
              : installFinishing
                ? "Finalizing…"
                : updateAvailable
                  ? "Update"
                  : "Install";
          const installButtonTitle =
            !installSupported
              ? "Install not supported yet"
              : installRunning
                ? "Install in progress"
                : installFinishing
                  ? "Install finishing"
                  : updateAvailable
                    ? "Update this harness"
                    : "Install this harness";
          const installButtonStyle =
            installRunning
              ? ({ "--wb-install-pct": `${Math.max(0, Math.min(100, installPct ?? 0))}%` } as React.CSSProperties)
              : undefined;
          const installFailureMessage =
            installUi?.state === "failed" || installUi?.state === "cancelled"
              ? installErrorSummary(installUi.errorCode, installUi.error)
              : null;
          const showInstallActions =
            PROVIDER_INSTALLS_ENABLED
            && (!binaryInstalled || updateAvailable || installBusy || installFailureMessage !== null);
          const checked = newSession.draftHarness?.providerId === id;
          const hasActiveAuth = hasConfiguredHarnessAuth(id, newSession.providerOptions[id]);
          const canOpenHarnessRow = installed || (binaryInstalled && !hasActiveAuth && !installBusy);

          return (
            <div key={id} className={`wb-harness-row ${canOpenHarnessRow ? "" : "wb-disabled"}`}>
              <button
                type="button"
                className="wb-harness-row-main"
                onClick={() => toggleHarness(id)}
                disabled={!canOpenHarnessRow}
              >
                <span className={`wb-check ${checked ? "wb-check-on" : ""}`} aria-hidden="true">
                  {checked ? "✓" : ""}
                </span>
                {h.logoSrc ? (
                  <img
                    className={logoClasses("wb-harness-logo", h.invertInDark, h.invertInLight)}
                    src={h.logoSrc}
                    alt=""
                  />
                ) : null}
                <span className="wb-harness-name">{label}</span>
                {binaryInstalled && !showInstallActions ? (
                  <span className="wb-harness-status-lights">
                    <span
                      className={`wb-harness-auth-dot ${hasActiveAuth ? "wb-harness-auth-dot-active" : "wb-harness-auth-dot-inactive"}`}
                      aria-label={hasActiveAuth ? "Authentication configured" : "Authentication not configured"}
                      title={hasActiveAuth ? "Authentication configured" : "Authentication not configured"}
                    />
                  </span>
                ) : null}
              </button>

              {showInstallActions ? (
                <div className="wb-harness-actions">
                  <button
                    type="button"
                    className={`wb-harness-install${installBusy ? " wb-harness-install-busy" : ""}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      if (installBusy) return;
                      newSession.onInstallProvider(id);
                    }}
                    disabled={!installSupported}
                    title={installButtonTitle}
                    style={installButtonStyle}
                  >
                    {installButtonLabel}
                  </button>
                  {installFailureMessage ? (
                    <span className="wb-harness-install-error" title={installFailureMessage}>
                      {installFailureMessage}
                    </span>
                  ) : null}
                </div>
              ) : null}
            </div>
          );
        })}
      </div>
    </div>
  );
}
