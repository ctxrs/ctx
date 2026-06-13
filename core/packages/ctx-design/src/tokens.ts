export type ContextTokens = {
  colors: {
    background: string;
    surface: string;
    surfaceAlt: string;
    border: string;
    text: string;
    muted: string;
    accent: string;
    success: string;
    warning: string;
    danger: string;
  };
  spacing: {
    xs: number;
    sm: number;
    md: number;
    lg: number;
    xl: number;
  };
  radii: {
    sm: number;
    md: number;
    lg: number;
    pill: number;
  };
  typography: {
    fontFamily: string;
    monoFamily: string;
    sizes: {
      xs: number;
      sm: number;
      md: number;
      lg: number;
      xl: number;
    };
  };
};

export const tokens: ContextTokens = {
  colors: {
    background: "#0b0d17",
    surface: "#111629",
    surfaceAlt: "#161b2b",
    border: "#28304a",
    text: "#f5f7ff",
    muted: "#9aa3c2",
    accent: "#4c8bff",
    success: "#72f0b4",
    warning: "#f6c760",
    danger: "#ff8a80",
  },
  spacing: {
    xs: 4,
    sm: 8,
    md: 12,
    lg: 16,
    xl: 24,
  },
  radii: {
    sm: 6,
    md: 10,
    lg: 14,
    pill: 999,
  },
  typography: {
    fontFamily: "Inter, system-ui, -apple-system, BlinkMacSystemFont, \"Segoe UI\", sans-serif",
    monoFamily: "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, \"Liberation Mono\", monospace",
    sizes: {
      xs: 12,
      sm: 14,
      md: 16,
      lg: 20,
      xl: 24,
    },
  },
};

export const tokenPairs = (): [string, string][] => [
  ["color-background", tokens.colors.background],
  ["color-surface", tokens.colors.surface],
  ["color-surface-alt", tokens.colors.surfaceAlt],
  ["color-border", tokens.colors.border],
  ["color-text", tokens.colors.text],
  ["color-muted", tokens.colors.muted],
  ["color-accent", tokens.colors.accent],
  ["color-success", tokens.colors.success],
  ["color-warning", tokens.colors.warning],
  ["color-danger", tokens.colors.danger],
  ["font-family", tokens.typography.fontFamily],
  ["font-mono", tokens.typography.monoFamily],
];
