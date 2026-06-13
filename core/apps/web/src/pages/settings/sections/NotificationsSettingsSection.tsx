import type { ClientSettingsState } from "../../../state/clientSettings";
import type { DesktopNotificationPermission } from "../../../utils/desktopNotifications";
import { Row, Toggle } from "../SettingsPage.components";
import { GeneralSection } from "./GeneralSection";

type NotificationsSettingsSectionProps = {
  isDesktopApp: () => boolean;
  completedNotifications: boolean;
  failedNotifications: boolean;
  badgeUnreadCount: boolean;
  desktopNotificationPermission: DesktopNotificationPermission;
  desktopNotificationPermissionBusy: boolean;
  clientSettingsState: ClientSettingsState;
  clientSettingsSaving: boolean;
  clientSettingsError: string | null;
  onToggleCompletedNotifications: (next: boolean) => Promise<void>;
  onToggleFailedNotifications: (next: boolean) => Promise<void>;
  onToggleBadgeUnreadCount: (next: boolean) => Promise<void>;
  onRequestDesktopNotificationPermission: () => Promise<void>;
};

const permissionLabel = (permission: DesktopNotificationPermission): string => {
  switch (permission) {
    case "granted":
      return "Enabled";
    case "denied":
      return "Denied";
    case "default":
      return "Not requested";
    default:
      return "Unavailable";
  }
};

const permissionDescription = (permission: DesktopNotificationPermission): string => {
  switch (permission) {
    case "granted":
      return "System notifications are enabled for ctx.";
    case "denied":
      return "System notifications are disabled. Re-enable them in System Settings if the OS does not re-prompt.";
    case "default":
      return "ctx will ask the OS the first time a desktop notification matters.";
    default:
      return "Desktop notifications are only available in the desktop app.";
  }
};

export function NotificationsSettingsSection({
  isDesktopApp,
  completedNotifications,
  failedNotifications,
  badgeUnreadCount,
  desktopNotificationPermission,
  desktopNotificationPermissionBusy,
  clientSettingsState,
  clientSettingsSaving,
  clientSettingsError,
  onToggleCompletedNotifications,
  onToggleFailedNotifications,
  onToggleBadgeUnreadCount,
  onRequestDesktopNotificationPermission,
}: NotificationsSettingsSectionProps) {
  const desktop = isDesktopApp();
  const togglesDisabled = !desktop || !clientSettingsState.loaded || clientSettingsSaving;
  const showPermissionButton =
    desktop && (desktopNotificationPermission === "default" || desktopNotificationPermission === "denied");

  return (
    <GeneralSection>
      <div className="settings-preferences-flat">
        <div className="settings-preferences-group">
          <Row
            title="Turn completed"
            description={
              desktop
                ? "Send a system notification when a primary turn completes and ctx is in the background."
                : "Available in the desktop app."
            }
            control={
              <Toggle
                checked={completedNotifications}
                disabled={togglesDisabled}
                onChange={(next) => {
                  void onToggleCompletedNotifications(next);
                }}
                ariaLabel="Turn completed notifications"
              />
            }
          />
          <Row
            title="Turn failed"
            description={
              desktop
                ? "Send a system notification when a primary turn fails and ctx is in the background."
                : "Available in the desktop app."
            }
            control={
              <Toggle
                checked={failedNotifications}
                disabled={togglesDisabled}
                onChange={(next) => {
                  void onToggleFailedNotifications(next);
                }}
                ariaLabel="Turn failed notifications"
              />
            }
          />
          <Row
            title="Unread badge"
            description={
              desktop
                ? "Show the unread main-thread count for open workspaces in the app icon."
                : "Available in the desktop app."
            }
            control={
              <Toggle
                checked={badgeUnreadCount}
                disabled={togglesDisabled}
                onChange={(next) => {
                  void onToggleBadgeUnreadCount(next);
                }}
                ariaLabel="Unread badge count"
              />
            }
          />
          <Row
            title={`Permission: ${permissionLabel(desktopNotificationPermission)}`}
            description={permissionDescription(desktopNotificationPermission)}
            control={
              showPermissionButton ? (
                <button
                  type="button"
                  className="settings-btn settings-btn-secondary"
                  disabled={desktopNotificationPermissionBusy}
                  onClick={() => {
                    void onRequestDesktopNotificationPermission();
                  }}
                >
                  {desktopNotificationPermissionBusy ? "Requesting..." : "Request Permission"}
                </button>
              ) : (
                <span className="settings-row-desc">{permissionLabel(desktopNotificationPermission)}</span>
              )
            }
          />
        </div>
      </div>
      {clientSettingsError ? <div className="settings-banner settings-banner-error">{clientSettingsError}</div> : null}
    </GeneralSection>
  );
}
