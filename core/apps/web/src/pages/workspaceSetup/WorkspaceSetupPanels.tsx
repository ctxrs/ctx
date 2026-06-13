import type { Dispatch, RefObject, SetStateAction } from "react";
import type {
  InstallErrorCode,
  InstallTarget,
  ProviderAuthImportCandidate,
} from "../../api/client";
import {
  formatByteSize,
  installErrorSummary,
  installTargetLabel,
} from "../../utils/providerInstallUi";

type HarnessMeta = {
  logoSrc?: string;
  invertInDark?: boolean;
  invertInLight?: boolean;
};

type HarnessInstallCandidate = {
  providerId: string;
  label: string;
  installed: boolean;
  healthy: boolean;
  installSupported: boolean;
  installRunning: boolean;
  installId?: string;
  installTarget?: InstallTarget;
  installSizeBytes?: number | null;
};

type HarnessInstallRowState = {
  installId: string;
  state: "checking" | "queued" | "running" | "succeeded" | "failed" | "cancelled";
  pct: number | null;
  target?: InstallTarget;
  errorCode?: string;
  error?: string;
};

const logoClasses = (base: string, invertInDark?: boolean, invertInLight?: boolean): string =>
  [base, invertInDark ? "wb-invert" : "", invertInLight ? "wb-invert-light" : ""]
    .filter(Boolean)
    .join(" ");

type AuthImportStepPanelProps = {
  busy: boolean;
  error: string | null;
  candidates: ProviderAuthImportCandidate[];
  selected: Record<string, boolean>;
  setSelected: Dispatch<SetStateAction<Record<string, boolean>>>;
  harnessByProviderId: Map<string, HarnessMeta>;
  onSkip: () => void;
};

export function AuthImportStepPanel({
  busy,
  error,
  candidates,
  selected,
  setSelected,
  harnessByProviderId,
  onSkip,
}: AuthImportStepPanelProps) {
  return (
    <div className="wizard-input">
      {busy ? <div className="wizard-note">Scanning/importing credentials…</div> : null}
      {error ? <div className="wizard-error">{error}</div> : null}
      {!busy && !candidates.length ? (
        <div className="wizard-note">No import candidates found on this host.</div>
      ) : null}
      {candidates.length > 0 && (
        <div className="wizard-auth-import-list">
          {candidates.map((candidate) => {
            const checked = Boolean(selected[candidate.id]);
            const importable = candidate.parse_status === "parsed";
            const harness = harnessByProviderId.get(candidate.provider_id);
            const showSummary = candidate.summary
              && !(candidate.provider_id === "codex" && candidate.summary === "Codex auth session");
            const showUnsupportedReason = candidate.unsupported_reason
              && !(
                candidate.provider_id === "cursor"
                && candidate.unsupported_reason.includes("secure local storage without a canonical import file path")
              );
            return (
              <label
                key={candidate.id}
                className={`wizard-auth-import-row ${importable ? "" : "wizard-auth-import-row--disabled"}`}
              >
                <div className="wizard-auth-import-title">
                  <input
                    type="checkbox"
                    className="wizard-auth-import-checkbox"
                    checked={checked}
                    disabled={!importable || busy}
                    onChange={(event) =>
                      setSelected((prev) => ({ ...prev, [candidate.id]: event.target.checked }))
                    }
                  />
                  {harness?.logoSrc ? (
                    <img
                      className={logoClasses(
                        "wizard-auth-import-logo",
                        harness.invertInDark,
                        harness.invertInLight,
                      )}
                      src={harness.logoSrc}
                      alt=""
                    />
                  ) : (
                    <span className="wizard-auth-import-logo-fallback" aria-hidden="true" />
                  )}
                  <span className="wizard-auth-import-name">{candidate.provider_label}</span>
                </div>
                <div className="wizard-auth-import-path">Source: {candidate.path}</div>
                {showSummary ? <div className="wizard-note wizard-note--tight">{candidate.summary}</div> : null}
                {showUnsupportedReason ? (
                  <div className="wizard-note wizard-note--tight">{candidate.unsupported_reason}</div>
                ) : null}
              </label>
            );
          })}
        </div>
      )}
      <button
        type="button"
        className="wizard-skip wizard-skip--left wizard-skip--below"
        onClick={onSkip}
        disabled={busy}
      >
        Skip for now
      </button>
    </div>
  );
}

type HarnessDownloadsStepPanelProps = {
  busy: boolean;
  error: string | null;
  selectedRunningCount: number;
  selectedBlockedCount: number;
  candidates: HarnessInstallCandidate[];
  canScroll: boolean;
  atBottom: boolean;
  scrollRef: RefObject<HTMLDivElement | null>;
  onScroll: () => void;
  selected: Record<string, boolean>;
  setSelected: Dispatch<SetStateAction<Record<string, boolean>>>;
  rows: Record<string, HarnessInstallRowState>;
  selectedInstallTarget: InstallTarget;
  harnessByProviderId: Map<string, HarnessMeta>;
  onCancelInstall: (providerId: string) => void;
  onSkip: () => void;
};

export function HarnessDownloadsStepPanel({
  busy,
  error,
  selectedRunningCount,
  selectedBlockedCount,
  candidates,
  canScroll,
  atBottom,
  scrollRef,
  onScroll,
  selected,
  setSelected,
  rows,
  selectedInstallTarget,
  harnessByProviderId,
  onCancelInstall,
  onSkip,
}: HarnessDownloadsStepPanelProps) {
  return (
    <div className="wizard-input">
      {busy ? <div className="wizard-note">Checking/downloading harnesses…</div> : null}
      {error ? <div className="wizard-error">{error}</div> : null}
      {!busy && selectedRunningCount > 0 ? (
        <div className="wizard-note">
          Selected downloads are running in the background. Continue now, or stay here to watch/cancel them.
        </div>
      ) : null}
      {!busy && selectedBlockedCount > 0 ? (
        <div className="wizard-note">
          One or more selected downloads failed or were canceled. Continue now and retry later if you still want those harnesses here.
        </div>
      ) : null}
      {!busy && !candidates.length ? (
        <div className="wizard-note">No downloadable harness providers detected on this daemon.</div>
      ) : null}
      {candidates.length > 0 && (
        <div
          className={`wizard-harness-downloads-scroll-shell${canScroll ? " is-scrollable" : ""}${atBottom ? " is-bottom" : ""}`}
          data-testid="wizard-harness-downloads-scroll-shell"
        >
          <div
            ref={scrollRef}
            className="wizard-auth-import-list wizard-auth-import-list--scrollable"
            onScroll={onScroll}
          >
            {candidates.map((candidate) => {
              const checked = Boolean(selected[candidate.providerId]);
              const installedReady = candidate.installed && candidate.healthy;
              const installUi = rows[candidate.providerId];
              const running = installUi?.state === "running" || candidate.installRunning;
              const canCancelInstall = Boolean(installUi?.installId ?? candidate.installId);
              const disabled = !candidate.installSupported || installedReady || running || busy;
              const harness = harnessByProviderId.get(candidate.providerId);
              const installTarget = installUi?.target ?? candidate.installTarget ?? selectedInstallTarget;
              const sizeLabel = formatByteSize(candidate.installSizeBytes ?? null);
              const installContextLabel = `${installTargetLabel(installTarget)}${sizeLabel ? ` · ${sizeLabel}` : ""}`;
              const installFailureMessage =
                installUi?.state === "failed" || installUi?.state === "cancelled"
                  ? installErrorSummary(installUi.errorCode as InstallErrorCode | undefined, installUi.error)
                  : null;
              return (
                <label
                  key={candidate.providerId}
                  className={`wizard-auth-import-row ${disabled ? "wizard-auth-import-row--disabled" : ""}`}
                >
                  <div className="wizard-auth-import-title">
                    <input
                      type="checkbox"
                      className="wizard-auth-import-checkbox"
                      data-testid={`wizard-harness-checkbox-${candidate.providerId}`}
                      checked={checked}
                      disabled={disabled}
                      onChange={(event) =>
                        setSelected((prev) => ({ ...prev, [candidate.providerId]: event.target.checked }))
                      }
                    />
                    {harness?.logoSrc ? (
                      <img
                        className={logoClasses(
                          "wizard-auth-import-logo",
                          harness.invertInDark,
                          harness.invertInLight,
                        )}
                        src={harness.logoSrc}
                        alt=""
                      />
                    ) : (
                      <span className="wizard-auth-import-logo-fallback" aria-hidden="true" />
                    )}
                    <span className="wizard-auth-import-name">{candidate.label}</span>
                  </div>
                  <div className="wizard-auth-import-path">
                    {installedReady
                      ? `Installed · ${installContextLabel}`
                      : running
                        ? `Downloading${typeof installUi?.pct === "number" ? ` (${installUi.pct}%)` : ""} · ${installContextLabel}`
                        : `Not installed · ${installContextLabel}`}
                  </div>
                  {running ? (
                    <div className="wizard-auth-import-actions">
                      <button
                        type="button"
                        className="wizard-inline-action"
                        onClick={(event) => {
                          event.preventDefault();
                          event.stopPropagation();
                          onCancelInstall(candidate.providerId);
                        }}
                        disabled={!canCancelInstall}
                      >
                        Cancel install
                      </button>
                    </div>
                  ) : null}
                  {installFailureMessage ? (
                    <div className="wizard-error wizard-note--tight">{installFailureMessage}</div>
                  ) : null}
                </label>
              );
            })}
          </div>
        </div>
      )}
      <button
        type="button"
        className="wizard-skip wizard-skip--left wizard-skip--below"
        data-testid="wizard-harness-skip"
        onClick={onSkip}
        disabled={busy}
      >
        Skip for now
      </button>
    </div>
  );
}
