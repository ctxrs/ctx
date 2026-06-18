import { toMarkdown } from "mdast-util-to-markdown";
import remarkFrontmatter from "remark-frontmatter";
import remarkGfm from "remark-gfm";
import remarkMdx from "remark-mdx";
import remarkParse from "remark-parse";
import { unified } from "unified";
import { visit } from "unist-util-visit";
import type { Artifact } from "../api/client";
import { artifactPathBaseName, artifactPathExtension } from "./artifactPaths";

const MARKDOWN_EXTENSIONS = new Set(["md", "markdown", "mdown", "mkd", "mkdn"]);
const MDX_EXTENSIONS = new Set(["mdx"]);
const TEXT_EXTENSIONS = new Set(["txt", "text", "log"]);
const JSON_EXTENSIONS = new Set(["json", "jsonl", "ndjson", "jsonc"]);
const CODE_EXTENSIONS = new Set([
  "c",
  "cc",
  "conf",
  "cpp",
  "cs",
  "css",
  "go",
  "h",
  "hpp",
  "ini",
  "java",
  "js",
  "jsx",
  "kt",
  "less",
  "mjs",
  "py",
  "rb",
  "rs",
  "scss",
  "sh",
  "sql",
  "swift",
  "toml",
  "ts",
  "tsx",
  "yaml",
  "yml",
  "zsh",
]);
const TEXT_FILENAMES = new Set(["dockerfile", ".env", ".gitignore", ".npmrc"]);
const MARKDOWN_MIME_TYPES = new Set(["text/markdown", "text/x-markdown"]);
const MDX_MIME_TYPES = new Set(["application/mdx", "text/mdx"]);

const MDX_DROP_TYPES = new Set([
  "yaml",
  "mdxFlowExpression",
  "mdxJsxFlowElement",
  "mdxJsxTextElement",
  "mdxTextExpression",
  "mdxjsEsm",
]);

export type ArtifactDocumentFormat = "markdown" | "mdx" | "text";
export type ArtifactDocumentRenderKind = "markdown" | "text";

export type ArtifactDocumentPreview = {
  content: string;
  format: ArtifactDocumentFormat;
  renderKind: ArtifactDocumentRenderKind;
};

type MdastNode = {
  type: string;
  children?: MdastNode[];
};

type MdastRoot = MdastNode;

function artifactPathParts(artifact: Artifact): { baseName: string; extension: string; mime: string } {
  const baseName = artifactPathBaseName(artifact).toLowerCase();
  return {
    baseName,
    extension: artifactPathExtension(artifact),
    mime: (artifact.mime_type ?? "").trim().toLowerCase(),
  };
}

function sanitizeMdxTree(root: MdastRoot): MdastRoot {
  visit(root as MdastNode, (node: MdastNode, index, parent: MdastNode | undefined) => {
    if (!parent || typeof index !== "number") return;
    if (!MDX_DROP_TYPES.has(node.type)) return;
    parent.children?.splice(index, 1);
    return index;
  });
  return root;
}

export function normalizeMdxDocumentContent(content: string): string {
  const tree = unified()
    .use(remarkParse)
    .use(remarkFrontmatter, ["yaml"])
    .use(remarkMdx)
    .use(remarkGfm)
    .parse(content) as MdastRoot;
  const sanitized = sanitizeMdxTree(tree);
  return toMarkdown(sanitized as Parameters<typeof toMarkdown>[0]).replace(/\n{3,}/g, "\n\n").trim();
}

export function getArtifactDocumentFormat(artifact: Artifact): ArtifactDocumentFormat | null {
  const { baseName, extension, mime } = artifactPathParts(artifact);
  if (MDX_MIME_TYPES.has(mime) || MDX_EXTENSIONS.has(extension)) return "mdx";
  if (MARKDOWN_MIME_TYPES.has(mime) || MARKDOWN_EXTENSIONS.has(extension)) return "markdown";
  if (mime === "text/plain") return "text";
  if (mime === "application/json" || mime === "text/json" || mime.endsWith("+json")) return "text";
  if (TEXT_EXTENSIONS.has(extension) || JSON_EXTENSIONS.has(extension) || CODE_EXTENSIONS.has(extension)) {
    return "text";
  }
  if (TEXT_FILENAMES.has(baseName)) return "text";
  return null;
}

export function buildArtifactDocumentPreview(
  artifact: Artifact,
  content: string,
): ArtifactDocumentPreview | null {
  const format = getArtifactDocumentFormat(artifact);
  if (!format) return null;
  if (format === "markdown") {
    return { format, renderKind: "markdown", content };
  }
  if (format === "mdx") {
    try {
      return {
        format,
        renderKind: "markdown",
        content: normalizeMdxDocumentContent(content),
      };
    } catch {
      // Invalid MDX should remain readable as raw text rather than breaking preview.
      return { format, renderKind: "text", content };
    }
  }
  return { format, renderKind: "text", content };
}
