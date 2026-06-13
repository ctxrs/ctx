export type PlanType = "free_local" | "pro" | "team" | "enterprise";

export const shouldTrackEntitlementActivated = (
  priorPlan: PlanType | null,
  nextPlan: PlanType | null,
): boolean => priorPlan === "free_local" && nextPlan !== null && nextPlan !== "free_local";
