import { desktopListenForDragDrop, type DesktopDragDropEvent } from "./desktop";

export type DropScope = {
  element: HTMLElement;
  accepts?: (dt: DataTransfer | null) => boolean;
  onDragOver?: (dt: DataTransfer | null, ev?: DragEvent | null) => void;
  onDragLeave?: () => void;
  onDrop?: (dt: DataTransfer | null, ev?: DragEvent | null) => void;
  onDropPaths?: (paths: string[], position: { x: number; y: number }) => void;
};

const scopes = new Map<HTMLElement, DropScope>();
let listenersInstalled = false;
let nativeDragDropInstalled = false;
const DROP_SCOPE_READY_PROP = "__ctxDropScopeReady";

type DropScopeReadyElement = HTMLElement & {
  [DROP_SCOPE_READY_PROP]?: boolean;
};

function dataTransferTypes(dt: DataTransfer | null): string[] {
  if (!dt) return [];
  const types = dt.types;
  if (!types) return [];
  if (Array.isArray(types)) return types.map(String);
  try {
    return Array.from(types as ArrayLike<string>).map(String);
  } catch {
    return [];
  }
}

function hasFileLikeItem(dt: DataTransfer | null): boolean {
  if (!dt) return false;
  if (dt.files && dt.files.length > 0) return true;
  const items = dt.items;
  if (items && items.length > 0) {
    for (const item of Array.from(items)) {
      if (item.kind === "file") return true;
    }
  }
  const types = dataTransferTypes(dt);
  return (
    types.includes("Files") ||
    types.includes("application/x-moz-file") ||
    // Safari / WebKit variants:
    types.includes("public.file-url") ||
    types.includes("public.url")
  );
}

function hasImageUrlLike(dt: DataTransfer | null): boolean {
  const types = dataTransferTypes(dt);
  return types.includes("text/uri-list") || types.includes("text/html");
}

function defaultAccepts(dt: DataTransfer | null): boolean {
  return hasFileLikeItem(dt) || hasImageUrlLike(dt);
}

function scopeAtPoint(x: number, y: number, fallbackTarget?: EventTarget | null): DropScope | null {
  const pointEl =
    Number.isFinite(x) && Number.isFinite(y)
      ? (document.elementFromPoint(x, y) as Element | null)
      : fallbackTarget instanceof Element
        ? fallbackTarget
        : null;
  if (!pointEl) return null;

  let el: Element | null = pointEl;
  while (el) {
    if (el instanceof HTMLElement) {
      const s = scopes.get(el);
      if (s) return s;
    }
    el = el.parentElement;
  }
  return null;
}

function scopeForEvent(ev: DragEvent): DropScope | null {
  const x = typeof ev.clientX === "number" ? ev.clientX : Number.NaN;
  const y = typeof ev.clientY === "number" ? ev.clientY : Number.NaN;
  return scopeAtPoint(x, y, ev.target);
}

function setNativeHoverScope(activeScope: DropScope | null) {
  for (const scope of scopes.values()) {
    if (scope === activeScope) {
      scope.onDragOver?.(null, null);
      continue;
    }
    scope.onDragLeave?.();
  }
}

function nativeScopeForPosition(position: { x: number; y: number }): DropScope | null {
  const directScope = scopeAtPoint(position.x, position.y);
  if (directScope) return directScope;
  const ratio = window.devicePixelRatio > 0 ? window.devicePixelRatio : 1;
  if (ratio === 1) return null;
  return scopeAtPoint(position.x / ratio, position.y / ratio);
}

function handleNativeDragDrop(event: DesktopDragDropEvent) {
  (
    globalThis as typeof globalThis & {
      __ctxNativeDropEvents?: Array<{
        type: DesktopDragDropEvent["type"];
        hasScope: boolean;
        hasOnDropPaths: boolean;
        pathCount: number;
      }>;
    }
  ).__ctxNativeDropEvents ??= [];
  if (event.type === "leave") {
    (
      globalThis as typeof globalThis & {
        __ctxNativeDropEvents?: Array<{
          type: DesktopDragDropEvent["type"];
          hasScope: boolean;
          hasOnDropPaths: boolean;
          pathCount: number;
        }>;
      }
    ).__ctxNativeDropEvents?.push({ type: event.type, hasScope: false, hasOnDropPaths: false, pathCount: 0 });
    setNativeHoverScope(null);
    return;
  }
  const scope = nativeScopeForPosition(event.position);
  (
    globalThis as typeof globalThis & {
        __ctxNativeDropEvents?: Array<{
          type: DesktopDragDropEvent["type"];
          hasScope: boolean;
          hasOnDropPaths: boolean;
          pathCount: number;
        }>;
      }
    ).__ctxNativeDropEvents?.push({
      type: event.type,
      hasScope: Boolean(scope),
      hasOnDropPaths: typeof scope?.onDropPaths === "function",
      pathCount: "paths" in event ? event.paths.length : 0,
    });
  if (event.type === "enter" || event.type === "over") {
    setNativeHoverScope(scope);
    return;
  }
  setNativeHoverScope(null);
  if (event.type === "drop" && scope && event.paths.length > 0) {
    (
      globalThis as typeof globalThis & {
        __ctxNativeDropDelivered?: { pathCount: number; position: { x: number; y: number } };
      }
    ).__ctxNativeDropDelivered = {
      pathCount: event.paths.length,
      position: event.position,
    };
    scope.onDropPaths?.(event.paths, event.position);
  }
}

function ensureListeners() {
  if (listenersInstalled) return;
  listenersInstalled = true;

  const onDragOver = (ev: DragEvent) => {
    const scope = scopeForEvent(ev);
    if (!scope) return;
    ev.preventDefault();
    try {
      if (ev.dataTransfer) ev.dataTransfer.dropEffect = "copy";
    } catch {}
    const accepts = (scope.accepts ?? defaultAccepts)(ev.dataTransfer);
    if (accepts) scope.onDragOver?.(ev.dataTransfer, ev);
  };

  const onDrop = (ev: DragEvent) => {
    const scope = scopeForEvent(ev);
    if (!scope) return;
    ev.preventDefault();
    ev.stopPropagation();
    const accepts = (scope.accepts ?? defaultAccepts)(ev.dataTransfer);
    if (accepts) scope.onDrop?.(ev.dataTransfer, ev);
  };

  window.addEventListener("dragover", onDragOver, true);
  window.addEventListener("drop", onDrop, true);

  if (!nativeDragDropInstalled) {
    nativeDragDropInstalled = true;
    void desktopListenForDragDrop((event) => {
      handleNativeDragDrop(event);
    });
  }
}

export function registerDropScope(scope: DropScope): () => void {
  ensureListeners();
  scopes.set(scope.element, scope);
  (scope.element as DropScopeReadyElement)[DROP_SCOPE_READY_PROP] = true;
  return () => {
    scopes.delete(scope.element);
    delete (scope.element as DropScopeReadyElement)[DROP_SCOPE_READY_PROP];
  };
}
