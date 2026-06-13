import type { Dispatch, SetStateAction } from "react";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../../../components/ui/select";
import type { DesktopEditorSettings, DesktopUpdateChannelSettings } from "../../../utils/desktop";
import { TextInput } from "../../../components/ui/text-input";
import type { ThemeMode } from "../../../utils/theme";
import { EDITOR_OPTIONS, UPDATE_CHANNEL_OPTIONS } from "../SettingsPage.constants";
import { Row } from "../SettingsPage.components";
import { GeneralSection } from "./GeneralSection";

type GeneralSettingsSectionProps = {
  theme: ThemeMode;
  onThemeChange: (next: ThemeMode) => void;
  editorSettings: DesktopEditorSettings;
  setEditorSettings: Dispatch<SetStateAction<DesktopEditorSettings>>;
  editorLoaded: boolean;
  editorError: string | null;
  updateChannel: DesktopUpdateChannelSettings["channel"];
  setUpdateChannel: (channel: DesktopUpdateChannelSettings["channel"]) => void;
  updateChannelLoaded: boolean;
  updateChannelError: string | null;
  clientSettingsError: string | null;
  showRemoteAuthority: boolean;
  isDesktopApp: () => boolean;
};

export function GeneralSettingsSection({
  theme,
  onThemeChange,
  editorSettings,
  setEditorSettings,
  editorLoaded,
  editorError,
  updateChannel,
  setUpdateChannel,
  updateChannelLoaded,
  updateChannelError,
  clientSettingsError,
  showRemoteAuthority,
  isDesktopApp,
}: GeneralSettingsSectionProps) {
  return (
    <GeneralSection>
      <div className="settings-preferences-flat">
        <div className="settings-preferences-group settings-preferences-group-center-controls">
          <Row
            title="Theme"
            control={
              <Select value={theme} onValueChange={(value) => onThemeChange(value as ThemeMode)}>
                <SelectTrigger className="settings-control settings-select tw-min-w-[10rem]" aria-label="Theme mode">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="system">System</SelectItem>
                  <SelectItem value="light">Light</SelectItem>
                  <SelectItem value="dark">Dark</SelectItem>
                </SelectContent>
              </Select>
            }
          />
        </div>

        <div className="settings-preferences-group">
          <Row
            title="Default IDE"
            description={isDesktopApp() ? "Used for open-in-editor links." : "Available in the desktop app."}
            control={
              <Select
                value={editorSettings.target}
                onValueChange={(value) =>
                  setEditorSettings((prev) => ({
                    ...prev,
                    target: value as DesktopEditorSettings["target"],
                  }))}
                disabled={!isDesktopApp() || !editorLoaded}
              >
                <SelectTrigger className="settings-control settings-select tw-min-w-[10rem]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {EDITOR_OPTIONS.map((option) => (
                    <SelectItem key={option.value} value={option.value}>
                      {option.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            }
          />
          <Row
            title="Update channel"
            description={isDesktopApp() ? "Stable is recommended for most installs." : "Available in the desktop app."}
            control={
              <Select
                value={updateChannel}
                onValueChange={(value) => setUpdateChannel(value as DesktopUpdateChannelSettings["channel"])}
                disabled={!isDesktopApp() || !updateChannelLoaded}
              >
                <SelectTrigger className="settings-control settings-select tw-min-w-[10rem]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {UPDATE_CHANNEL_OPTIONS.map((option) => (
                    <SelectItem key={option.value} value={option.value}>
                      {option.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            }
          />
          {editorSettings.target === "custom" ? (
            <Row
              title="Custom IDE command"
              description="Command to run when opening files."
              control={
                <TextInput
                  className="settings-control settings-control-wide"
                  value={editorSettings.custom_command ?? ""}
                  onChange={(e) => setEditorSettings((prev) => ({ ...prev, custom_command: e.target.value }))}
                  disabled={!isDesktopApp() || !editorLoaded}
                  placeholder="code --goto {path}:{line}:{col}"
                />
              }
            />
          ) : null}
          {showRemoteAuthority ? (
            <Row
              title="VS Code Remote Authority"
              description="Optional: ssh-remote+my-host for remote worktrees."
              control={
                <TextInput
                  className="settings-control settings-control-wide"
                  value={editorSettings.remote_authority ?? ""}
                  onChange={(e) => setEditorSettings((prev) => ({ ...prev, remote_authority: e.target.value }))}
                  disabled={!isDesktopApp() || !editorLoaded}
                  placeholder="ssh-remote+my-host"
                />
              }
            />
          ) : null}
        </div>
      </div>
      {editorError ? <div className="settings-banner settings-banner-error">{editorError}</div> : null}
      {updateChannelError ? <div className="settings-banner settings-banner-error">{updateChannelError}</div> : null}
      {clientSettingsError ? <div className="settings-banner settings-banner-error">{clientSettingsError}</div> : null}
    </GeneralSection>
  );
}
