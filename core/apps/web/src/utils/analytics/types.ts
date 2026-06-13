export type AnalyticsScalar = string | number | boolean;

export type AnalyticsProperties = Record<string, AnalyticsScalar>;

export type AnalyticsSurface = "web" | "desktop" | "mobile_shell";

export type AnalyticsSessionRootKind = "workspace_root" | "worktree";

export type AnalyticsSessionLocation = "local" | "remote";

export type AnalyticsSessionKind = "primary" | "subagent";
