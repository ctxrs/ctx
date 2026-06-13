import { WebLinksAddon } from "@xterm/addon-web-links";
import { type ILink, type ILinkProvider, Terminal } from "@xterm/xterm";
import { openExternalLink } from "../utils/desktop";

export function installWebLinksAddon(term: Terminal) {
  const isModifierPressed = (event: { metaKey?: boolean; ctrlKey?: boolean }) =>
    !!event.metaKey || !!event.ctrlKey;
  let modifierPressed = false;
  let hoveredLink: ILink | null = null;

  const setDecorations = (link: ILink, active: boolean) => {
    if (!link.decorations) {
      link.decorations = { underline: active, pointerCursor: active };
      return;
    }
    link.decorations.underline = active;
    link.decorations.pointerCursor = active;
  };

  const updateHoveredLink = () => {
    if (!hoveredLink) return;
    setDecorations(hoveredLink, modifierPressed);
  };

  const setModifierPressed = (next: boolean) => {
    if (modifierPressed === next) return;
    modifierPressed = next;
    updateHoveredLink();
  };

  const handleKeyEvent = (event: KeyboardEvent) => {
    if (event.key !== "Meta" && event.key !== "Control") return;
    setModifierPressed(event.type === "keydown");
  };

  const handleWindowBlur = () => {
    setModifierPressed(false);
  };

  window.addEventListener("keydown", handleKeyEvent);
  window.addEventListener("keyup", handleKeyEvent);
  window.addEventListener("blur", handleWindowBlur);

  const openLink = (event: MouseEvent, uri: string) => {
    if (!isModifierPressed(event) && !modifierPressed) return;
    void openExternalLink(uri);
  };

  const wrapLink = (link: ILink) => {
    setDecorations(link, false);
    const originalHover = link.hover;
    const originalLeave = link.leave;
    link.hover = (event, text) => {
      hoveredLink = link;
      queueMicrotask(() => setDecorations(link, isModifierPressed(event) || modifierPressed));
      originalHover?.(event, text);
    };
    link.leave = (event, text) => {
      if (hoveredLink === link) {
        hoveredLink = null;
      }
      setDecorations(link, false);
      originalLeave?.(event, text);
    };
  };

  const originalRegisterLinkProvider = term.registerLinkProvider;
  term.registerLinkProvider = (provider: ILinkProvider) =>
    originalRegisterLinkProvider.call(term, {
      provideLinks: (bufferLineNumber, callback) => {
        provider.provideLinks(bufferLineNumber, (links) => {
          if (links) {
            for (const link of links) {
              wrapLink(link);
            }
          }
          callback(links);
        });
      },
    });

  try {
    term.loadAddon(new WebLinksAddon(openLink));
  } finally {
    term.registerLinkProvider = originalRegisterLinkProvider;
  }

  return () => {
    window.removeEventListener("keydown", handleKeyEvent);
    window.removeEventListener("keyup", handleKeyEvent);
    window.removeEventListener("blur", handleWindowBlur);
  };
}
