import { describe, expect, it, vi } from "vitest";
import { runBillingCheckoutFlow } from "./billingCheckoutFlow";

describe("runBillingCheckoutFlow", () => {
  it("fails explicitly in the public export without invoking hosted checkout", async () => {
    const invokeCheckout = vi.fn();
    const trackSubscribeCtaClicked = vi.fn();
    const trackCheckoutStarted = vi.fn();

    await expect(
      runBillingCheckoutFlow({
        interval: "month",
        returnPath: "/settings#general",
        invokeCheckout,
        trackSubscribeCtaClicked,
        trackCheckoutStarted,
      }),
    ).rejects.toThrow("Billing and paid-plan management are not included in the public ADE export.");

    expect(invokeCheckout).not.toHaveBeenCalled();
    expect(trackSubscribeCtaClicked).not.toHaveBeenCalled();
    expect(trackCheckoutStarted).not.toHaveBeenCalled();
  });
});
