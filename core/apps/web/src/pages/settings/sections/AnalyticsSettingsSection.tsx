import { Row, Toggle } from "../SettingsPage.components";
import { GeneralSection } from "./GeneralSection";

type AnalyticsSettingsSectionProps = {
  clientTelemetryEnabled: boolean;
  clientLoaded: boolean;
  clientSaving: boolean;
  clientError: string | null;
  setClientTelemetryEnabled: (next: boolean) => Promise<void>;
  daemonTelemetryEnabled: boolean;
  daemonLoaded: boolean;
  daemonTelemetrySource: "default" | "configured";
  setDaemonTelemetryEnabled: (next: boolean) => void;
};

export function AnalyticsSettingsSection({
  clientTelemetryEnabled,
  clientLoaded,
  clientSaving,
  clientError,
  setClientTelemetryEnabled,
  daemonTelemetryEnabled,
  daemonLoaded,
  daemonTelemetrySource,
  setDaemonTelemetryEnabled,
}: AnalyticsSettingsSectionProps) {
  return (
    <GeneralSection>
      <div className="settings-preferences-flat">
        <div className="settings-preferences-group">
          <Row
            title="This Device"
            description="Share anonymous product and incident summaries from this client. Does not include prompts, code, or tool output bodies."
            control={
              <Toggle
                checked={clientTelemetryEnabled}
                disabled={!clientLoaded || clientSaving}
                onChange={(next) => {
                  void setClientTelemetryEnabled(next);
                }}
                ariaLabel="This Device Telemetry"
              />
            }
          />
          <Row
            title="Connected Daemon Host"
            description="Control remote semantic telemetry sends for the currently connected daemon host."
            control={
              <Toggle
                checked={daemonTelemetryEnabled}
                disabled={!daemonLoaded}
                onChange={setDaemonTelemetryEnabled}
                ariaLabel="Connected Daemon Host Telemetry"
              />
            }
          />
          {daemonTelemetrySource === "default"
            ? (
              <div className="settings-note">
                This daemon is still on its default telemetry policy. New hosts can inherit your
                current client preference before they are explicitly configured.
              </div>
            )
            : null}
          {clientError ? <div className="settings-banner settings-banner-error">{clientError}</div> : null}
        </div>
      </div>
    </GeneralSection>
  );
}
