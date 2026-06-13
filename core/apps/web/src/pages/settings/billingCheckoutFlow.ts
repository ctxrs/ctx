export type BillingInterval = "month" | "year";

export type BillingCheckoutInvokeResult = {
  error: unknown;
  data: unknown;
};

export type BillingCheckoutInvoke = (args: {
  interval: BillingInterval;
  returnPath: string;
  planType?: "pro" | "team";
  organizationId?: string;
}) => Promise<BillingCheckoutInvokeResult>;

type BillingCheckoutFlowOptions = {
  interval: BillingInterval;
  returnPath: string;
  planType?: "pro" | "team";
  organizationId?: string;
  invokeCheckout: BillingCheckoutInvoke;
  trackSubscribeCtaClicked: (interval: BillingInterval) => void;
  trackCheckoutStarted: (interval: BillingInterval) => void;
};

const HOSTED_BILLING_UNAVAILABLE =
  "Billing and paid-plan management are not included in the public ADE export.";

export const runBillingCheckoutFlow = async (
  options: BillingCheckoutFlowOptions,
): Promise<string> => {
  void options;
  throw new Error(HOSTED_BILLING_UNAVAILABLE);
};
