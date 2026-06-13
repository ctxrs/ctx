import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type React from "react";
import { createPortal } from "react-dom";
import { Info } from "lucide-react";
import { clamp } from "./WorkbenchComposer.utils";

export function MenuInfoTooltip({
  title,
  description,
  tooltipId,
}: {
  title: string;
  description: string;
  tooltipId: string;
}) {
  const buttonRef = useRef<HTMLButtonElement | null>(null);
  const tooltipRef = useRef<HTMLDivElement | null>(null);
  const closeTimerRef = useRef<number | null>(null);
  const [open, setOpen] = useState(false);
  const [ready, setReady] = useState(false);
  const [style, setStyle] = useState<React.CSSProperties | null>(null);

  const cancelClose = useCallback(() => {
    if (closeTimerRef.current == null) return;
    window.clearTimeout(closeTimerRef.current);
    closeTimerRef.current = null;
  }, []);

  const requestClose = useCallback(() => {
    cancelClose();
    closeTimerRef.current = window.setTimeout(() => {
      setOpen(false);
    }, 180);
  }, [cancelClose]);

  useLayoutEffect(() => {
    if (!open) {
      setReady(false);
      setStyle(null);
      return;
    }

    setReady(false);
    setStyle({ position: "fixed", left: 0, top: 0, visibility: "hidden" });

    const update = () => {
      const btn = buttonRef.current;
      const tip = tooltipRef.current;
      if (!btn || !tip) return;

      const anchor = btn.getBoundingClientRect();
      const tipRect = tip.getBoundingClientRect();
      const viewportW = window.innerWidth;
      const viewportH = window.innerHeight;
      const margin = 10;
      const gap = 8;

      const maxWidth = Math.max(220, Math.min(440, viewportW - margin * 2));
      const effectiveW = Math.min(tipRect.width, maxWidth);

      const downTop = anchor.bottom + gap;
      const availableDown = viewportH - margin - downTop;
      const availableUp = anchor.top - margin - gap;
      const pickUp = (h: number) => availableDown < h && availableUp > availableDown;
      let shouldOpenUp = pickUp(tipRect.height);
      let maxHeight = Math.min(320, Math.max(0, shouldOpenUp ? availableUp : availableDown));
      let effectiveH = Math.min(tipRect.height, maxHeight);

      const revisedShouldOpenUp = pickUp(effectiveH);
      if (revisedShouldOpenUp !== shouldOpenUp) {
        shouldOpenUp = revisedShouldOpenUp;
        maxHeight = Math.min(320, Math.max(0, shouldOpenUp ? availableUp : availableDown));
        effectiveH = Math.min(tipRect.height, maxHeight);
      }

      const upTop = anchor.top - gap - effectiveH;
      const rawTop = shouldOpenUp ? upTop : downTop;
      const top = clamp(rawTop, margin, viewportH - margin - effectiveH);

      const preferredLeft = anchor.right - effectiveW;
      const left = clamp(preferredLeft, margin, viewportW - margin - effectiveW);

      setStyle({
        position: "fixed",
        left,
        top,
        maxWidth,
        maxHeight,
        overflow: "auto",
        visibility: "visible",
      });
      setReady(true);
    };

    const raf = window.requestAnimationFrame(update);
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    return () => {
      window.cancelAnimationFrame(raf);
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
    };
  }, [open]);

  useEffect(() => {
    return () => {
      if (closeTimerRef.current != null) window.clearTimeout(closeTimerRef.current);
    };
  }, []);

  const tooltip =
    open && typeof document !== "undefined"
      ? createPortal(
          <div
            ref={tooltipRef}
            id={tooltipId}
            className="wb-menu-tooltip"
            role="tooltip"
            data-open={ready ? "true" : "false"}
            style={style ?? undefined}
            onMouseEnter={() => {
              cancelClose();
              setOpen(true);
            }}
            onMouseLeave={requestClose}
          >
            {description}
          </div>,
          document.body,
        )
      : null;

  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        className="wb-menu-info-btn"
        aria-label={`About ${title}`}
        aria-describedby={open ? tooltipId : undefined}
        onMouseEnter={() => {
          cancelClose();
          setOpen(true);
        }}
        onMouseLeave={requestClose}
        onFocus={() => {
          cancelClose();
          setOpen(true);
        }}
        onBlur={requestClose}
      >
        <Info size={14} />
      </button>
      {tooltip}
    </>
  );
}

export function MenuTitleRow({
  title,
  description,
  tooltipId,
}: {
  title: string;
  description: string;
  tooltipId: string;
}) {
  return (
    <div className="wb-menu-title-row">
      <div className="wb-menu-title">{title}</div>
      <div className="wb-menu-info">
        <MenuInfoTooltip title={title} description={description} tooltipId={tooltipId} />
      </div>
    </div>
  );
}
