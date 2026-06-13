import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";

export function useWorkbenchConversationMenu() {
  const [convoMenu, setConvoMenu] = useState<{ style: CSSProperties } | null>(null);
  const convoMenuRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const onPointerDown = (event: PointerEvent) => {
      if (!convoMenu) return;
      const element = event.target as HTMLElement | null;
      if (element && (element.closest(".wb-convo-menu") || element.closest(".wb-convo-menu-trigger"))) return;
      setConvoMenu(null);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setConvoMenu(null);
    };
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [convoMenu]);

  const openConvoMenu = useCallback((triggerEl: HTMLElement) => {
    const rect = triggerEl.getBoundingClientRect();
    const left = Math.min(rect.left, window.innerWidth - 240);
    const top = Math.min(rect.bottom + 6, window.innerHeight - 220);
    setConvoMenu((prev) => (prev ? null : { style: { left, top } }));
  }, []);

  const closeConvoMenu = useCallback(() => {
    setConvoMenu(null);
  }, []);

  return {
    convoMenu,
    convoMenuRef,
    openConvoMenu,
    closeConvoMenu,
  };
}
