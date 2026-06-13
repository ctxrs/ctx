import { tokens, tokenPairs } from "./tokens";

export { tokens };

export const cssVariablePrefix = "--context";

export const applyContextTheme = (target: HTMLElement = document.documentElement): void => {
  target.style.setProperty(`${cssVariablePrefix}-font-family`, tokens.typography.fontFamily);
  target.style.setProperty(`${cssVariablePrefix}-font-mono`, tokens.typography.monoFamily);
  for (const [key, value] of tokenPairs()) {
    target.style.setProperty(`${cssVariablePrefix}-${key}`, value);
  }
};
