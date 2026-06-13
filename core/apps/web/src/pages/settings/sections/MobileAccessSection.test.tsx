import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { MobileAccessSection } from "./MobileAccessSection";

describe("MobileAccessSection", () => {
  it("renders the public-export unavailable state", () => {
    render(
      <MobileAccessSection
        hostedServicesConfigured={false}
        billingUser={null}
        entitlementsBusy={false}
        proEnabled={false}
        mobileStatus={null}
        mobileStatusBusy={false}
        mobileStatusError={null}
        mobileEnableBusy={false}
        mobileEnableError={null}
        mobileQr={null}
        qrFgColor="#000"
        onEnable={vi.fn()}
        onDisable={vi.fn()}
      />,
    );

    expect(screen.getByText(/Managed mobile access is not included in the public ADE export/i)).toBeInTheDocument();
  });
});
