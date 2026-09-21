export type ThemeMode = 'dark' | 'light';

const storageKey = 'ctx-docs-theme';

export function getPreferredTheme(): ThemeMode {
  if (typeof document === 'undefined') {
    return 'dark';
  }

  return document.documentElement.dataset.theme === 'light' ? 'light' : 'dark';
}

export function applyTheme(theme: ThemeMode): void {
  if (typeof document === 'undefined') {
    return;
  }

  document.documentElement.dataset.theme = theme;

  try {
    window.localStorage.setItem(storageKey, theme);
  } catch {
    // Ignore storage failures in restricted contexts.
  }
}
