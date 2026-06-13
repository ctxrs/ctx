import type { EnableMobileAccessResponse, MobileAccessStatus } from "../../../api/client";

export function MobileAccessSection(_props: {
  hostedServicesConfigured: boolean;
  billingUser: unknown;
  entitlementsBusy: boolean;
  proEnabled: boolean;
  mobileStatus: MobileAccessStatus | null;
  mobileStatusBusy: boolean;
  mobileStatusError: string | null;
  mobileEnableBusy: boolean;
  mobileEnableError: string | null;
  mobileQr: EnableMobileAccessResponse | null;
  qrFgColor: string;
  onEnable: () => void | Promise<void>;
  onDisable: () => void | Promise<void>;
}) {
  return (
    <div className="settings-empty">
      Managed mobile access is not included in the public ADE export. Use direct daemon access on
      a network you control.
    </div>
  );
}
