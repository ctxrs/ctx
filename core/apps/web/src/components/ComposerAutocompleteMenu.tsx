import { useLayoutEffect, useRef, useState } from "react";
import type React from "react";
import { createPortal } from "react-dom";
import { File as FileLucideIcon, Folder } from "lucide-react";
import { FileIcon } from "./FileIcon";

export type ComposerAutocompleteItem =
  | {
      key: string;
      kind: "slash";
      label: string;
      insertText: string;
      description?: string;
    }
  | {
      key: string;
      kind: "file";
      path: string;
      label: string;
      insertText: string;
      description?: string;
    };

function clamp(n: number, min: number, max: number) {
  return Math.max(min, Math.min(max, n));
}

function splitPath(path: string): { fileName: string; dirName: string } {
  const normalized = String(path).replace(/\\/g, "/");
  const parts = normalized.split("/").filter(Boolean);
  const fileName = parts.length > 0 ? parts[parts.length - 1] : normalized;
  const dirName = parts.length > 1 ? parts.slice(0, -1).join("/") : "";
  return { fileName, dirName };
}

export function ComposerAutocompleteMenu({
  open,
  loading,
  items,
  activeIndex,
  onPick,
  onHoverIndex,
  anchorRect,
  anchorInputRect,
  inlineFallback,
}: {
  open: boolean;
  loading: boolean;
  items: ComposerAutocompleteItem[];
  activeIndex: number;
  onPick: (index: number) => void;
  onHoverIndex: (index: number) => void;
  anchorRect: DOMRect | null;
  anchorInputRect?: DOMRect | null;
  inlineFallback: boolean;
}) {
  const itemRefs = useRef<Array<HTMLDivElement | null>>([]);
  const [previewStyle, setPreviewStyle] = useState<React.CSSProperties | null>(null);

  const anchorForWidth = anchorInputRect ?? anchorRect;
  const anchorForPosition = anchorRect ?? anchorInputRect;
  const canPortal = Boolean(anchorForPosition) && Boolean(anchorForWidth) && !inlineFallback;

  const offset = 6;
  const margin = 10;
  const viewportH = window.innerHeight;
  const viewportW = window.innerWidth;

  const popoverWidth = canPortal
    ? clamp(anchorForWidth!.width || 0, 340, 520)
    : 320;

  const spaceBelow = canPortal ? viewportH - (anchorForPosition!.bottom + offset) - margin : 240;
  const spaceAbove = canPortal ? anchorForPosition!.top - offset - margin : 240;
  const rowH = 30;
  const containerExtraY = 14; // padding (6*2) + border (1*2)
  const estimatedRows = items.length > 0 ? items.length : loading ? 0 : 1;
  const estimatedHeight = estimatedRows * rowH + containerExtraY;
  const openAbove = canPortal ? spaceBelow < estimatedHeight && spaceAbove > spaceBelow : false;
  const minHeight = rowH + containerExtraY;
  const maxHeight = clamp(
    Math.min(openAbove ? spaceAbove : spaceBelow, estimatedHeight || 360),
    minHeight,
    360,
  );

  const active = items[activeIndex] ?? null;
  const activeFilePath = active?.kind === "file" ? active.path : null;

  const popoverLeft = clamp(anchorForWidth?.left ?? 0, margin, viewportW - popoverWidth - margin);
  const popoverTop = openAbove ? null : (anchorForPosition?.bottom ?? 0) + offset;
  const popoverBottom = openAbove ? viewportH - (anchorForPosition?.top ?? 0) + offset : null;

  useLayoutEffect(() => {
    if (!open || !canPortal || !activeFilePath) {
      setPreviewStyle(null);
      return;
    }
    const el = itemRefs.current[activeIndex] ?? null;
    if (!el) {
      setPreviewStyle(null);
      return;
    }
    const r = el.getBoundingClientRect();
    const previewWidth = 240;
    const gap = 10;
    const maxPreviewHeight = 285;

    let left = popoverLeft + popoverWidth + gap;
    if (left + previewWidth + margin > viewportW) {
      left = popoverLeft - previewWidth - gap;
    }
    left = clamp(left, margin, viewportW - previewWidth - margin);
    const top = clamp(r.top - 6, margin, viewportH - maxPreviewHeight - margin);
    setPreviewStyle({
      position: "fixed",
      left,
      top,
      width: `${previewWidth}px`,
      maxHeight: `${maxPreviewHeight}px`,
    });
  }, [activeFilePath, activeIndex, canPortal, open, popoverLeft, popoverWidth, viewportH, viewportW]);

  itemRefs.current.length = items.length;

  if (!open) return null;

  // Don't show container during initial loading state (loading + no items)
  const hasContent = items.length > 0 || (!loading && items.length === 0);
  if (!hasContent) return null;

  const body = (
    <div className="composer-ac" style={{ maxHeight }} role="listbox" aria-label="Completions">
      {!loading && items.length === 0 && <div className="composer-ac-empty">No matches</div>}
      {items.map((it, idx) => {
        const active = idx === activeIndex;
        const file = it.kind === "file" ? splitPath(it.path) : null;
        const left = it.kind === "file" ? file?.fileName ?? it.label : it.label;
        const right =
          it.kind === "file"
            ? file?.dirName
              ? `…/${file.dirName}`
              : ""
            : it.description
              ? it.description
              : "";

        return (
          <div
            key={it.key}
            ref={(el) => {
              itemRefs.current[idx] = el;
            }}
            className={`composer-ac-item ${active ? "composer-ac-item-active" : ""}`}
            onMouseEnter={() => onHoverIndex(idx)}
            onMouseDown={(e) => {
              e.preventDefault();
              onPick(idx);
            }}
            role="option"
            aria-selected={active}
          >
            {it.kind === "file" ? (
              <span className="composer-ac-icon" aria-hidden="true">
                <FileIcon path={it.path} size={16} />
              </span>
            ) : null}
            <span className="composer-ac-item-text" title={it.kind === "file" ? `${left} ${right}`.trim() : left}>
              <span className="composer-ac-item-left">{left}</span>
              {right && <span className="composer-ac-item-right"> {right}</span>}
            </span>
          </div>
        );
      })}
    </div>
  );

  if (!anchorForPosition || !anchorForWidth || inlineFallback) {
    return <div className="composer-ac-inline">{body}</div>;
  }

  const preview = activeFilePath ? (
    <div className="composer-ac-preview-popover" style={previewStyle ?? undefined} aria-hidden="true">
      {(() => {
        const normalized = activeFilePath.replace(/\\/g, "/");
        const parts = normalized.split("/").filter(Boolean);
        const dirs = parts.slice(0, -1);
        const file = parts[parts.length - 1] ?? normalized;
        const maxDirs = 6;
        const trimmed = dirs.length > maxDirs ? dirs.slice(dirs.length - maxDirs) : dirs;
        const hasMore = dirs.length > trimmed.length;
        return (
          <div className="composer-ac-preview-tree">
            {hasMore && <div className="composer-ac-preview-more">…</div>}
            {trimmed.map((seg, i) => (
              <div
                key={`${seg}:${i}`}
                className="composer-ac-preview-row"
                style={{ paddingLeft: `${i * 14}px` }}
              >
                <span className="composer-ac-preview-icon" aria-hidden="true">
                  <Folder size={14} />
                </span>
                <span className="composer-ac-preview-name">{seg}</span>
              </div>
            ))}
            <div
              className="composer-ac-preview-row composer-ac-preview-row-file"
              style={{ paddingLeft: `${trimmed.length * 14}px` }}
            >
              <span className="composer-ac-preview-icon" aria-hidden="true">
                <FileLucideIcon size={14} />
              </span>
              <span className="composer-ac-preview-name">{file}</span>
            </div>
          </div>
        );
      })()}
    </div>
  ) : null;

  return (
    <>
      {createPortal(
        <div
          className="composer-ac-popover"
          style={{
            left: popoverLeft,
            width: popoverWidth,
            ...(popoverTop !== null ? { top: popoverTop } : {}),
            ...(popoverBottom !== null ? { bottom: popoverBottom } : {}),
          }}
        >
          {body}
        </div>,
        document.body,
      )}
      {previewStyle && preview ? createPortal(preview, document.body) : null}
    </>
  );
}
