import type { DevRestartProvidersResult } from "../../../api/client";
import { Row } from "../SettingsPage.components";
import { GeneralSection } from "./GeneralSection";

type DevToolsSectionProps = {
  devToolsEnabled: boolean;
  devRestartBusy: boolean;
  devRestartError: string | null;
  devRestartResults: DevRestartProvidersResult[] | null;
  onRestart: (mode: "drain" | "immediate") => void;
};

export function DevToolsSection({
  devToolsEnabled,
  devRestartBusy,
  devRestartError,
  devRestartResults,
  onRestart,
}: DevToolsSectionProps) {
  if (!devToolsEnabled) {
    return <div className="settings-empty">Dev tools are only available in development builds.</div>;
  }

  return (
    <>
      <GeneralSection>
        <div className="settings-preferences-flat">
          <div className="settings-preferences-group">
            <Row
              title="Drain all providers"
              description="Let active turns finish, then restart provider processes."
              control={
                <button
                  type="button"
                  className="settings-btn settings-btn-secondary"
                  onClick={() => onRestart("drain")}
                  disabled={devRestartBusy}
                >
                  {devRestartBusy ? "Draining..." : "Drain"}
                </button>
              }
            />
            <Row
              title="Immediate restart"
              description="Interrupt running turns and restart provider processes immediately."
              control={
                <button
                  type="button"
                  className="settings-btn"
                  onClick={() => onRestart("immediate")}
                  disabled={devRestartBusy}
                >
                  {devRestartBusy ? "Restarting..." : "Restart now"}
                </button>
              }
            />
          </div>
        </div>
      </GeneralSection>
      {devRestartError ? (
        <div className="settings-banner settings-banner-error">{devRestartError}</div>
      ) : null}
      {devRestartResults ? (
        <div className="settings-banner">
          {devRestartResults.map((result) => (
            <div key={result.provider_id} className="settings-meta-line">
              {result.provider_id}: {result.status}
              {result.message ? ` — ${result.message}` : ""}
            </div>
          ))}
        </div>
      ) : null}
    </>
  );
}
