import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import type { TerminalSession } from "@ctx/types";

export type TerminalPanelContextMenuState = {
  terminalId: string;
  x: number;
  y: number;
};

export function TerminalPanelContextMenu({
  contextMenu,
  contextTerminal,
  canUnsplit,
  onClose,
  onRename,
  onSplit,
  onUnsplit,
  onKill,
}: {
  contextMenu: TerminalPanelContextMenuState;
  contextTerminal: TerminalSession | null;
  canUnsplit: boolean;
  onClose: () => void;
  onRename: (terminalId: string) => void;
  onSplit: (terminalId: string | null) => void;
  onUnsplit: (terminalId: string | null) => void;
  onKill: (terminalId: string | null) => void;
}) {
  const contextMenuRef = useRef<HTMLDivElement | null>(null);
  const [contextMenuStyle, setContextMenuStyle] = useState<CSSProperties | null>(null);

  useEffect(() => {
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as HTMLElement | null;
      if (target && contextMenuRef.current?.contains(target)) return;
      onClose();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [onClose]);

  useLayoutEffect(() => {
    const menu = contextMenuRef.current;
    if (!menu) return;
    const rect = menu.getBoundingClientRect();
    const margin = 8;
    let left = contextMenu.x;
    let top = contextMenu.y;
    if (left + rect.width > window.innerWidth - margin) {
      left = window.innerWidth - margin - rect.width;
    }
    if (top + rect.height > window.innerHeight - margin) {
      top = window.innerHeight - margin - rect.height;
    }
    left = Math.max(margin, left);
    top = Math.max(margin, top);
    setContextMenuStyle({ left, top, position: "fixed" });
  }, [contextMenu]);

  return (
    <div
      ref={contextMenuRef}
      className="wb-menu wb-terminal-menu"
      role="menu"
      style={contextMenuStyle ?? { left: contextMenu.x, top: contextMenu.y, position: "fixed" }}
    >
      <button
        type="button"
        className="wb-menu-item"
        onClick={() => {
          onClose();
          if (contextTerminal) onRename(contextMenu.terminalId);
        }}
        disabled={!contextTerminal}
        role="menuitem"
      >
        Rename
      </button>
      <button
        type="button"
        className="wb-menu-item"
        onClick={() => {
          onClose();
          onSplit(contextTerminal ? contextMenu.terminalId : null);
        }}
        disabled={!contextTerminal}
        role="menuitem"
      >
        Split
      </button>
      <button
        type="button"
        className="wb-menu-item"
        onClick={() => {
          onClose();
          onUnsplit(contextTerminal ? contextMenu.terminalId : null);
        }}
        disabled={!contextTerminal || !canUnsplit}
        role="menuitem"
      >
        Unsplit
      </button>
      <button
        type="button"
        className="wb-menu-item wb-menu-item-danger"
        onClick={() => {
          onClose();
          onKill(contextTerminal ? contextMenu.terminalId : null);
        }}
        disabled={!contextTerminal}
        role="menuitem"
      >
        Kill terminal
      </button>
    </div>
  );
}
