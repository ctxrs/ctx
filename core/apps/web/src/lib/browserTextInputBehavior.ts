// Keep browser autofill/correction from fighting prompt, search, and rename fields.
export const DISABLE_BROWSER_TEXT_ASSISTS = {
  autoComplete: "off",
  autoCorrect: "off",
  autoCapitalize: "none",
  spellCheck: false,
} as const;
