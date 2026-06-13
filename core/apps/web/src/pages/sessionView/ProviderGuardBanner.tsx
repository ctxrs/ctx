import { useRelativeNowMs } from "../../utils/useRelativeNowMs";
import { formatElapsedMs } from "./SessionPage.helpers";

type ProviderGuardBannerProps = {
  heading: string;
  message: string;
  providerLabel?: string;
  pidLabel: string | null;
  memoryLabel: string | null;
  systemLabel: string | null;
  notice:
    | {
        kind: string;
        stage?: string | null;
        killAtMs?: number | null;
      }
    | null;
  actionBusy: boolean;
  actionError: string | null;
  canRaiseLimit: boolean;
  onRaiseLimit: () => void | Promise<void>;
  onDisableGuard: () => void | Promise<void>;
};

function ProviderGuardCountdownLine({
  notice,
}: {
  notice: ProviderGuardBannerProps["notice"];
}) {
  const enabled = notice?.kind === "provider_guard_warning" && notice.stage === "max" && notice.killAtMs != null;
  const nowMs = useRelativeNowMs(1000, enabled);
  if (!enabled || notice.killAtMs == null) return null;

  const remainingMs = notice.killAtMs - nowMs;
  const text =
    remainingMs > 0
      ? `Kill in ${formatElapsedMs(remainingMs)} unless memory drops.`
      : "Kill imminent unless memory drops.";
  return <div className="muted">{text}</div>;
}

export function ProviderGuardBanner({
  heading,
  message,
  providerLabel,
  pidLabel,
  memoryLabel,
  systemLabel,
  notice,
  actionBusy,
  actionError,
  canRaiseLimit,
  onRaiseLimit,
  onDisableGuard,
}: ProviderGuardBannerProps) {
  if (!notice) return null;

  return (
    <div className="banner" role="alert">
      <div className="row" style={{ justifyContent: "space-between" }}>
        <strong>{heading}</strong>
        {providerLabel ? <span className="muted">{providerLabel}</span> : null}
      </div>
      <div
        className={notice.kind === "provider_guard_kill" ? "error" : "muted"}
        style={{ whiteSpace: "pre-wrap" }}
      >
        {message}
      </div>
      <div className="row" style={{ flexWrap: "wrap", gap: 12 }}>
        {memoryLabel ? <span className="muted">{memoryLabel}</span> : null}
        {systemLabel ? <span className="muted">{systemLabel}</span> : null}
        {pidLabel ? <span className="muted">{pidLabel}</span> : null}
      </div>
      <ProviderGuardCountdownLine notice={notice} />
      <div className="row" style={{ flexWrap: "wrap", gap: 8 }}>
        <button
          type="button"
          disabled={actionBusy || !canRaiseLimit}
          onClick={() => void onRaiseLimit()}
          title={!canRaiseLimit ? "System memory total is unavailable." : undefined}
        >
          Raise limit to 90%
        </button>
        <button type="button" disabled={actionBusy} onClick={() => void onDisableGuard()}>
          Disable guard
        </button>
        {actionError ? <span className="error">{actionError}</span> : null}
      </div>
    </div>
  );
}
