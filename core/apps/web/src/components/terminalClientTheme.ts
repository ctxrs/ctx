import { readCssVar, withAlpha, type ThemeVariant } from "../utils/theme";

export function terminalTheme(themeVariant: ThemeVariant) {
  const read = (name: string, fallback: string) => readCssVar(name, fallback);
  const accentFallback = themeVariant === "dark" ? "#3794ff" : "#005fb8";
  const panelFallback = themeVariant === "dark" ? "#252526" : "#f8f8f8";
  const textFallback = themeVariant === "dark" ? "#d4d4d4" : "#3b3b3b";
  const accent = read("--accent", accentFallback);
  const selectionFallback =
    themeVariant === "dark" ? "rgba(255, 255, 255, 0.2)" : "rgba(0, 0, 0, 0.12)";
  const selectionAlpha = themeVariant === "dark" ? 0.2 : 0.15;
  const ansiFallbacks =
    themeVariant === "dark"
      ? {
          black: "#000000",
          red: "#cd3131",
          green: "#0dbc79",
          yellow: "#e5e510",
          blue: "#2472c8",
          magenta: "#bc3fbc",
          cyan: "#11a8cd",
          white: "#e5e5e5",
          brightBlack: "#666666",
          brightRed: "#f14c4c",
          brightGreen: "#23d18b",
          brightYellow: "#f5f543",
          brightBlue: "#3b8eea",
          brightMagenta: "#d670d6",
          brightCyan: "#29b8db",
          brightWhite: "#e5e5e5",
        }
      : {
          black: "#000000",
          red: "#cd3131",
          green: "#107c10",
          yellow: "#949800",
          blue: "#0451a5",
          magenta: "#bc05bc",
          cyan: "#0598bc",
          white: "#555555",
          brightBlack: "#666666",
          brightRed: "#cd3131",
          brightGreen: "#14ce14",
          brightYellow: "#b5ba00",
          brightBlue: "#0451a5",
          brightMagenta: "#bc05bc",
          brightCyan: "#0598bc",
          brightWhite: "#a5a5a5",
        };
  const background = read("--terminal-bg", read("--panel", panelFallback));
  const foreground = read("--terminal-fg", read("--text", textFallback));
  const cursor = read("--terminal-cursor", foreground);
  const cursorAccent = read("--terminal-cursor-accent", background);
  const selectionBackground = read(
    "--terminal-selection-bg",
    withAlpha(accent, selectionAlpha, selectionFallback),
  );
  const selectionInactiveBackground = read(
    "--terminal-selection-inactive-bg",
    withAlpha(selectionBackground, 0.5, selectionBackground),
  );
  return {
    background,
    foreground,
    cursor,
    cursorAccent,
    selectionBackground,
    selectionInactiveBackground,
    black: read("--terminal-ansi-black", ansiFallbacks.black),
    red: read("--terminal-ansi-red", ansiFallbacks.red),
    green: read("--terminal-ansi-green", ansiFallbacks.green),
    yellow: read("--terminal-ansi-yellow", ansiFallbacks.yellow),
    blue: read("--terminal-ansi-blue", ansiFallbacks.blue),
    magenta: read("--terminal-ansi-magenta", ansiFallbacks.magenta),
    cyan: read("--terminal-ansi-cyan", ansiFallbacks.cyan),
    white: read("--terminal-ansi-white", ansiFallbacks.white),
    brightBlack: read("--terminal-ansi-bright-black", ansiFallbacks.brightBlack),
    brightRed: read("--terminal-ansi-bright-red", ansiFallbacks.brightRed),
    brightGreen: read("--terminal-ansi-bright-green", ansiFallbacks.brightGreen),
    brightYellow: read("--terminal-ansi-bright-yellow", ansiFallbacks.brightYellow),
    brightBlue: read("--terminal-ansi-bright-blue", ansiFallbacks.brightBlue),
    brightMagenta: read("--terminal-ansi-bright-magenta", ansiFallbacks.brightMagenta),
    brightCyan: read("--terminal-ansi-bright-cyan", ansiFallbacks.brightCyan),
    brightWhite: read("--terminal-ansi-bright-white", ansiFallbacks.brightWhite),
  };
}

export function terminalFontFamily() {
  return readCssVar("--mono", "monospace");
}
