import { unified } from "unified";
import remarkGfm from "remark-gfm";
import remarkParse from "remark-parse";

export type SessionMarkdownNode = {
  type?: string;
  children?: SessionMarkdownNode[];
  value?: unknown;
  depth?: unknown;
  lang?: unknown;
  ordered?: unknown;
  start?: unknown;
  checked?: unknown;
  [key: string]: unknown;
};

const markdownProcessor = unified()
  .use(remarkParse)
  .use(remarkGfm);

function asNode(value: unknown): SessionMarkdownNode | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value as SessionMarkdownNode;
}

export function remarkNormalizeCursorMarkdown() {
  return (tree: SessionMarkdownNode) => {
    const walk = (node: SessionMarkdownNode) => {
      if (node?.type === "listItem" && Array.isArray(node.children)) {
        const maybeInlineFromCode = (codeNode: SessionMarkdownNode): string | null => {
          if (codeNode?.type !== "code") return null;
          const lang = typeof codeNode.lang === "string" ? codeNode.lang : undefined;
          const raw = typeof codeNode.value === "string" ? codeNode.value : "";
          const trimmed = raw.replace(/[\r\n]+$/, "");
          if ((!lang || lang === "code") && trimmed.length > 0 && !trimmed.includes("\n")) return trimmed;
          return null;
        };

        if (node.children.length === 1) {
          const trimmed = maybeInlineFromCode(node.children[0] as SessionMarkdownNode);
          if (trimmed) {
            node.children = [
              {
                type: "paragraph",
                children: [{ type: "inlineCode", value: trimmed }],
              },
            ];
          }
        }

        if (node.children.length === 2) {
          const [firstChild, secondChild] = node.children as SessionMarkdownNode[];
          const firstChildren = firstChild?.children;
          if (firstChild?.type === "paragraph" && Array.isArray(firstChildren)) {
            const onlyChild = asNode(firstChildren[0]);
            if (
              firstChildren.length === 1 &&
              onlyChild?.type === "text" &&
              String(onlyChild.value ?? "").trim().toLowerCase() === "code"
            ) {
              const trimmed = maybeInlineFromCode(secondChild);
              if (trimmed) {
                node.children = [
                  {
                    type: "paragraph",
                    children: [{ type: "inlineCode", value: trimmed }],
                  },
                ];
              }
            }
          }
        }
      }

      if (!Array.isArray(node.children)) return;
      for (let index = 0; index < node.children.length; index += 1) {
        const child = asNode(node.children[index]);
        if (!child) continue;
        if (
          child.type === "paragraph" &&
          Array.isArray(child.children) &&
          child.children.length === 1
        ) {
          const onlyChild = asNode(child.children[0]);
          if (onlyChild?.type === "text") {
            const trimmed = String(onlyChild.value ?? "").trim();
            if (/^(⸻|—{3,}|-{3,}|_{3,}|\*{3,})$/.test(trimmed)) {
              node.children[index] = { type: "thematicBreak" };
              continue;
            }
          }
        }
        walk(child);
      }
    };

    walk(tree);
  };
}

export function parseSessionMarkdown(content: string): SessionMarkdownNode {
  const tree = markdownProcessor.parse(content) as SessionMarkdownNode;
  remarkNormalizeCursorMarkdown()(tree);
  return tree;
}

export function readMarkdownDepth(node: SessionMarkdownNode, fallback = 1): number {
  const depth = Number(node.depth ?? fallback);
  return Number.isFinite(depth) && depth > 0 ? Math.floor(depth) : fallback;
}

export function readMarkdownOrdered(node: SessionMarkdownNode): boolean {
  return Boolean(node.ordered);
}

export function readMarkdownStart(node: SessionMarkdownNode, fallback = 1): number {
  const start = Number(node.start ?? fallback);
  return Number.isFinite(start) && start > 0 ? Math.floor(start) : fallback;
}

export function readMarkdownChecked(node: SessionMarkdownNode): boolean | null {
  if (typeof node.checked !== "boolean") return null;
  return node.checked;
}
