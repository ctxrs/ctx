import { stripCitationMarkers } from "./citationMarkers";
import { splitWhitespaceTokens } from "./codeTokenLinks";
import {
  parseSessionMarkdown,
  readMarkdownChecked,
  readMarkdownDepth,
  readMarkdownOrdered,
  readMarkdownStart,
  type SessionMarkdownNode,
} from "./sessionMarkdownShared";
import { SESSION_MARKDOWN_MEASUREMENT_CONTRACT } from "./sessionThreadMeasurementContract";
import { resolveSessionMarkdownListMarkerColumnWidthPx } from "./sessionThreadLayoutTokens";

export type SessionMarkdownInlineNode =
  | { kind: "text"; text: string }
  | { kind: "inlineCode"; text: string; parts: readonly string[] }
  | { kind: "break" }
  | { kind: "strong"; children: SessionMarkdownInlineNode[] }
  | { kind: "emphasis"; children: SessionMarkdownInlineNode[] }
  | { kind: "delete"; children: SessionMarkdownInlineNode[] }
  | { kind: "link"; href: string; title: string | null; children: SessionMarkdownInlineNode[] }
  | { kind: "image"; src: string; alt: string; title: string | null };

export type SessionMarkdownInlineRun =
  | { kind: "hardBreak" }
  | { kind: "text"; text: string; style: SessionMarkdownTextRunStyle; deleted: boolean }
  | { kind: "inlineCode"; text: string; parts: readonly string[] };

export type SessionMarkdownTextRunStyle = "body" | "strong" | "emphasis" | "strongEmphasis";

export type SessionMarkdownTextContent = {
  plainText: string;
  runs: SessionMarkdownInlineRun[];
  hasInlineCode: boolean;
  hasHardBreak: boolean;
  hasStyledText: boolean;
  hasLink: boolean;
};

export type SessionMarkdownBlockContext = "root" | "listItem";

export type SessionMarkdownBlockKind =
  | "paragraph"
  | "image"
  | "heading"
  | "list"
  | "blockquote"
  | "code"
  | "table"
  | "thematicBreak";

export type SessionMarkdownParagraphBlock = {
  kind: "paragraph";
  node: SessionMarkdownNode;
  inlines: SessionMarkdownInlineNode[];
  text: SessionMarkdownTextContent;
};

export type SessionMarkdownImageBlock = {
  kind: "image";
  node: SessionMarkdownNode;
  src: string;
  alt: string;
  title: string | null;
};

export type SessionMarkdownHeadingBlock = {
  kind: "heading";
  node: SessionMarkdownNode;
  depth: number;
  inlines: SessionMarkdownInlineNode[];
  text: SessionMarkdownTextContent;
};

export type SessionMarkdownListItem = {
  checked: boolean | null;
  markerText: string | null;
  blocks: SessionMarkdownBlock[];
};

export type SessionMarkdownListBlock = {
  kind: "list";
  node: SessionMarkdownNode;
  ordered: boolean;
  start: number;
  markerColumnWidthPx: number;
  items: SessionMarkdownListItem[];
};

export type SessionMarkdownBlockquoteBlock = {
  kind: "blockquote";
  node: SessionMarkdownNode;
  blocks: SessionMarkdownBlock[];
};

export type SessionMarkdownCodeBlock = {
  kind: "code";
  node: SessionMarkdownNode;
  code: string;
  lang: string | null;
};

export type SessionMarkdownTableCell = {
  blocks: SessionMarkdownBlock[];
};

export type SessionMarkdownTableRow = {
  cells: SessionMarkdownTableCell[];
};

export type SessionMarkdownTableBlock = {
  kind: "table";
  node: SessionMarkdownNode;
  rows: SessionMarkdownTableRow[];
};

export type SessionMarkdownThematicBreakBlock = {
  kind: "thematicBreak";
  node: SessionMarkdownNode;
};

export type SessionMarkdownBlock =
  | SessionMarkdownParagraphBlock
  | SessionMarkdownImageBlock
  | SessionMarkdownHeadingBlock
  | SessionMarkdownListBlock
  | SessionMarkdownBlockquoteBlock
  | SessionMarkdownCodeBlock
  | SessionMarkdownTableBlock
  | SessionMarkdownThematicBreakBlock;

export type SessionMarkdownDocument = {
  source: string;
  blocks: SessionMarkdownBlock[];
};

const isRecord = (value: unknown): value is Record<string, unknown> =>
  Boolean(value) && typeof value === "object" && !Array.isArray(value);

const readString = (value: unknown): string => (typeof value === "string" ? value : "");

function normalizeInlineCodeParts(text: string): readonly string[] {
  return splitWhitespaceTokens(text.replace(/\u00a0/g, " "));
}

export function nodeChildren(node: SessionMarkdownNode | null | undefined): SessionMarkdownNode[] {
  if (!node || !Array.isArray(node.children)) return [];
  return node.children.filter((child): child is SessionMarkdownNode => isRecord(child));
}

function normalizeInlineNodes(nodes: readonly SessionMarkdownNode[]): SessionMarkdownInlineNode[] {
  const normalized: SessionMarkdownInlineNode[] = [];
  for (const node of nodes) {
    switch (node.type) {
      case "text":
        normalized.push({ kind: "text", text: readString(node.value).replace(/\u00a0/g, " ") });
        break;
      case "inlineCode":
        {
          const text = readString(node.value);
          normalized.push({ kind: "inlineCode", text, parts: normalizeInlineCodeParts(text) });
        }
        break;
      case "break":
        normalized.push({ kind: "break" });
        break;
      case "strong":
        normalized.push({ kind: "strong", children: normalizeInlineNodes(nodeChildren(node)) });
        break;
      case "emphasis":
        normalized.push({ kind: "emphasis", children: normalizeInlineNodes(nodeChildren(node)) });
        break;
      case "delete":
        normalized.push({ kind: "delete", children: normalizeInlineNodes(nodeChildren(node)) });
        break;
      case "link":
        normalized.push({
          kind: "link",
          href: readString(node.url),
          title: readString(node.title) || null,
          children: normalizeInlineNodes(nodeChildren(node)),
        });
        break;
      case "image":
        normalized.push({
          kind: "image",
          src: readString(node.url),
          alt: readString(node.alt),
          title: readString(node.title) || null,
        });
        break;
      default:
        normalized.push(...normalizeInlineNodes(nodeChildren(node)));
        break;
    }
  }
  return normalized;
}

function appendTextRun(
  runs: SessionMarkdownInlineRun[],
  text: string,
  style: SessionMarkdownTextRunStyle,
  deleted: boolean,
) {
  if (text.length === 0) return;
  const last = runs[runs.length - 1];
  if (last?.kind === "text" && last.style === style && last.deleted === deleted) {
    last.text += text;
    return;
  }
  runs.push({ kind: "text", text, style, deleted });
}

function resolveTextRunStyle(state: { strong: boolean; emphasis: boolean }): SessionMarkdownTextRunStyle {
  if (state.strong && state.emphasis) return "strongEmphasis";
  if (state.strong) return "strong";
  if (state.emphasis) return "emphasis";
  return "body";
}

function buildTextContent(inlines: readonly SessionMarkdownInlineNode[]): SessionMarkdownTextContent {
  const plainTextParts: string[] = [];
  const runs: SessionMarkdownInlineRun[] = [];
  let hasInlineCode = false;
  let hasHardBreak = false;
  let hasStyledText = false;
  let hasLink = false;

  const walk = (
    nodes: readonly SessionMarkdownInlineNode[],
    state: { strong: boolean; emphasis: boolean; deleted: boolean },
  ) => {
    for (const node of nodes) {
      switch (node.kind) {
        case "text": {
          const text = node.text.replace(/\u00a0/g, " ");
          const style = resolveTextRunStyle(state);
          plainTextParts.push(text);
          appendTextRun(runs, text, style, state.deleted);
          if (style !== "body" || state.deleted) hasStyledText = true;
          break;
        }
        case "inlineCode":
          plainTextParts.push(node.text);
          runs.push({ kind: "inlineCode", text: node.text, parts: node.parts });
          hasInlineCode = true;
          break;
        case "break":
          plainTextParts.push("\n");
          runs.push({ kind: "hardBreak" });
          hasHardBreak = true;
          break;
        case "image": {
          const alt = node.alt.trim();
          const style = resolveTextRunStyle(state);
          plainTextParts.push(alt);
          appendTextRun(runs, alt, style, state.deleted);
          if ((style !== "body" || state.deleted) && alt.length > 0) hasStyledText = true;
          break;
        }
        case "strong":
          walk(node.children, { ...state, strong: true });
          break;
        case "emphasis":
          walk(node.children, { ...state, emphasis: true });
          break;
        case "delete":
          walk(node.children, { ...state, deleted: true });
          break;
        case "link":
          hasLink = true;
          walk(node.children, state);
          break;
      }
    }
  };

  walk(inlines, { strong: false, emphasis: false, deleted: false });
  return {
    plainText: plainTextParts.join(""),
    runs,
    hasInlineCode,
    hasHardBreak,
    hasStyledText,
    hasLink,
  };
}

function isStandaloneImageParagraph(node: SessionMarkdownNode): boolean {
  if (node.type !== "paragraph") return false;
  const children = nodeChildren(node).filter((child) => child.type !== "text" || readString(child.value).trim().length > 0);
  return children.length === 1 && children[0]?.type === "image";
}

function normalizeTableRows(node: SessionMarkdownNode): SessionMarkdownTableRow[] {
  return nodeChildren(node)
    .filter((child) => child.type === "tableRow")
    .map((row) => ({
      cells: nodeChildren(row)
        .filter((cell) => cell.type === "tableCell")
        .map((cell) => {
          const inlines = normalizeInlineNodes(nodeChildren(cell));
          return {
            blocks:
              inlines.length === 0
                ? []
                : [
                    {
                      kind: "paragraph",
                      node: cell,
                      inlines,
                      text: buildTextContent(inlines),
                    },
                  ],
          };
        }),
    }));
}

function buildListMarkerText(ordered: boolean, start: number, index: number, checked: boolean | null): string | null {
  if (checked != null) return null;
  return ordered ? `${start + index}.` : "•";
}

function normalizeListItems(node: SessionMarkdownNode, ordered: boolean, start: number): SessionMarkdownListItem[] {
  return nodeChildren(node)
    .filter((child) => child.type === "listItem")
    .map((item, index) => {
      const checked = readMarkdownChecked(item);
      return {
        checked,
        markerText: buildListMarkerText(ordered, start, index, checked),
        blocks: normalizeSessionMarkdownBlocks(nodeChildren(item)),
      };
    });
}

export function normalizeSessionMarkdownBlocks(nodes: readonly SessionMarkdownNode[]): SessionMarkdownBlock[] {
  const normalized: SessionMarkdownBlock[] = [];
  for (const node of nodes) {
    switch (node.type) {
      case "definition":
      case "yaml":
      case "html":
        break;
      case "heading":
        {
          const inlines = normalizeInlineNodes(nodeChildren(node));
        normalized.push({
          kind: "heading",
          node,
          depth: readMarkdownDepth(node, 1),
          inlines,
          text: buildTextContent(inlines),
        });
        }
        break;
      case "list":
        {
          const ordered = readMarkdownOrdered(node);
          const start = readMarkdownStart(node, 1);
          const items = normalizeListItems(node, ordered, start);
        normalized.push({
          kind: "list",
          node,
          ordered,
          start,
          markerColumnWidthPx: resolveSessionMarkdownListMarkerColumnWidthPx(
            items.map((item) => item.markerText ?? ""),
          ),
          items,
        });
        }
        break;
      case "blockquote":
        normalized.push({
          kind: "blockquote",
          node,
          blocks: normalizeSessionMarkdownBlocks(nodeChildren(node)),
        });
        break;
      case "code":
        normalized.push({
          kind: "code",
          node,
          code: readString(node.value).replace(/[\r\n]+$/, ""),
          lang: readString(node.lang) || null,
        });
        break;
      case "table":
        normalized.push({
          kind: "table",
          node,
          rows: normalizeTableRows(node),
        });
        break;
      case "thematicBreak":
        normalized.push({ kind: "thematicBreak", node });
        break;
      case "paragraph":
        if (isStandaloneImageParagraph(node)) {
          const imageNode = nodeChildren(node)[0]!;
          normalized.push({
            kind: "image",
            node,
            src: readString(imageNode.url),
            alt: readString(imageNode.alt),
            title: readString(imageNode.title) || null,
          });
          break;
        }
        {
          const inlines = normalizeInlineNodes(nodeChildren(node));
        normalized.push({
          kind: "paragraph",
          node,
          inlines,
          text: buildTextContent(inlines),
        });
        }
        break;
      default:
        if (nodeChildren(node).length > 0) {
          normalized.push(...normalizeSessionMarkdownBlocks(nodeChildren(node)));
        } else {
          const inlines = normalizeInlineNodes([node]);
          normalized.push({
            kind: "paragraph",
            node,
            inlines,
            text: buildTextContent(inlines),
          });
        }
        break;
    }
  }
  return normalized;
}

export function createSessionMarkdownDocument(content: string): SessionMarkdownDocument {
  const source = stripCitationMarkers(content);
  return {
    source,
    blocks: normalizeSessionMarkdownBlocks(nodeChildren(parseSessionMarkdown(source))),
  };
}

function resolveBlockBeforePx(kind: SessionMarkdownBlockKind, context: SessionMarkdownBlockContext): number {
  return SESSION_MARKDOWN_MEASUREMENT_CONTRACT.blockSpacing.entryGapPxByContext[context][kind];
}

function resolveBlockAfterPx(kind: SessionMarkdownBlockKind, context: SessionMarkdownBlockContext): number {
  return SESSION_MARKDOWN_MEASUREMENT_CONTRACT.blockSpacing.exitGapPxByContext[context][kind];
}

export function resolveSessionMarkdownBlockEntryGapPx(
  kind: SessionMarkdownBlockKind,
  context: SessionMarkdownBlockContext,
): number {
  return resolveBlockBeforePx(kind, context);
}

export function resolveSessionMarkdownBlockGapPx(
  previousKind: SessionMarkdownBlockKind,
  nextKind: SessionMarkdownBlockKind,
  context: SessionMarkdownBlockContext,
): number {
  return Math.max(resolveBlockAfterPx(previousKind, context), resolveBlockBeforePx(nextKind, context));
}
