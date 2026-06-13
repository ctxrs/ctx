import {
  useCallback,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type MouseEvent,
  type ReactNode,
  type WheelEvent,
} from "react";
import { defaultUrlTransform } from "react-markdown";
import { Check, Copy } from "lucide-react";
import { ExternalLink } from "../../components/ExternalLink";
import { copyTextToClipboard } from "../../utils/clipboard";
import {
  type FileRef,
  isAbsolutePath,
  parseFileRefToken,
  parseUrlToken,
  splitWhitespaceTokens,
} from "../../utils/codeTokenLinks";
import { desktopOpenDeepLink, desktopOpenFile, isDesktopApp, openExternalLink } from "../../utils/desktop";
import { isSealedInlineCodeFragment, splitInlineCodeFragments } from "../../utils/inlineCodeFragments";

type CodeTokenOptions = {
  enableLinks: boolean;
  worktreeId: string | null;
  onFileOpenError?: (message: string | null) => void;
  wrapPlainTokens?: boolean;
};

export type MarkdownRenderOptions = {
  enableLinks: boolean;
  worktreeId: string | null;
  onFileOpenError?: (message: string | null) => void;
};

export function forwardVerticalWheelToTranscript(event: WheelEvent<HTMLElement>) {
  if (Math.abs(event.deltaY) <= Math.abs(event.deltaX)) return;
  const transcriptScroller = event.currentTarget.closest("[data-pretext-virtualizer-list='1'], .wb-thread-scroller") as HTMLElement | null;
  if (!transcriptScroller) return;
  const maxScrollTop = Math.max(0, transcriptScroller.scrollHeight - transcriptScroller.clientHeight);
  if (maxScrollTop <= 0) return;
  const nextScrollTop = Math.max(0, Math.min(maxScrollTop, transcriptScroller.scrollTop + event.deltaY));
  if (Math.abs(nextScrollTop - transcriptScroller.scrollTop) <= 0.5) return;
  transcriptScroller.scrollTop = nextScrollTop;
  transcriptScroller.dispatchEvent(new Event("scroll", { bubbles: true }));
  event.preventDefault();
}

const hasModifier = (event: { metaKey?: boolean; ctrlKey?: boolean }): boolean =>
  Boolean(event.metaKey || event.ctrlKey);

export const joinClassNames = (...values: Array<string | false | null | undefined>): string =>
  values.filter(Boolean).join(" ");

const modifierSubscribers = new Set<() => void>();
let modifierSnapshot = false;
let detachModifierListeners: (() => void) | null = null;

const emitModifierSnapshot = () => {
  for (const notify of modifierSubscribers) notify();
};

const setModifierSnapshot = (next: boolean) => {
  if (modifierSnapshot === next) return;
  modifierSnapshot = next;
  emitModifierSnapshot();
};

const updateModifierSnapshot = (event: { metaKey?: boolean; ctrlKey?: boolean }) => {
  setModifierSnapshot(hasModifier(event));
};

const clearModifierSnapshot = () => {
  setModifierSnapshot(false);
};

const subscribeModifierSnapshot = (notify: () => void) => {
  modifierSubscribers.add(notify);
  if (modifierSubscribers.size === 1 && typeof window !== "undefined") {
    const handleKeyboard = (event: KeyboardEvent) => updateModifierSnapshot(event);
    const handleMouse = (event: globalThis.MouseEvent) => updateModifierSnapshot(event);
    const handleBlur = () => clearModifierSnapshot();

    window.addEventListener("keydown", handleKeyboard);
    window.addEventListener("keyup", handleKeyboard);
    window.addEventListener("mousemove", handleMouse);
    window.addEventListener("blur", handleBlur);

    detachModifierListeners = () => {
      window.removeEventListener("keydown", handleKeyboard);
      window.removeEventListener("keyup", handleKeyboard);
      window.removeEventListener("mousemove", handleMouse);
      window.removeEventListener("blur", handleBlur);
    };
  }

  return () => {
    modifierSubscribers.delete(notify);
    if (modifierSubscribers.size > 0) return;
    detachModifierListeners?.();
    detachModifierListeners = null;
    clearModifierSnapshot();
  };
};

const getModifierSnapshot = () => modifierSnapshot;

function useModifierHoverState<T extends HTMLElement>() {
  const modifierDown = useSyncExternalStore(subscribeModifierSnapshot, getModifierSnapshot, () => false);
  const [hovered, setHovered] = useState(false);

  const syncFromPointer = useCallback((event: MouseEvent<T>) => {
    setHovered(true);
    updateModifierSnapshot(event);
  }, []);

  const clear = useCallback(() => {
    setHovered(false);
  }, []);

  return {
    modifierHoverActive: hovered && modifierDown,
    hoverProps: {
      onMouseEnter: syncFromPointer,
      onMouseMove: syncFromPointer,
      onMouseLeave: clear,
    },
  };
}

function ModifierAwareExternalLink({
  href,
  className,
  children,
  ...rest
}: React.ComponentProps<typeof ExternalLink>) {
  const { modifierHoverActive, hoverProps } = useModifierHoverState<HTMLAnchorElement>();
  return (
    <ExternalLink
      href={href}
      className={joinClassNames(className, modifierHoverActive && "ctx-modifier-hover")}
      {...hoverProps}
      {...rest}
    >
      {children}
    </ExternalLink>
  );
}

function ModifierAwareFileLink({
  href,
  className,
  children,
  ...rest
}: React.AnchorHTMLAttributes<HTMLAnchorElement>) {
  const { modifierHoverActive, hoverProps } = useModifierHoverState<HTMLAnchorElement>();
  return (
    <a
      data-allow-raw-anchor
      href={href}
      className={joinClassNames(className, modifierHoverActive && "ctx-modifier-hover")}
      {...hoverProps}
      {...rest}
    >
      {children}
    </a>
  );
}

function ModifierAwareCodePath({
  className,
  children,
  ...rest
}: React.HTMLAttributes<HTMLSpanElement>) {
  const { modifierHoverActive, hoverProps } = useModifierHoverState<HTMLSpanElement>();
  return (
    <span
      className={joinClassNames(className, modifierHoverActive && "ctx-modifier-hover")}
      {...hoverProps}
      {...rest}
    >
      {children}
    </span>
  );
}

function buildAbsolutePathDeepLink(ref: FileRef): string {
  const url = new URL("ctx://open");
  url.searchParams.set("path", ref.path);
  url.searchParams.set("openWith", "editor");
  if (ref.line && ref.line > 0) url.searchParams.set("line", String(ref.line));
  if (ref.col && ref.col > 0) url.searchParams.set("col", String(ref.col));
  return url.toString();
}

const handleCodeTokenClick = async (
  event: MouseEvent<HTMLElement>,
  ref: FileRef,
  worktreeId: string | null,
  onFileOpenError?: (message: string | null) => void,
) => {
  if (!event.metaKey && !event.ctrlKey) return;
  if (!isDesktopApp()) return;
  if (!worktreeId && !isAbsolutePath(ref.path)) return;
  event.preventDefault();
  event.stopPropagation();

  try {
    if (isAbsolutePath(ref.path)) {
      await desktopOpenDeepLink(buildAbsolutePathDeepLink(ref));
    } else {
      await desktopOpenFile({
        worktree_id: worktreeId ?? "",
        path: ref.path,
        line: ref.line ?? null,
        col: ref.col ?? null,
      });
    }
    onFileOpenError?.(null);
  } catch {
    // Ignore failures to keep interaction silent.
  }
};

const handleUrlTokenClick = (event: MouseEvent<HTMLElement>, href: string) => {
  if (!isDesktopApp()) return;
  if (!event.metaKey && !event.ctrlKey) {
    event.preventDefault();
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  void openExternalLink(href);
};

const buildInlineCodeFragments = (text: string, keyPrefix: string): ReactNode[] => {
  const fragments = splitInlineCodeFragments(text);
  return fragments.flatMap((fragment, fragmentIndex) => [
    <span
      key={`${keyPrefix}-fragment-${fragmentIndex}`}
      className={isSealedInlineCodeFragment(fragment) ? "code-token-fragment code-token-fragment-sealed" : "code-token-fragment"}
    >
      {fragment}
    </span>,
    fragmentIndex < fragments.length - 1 ? <wbr key={`${keyPrefix}-break-${fragmentIndex}`} /> : null,
  ]);
};

const buildCodeTokenNodes = (text: string, opts: CodeTokenOptions, parts = splitWhitespaceTokens(text)): ReactNode[] => {
  return parts.flatMap((part, idx) => {
    if (!part) return null;
    if (part.trim() === "") return part;
    if (opts.enableLinks) {
      const urlRef = parseUrlToken(part);
      if (urlRef) {
        return (
          <ModifierAwareExternalLink
            key={`token-${idx}`}
            className="code-token code-token-url"
            data-allow-raw-anchor
            href={urlRef.url}
            rel="noreferrer noopener"
            target="_blank"
            onClick={(event) => handleUrlTokenClick(event, urlRef.url)}
          >
            {buildInlineCodeFragments(part, `token-${idx}`)}
          </ModifierAwareExternalLink>
        );
      }

      const ref = parseFileRefToken(part);
      if (ref && (opts.worktreeId || isAbsolutePath(ref.path))) {
        return (
          <ModifierAwareCodePath
            key={`token-${idx}`}
            className="code-token code-token-path"
            onClick={(event) => handleCodeTokenClick(event, ref, opts.worktreeId, opts.onFileOpenError)}
          >
            {buildInlineCodeFragments(part, `token-${idx}`)}
          </ModifierAwareCodePath>
        );
      }
    }

    if (!opts.wrapPlainTokens) return part;
    const fragments = splitInlineCodeFragments(part);
    return fragments.flatMap((fragment, fragmentIndex) => [
      <span key={`token-${idx}-fragment-${fragmentIndex}`} className="code-token">
        <span className={isSealedInlineCodeFragment(fragment) ? "code-token-fragment code-token-fragment-sealed" : "code-token-fragment"}>
          {fragment}
        </span>
      </span>,
      fragmentIndex < fragments.length - 1 ? <wbr key={`token-${idx}-break-${fragmentIndex}`} /> : null,
    ]);
  });
};

export function TokenizedInlineCode({
  codeString,
  codeParts,
  className,
  enableLinks,
  worktreeId,
  onFileOpenError,
}: {
  codeString: string;
  codeParts: readonly string[];
  className?: string;
  enableLinks: boolean;
  worktreeId: string | null;
  onFileOpenError?: (message: string | null) => void;
}) {
  const handleDoubleClick = useCallback((event: MouseEvent<HTMLElement>) => {
    if (event.metaKey || event.ctrlKey) return;
    const selection = window.getSelection();
    if (!selection) return;
    const range = document.createRange();
    range.selectNodeContents(event.currentTarget);
    selection.removeAllRanges();
    selection.addRange(range);
  }, []);

  const content = buildCodeTokenNodes(
    codeString,
    {
      enableLinks,
      worktreeId,
      onFileOpenError,
      wrapPlainTokens: true,
    },
    [...codeParts],
  );
  return (
    <code className={className} onDoubleClick={handleDoubleClick}>
      {content}
    </code>
  );
}

export function FencedCodeBlock({
  codeString,
  enableLinks,
  worktreeId,
  onFileOpenError,
}: {
  codeString: string;
  enableLinks: boolean;
  worktreeId: string | null;
  onFileOpenError?: (message: string | null) => void;
}) {
  const [copied, setCopied] = useState(false);
  const resetTimerRef = useRef<number | null>(null);

  useEffect(() => {
    if (!copied) return;
    if (resetTimerRef.current) window.clearTimeout(resetTimerRef.current);
    resetTimerRef.current = window.setTimeout(() => {
      setCopied(false);
      resetTimerRef.current = null;
    }, 1000);
    return () => {
      if (resetTimerRef.current) window.clearTimeout(resetTimerRef.current);
    };
  }, [copied]);

  const handleCopy = useCallback(async () => {
    const ok = await copyTextToClipboard(codeString);
    if (!ok) return;

    setCopied(true);
  }, [codeString]);

  const content = enableLinks
    ? buildCodeTokenNodes(codeString, { enableLinks, worktreeId, onFileOpenError })
    : codeString;

  return (
    <div className="codeblock">
      <div className="codeblock-toolbar">
        <button
          type="button"
          className="wb-icon codeblock-copy"
          aria-label={copied ? "Copied" : "Copy code"}
          title={copied ? "Copied" : "Copy"}
          onClick={() => void handleCopy()}
        >
          {copied ? <Check size={14} aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
        </button>
      </div>
      <div className="codeblock-body">
        <pre className="codeblock-pre" onWheelCapture={forwardVerticalWheelToTranscript}>
          <code className="codeblock-code">{content}</code>
        </pre>
      </div>
    </div>
  );
}

function normalizeMarkdownHref(href: string): string {
  if (href.startsWith("ctx://")) return href;
  return defaultUrlTransform(href);
}

export function renderMarkdownLink(
  href: string,
  children: ReactNode,
  opts: MarkdownRenderOptions,
  key: string,
  className?: string,
): ReactNode {
  const normalizedHref = normalizeMarkdownHref(href);
  const isContextOpen = normalizedHref.startsWith("ctx://open?");
  const markdownLinkClassName = [className, "ctx-markdown-link"].filter(Boolean).join(" ");
  if (!isContextOpen) {
    const modifierOpenTitle = isDesktopApp() ? "Cmd/Ctrl+Click to open link" : undefined;
    return (
      <ModifierAwareExternalLink
        key={key}
        href={normalizedHref}
        className={markdownLinkClassName}
        title={modifierOpenTitle}
        onClick={(event) => handleUrlTokenClick(event, normalizedHref)}
      >
        {children}
      </ModifierAwareExternalLink>
    );
  }

  const handleClick = async (event: MouseEvent<HTMLAnchorElement>) => {
    event.preventDefault();
    if (!isDesktopApp()) return;
    if (!event.metaKey && !event.ctrlKey) return;
    event.stopPropagation();
    try {
      await desktopOpenDeepLink(normalizedHref);
      opts.onFileOpenError?.(null);
    } catch {
      // Ignore failures to keep interaction silent.
    }
  };

  return (
    <ModifierAwareFileLink
      key={key}
      href={normalizedHref}
      className={[className, "ctx-markdown-link", "ctx-file-link"].filter(Boolean).join(" ")}
      title="Cmd/Ctrl+Click to open in editor"
      onClick={handleClick}
    >
      {children}
    </ModifierAwareFileLink>
  );
}
