import {
  Fragment,
  memo,
  useMemo,
  type CSSProperties,
  type ReactNode,
} from "react";
import {
  createSessionMarkdownDocument,
  resolveSessionMarkdownBlockEntryGapPx,
  resolveSessionMarkdownBlockGapPx,
  type SessionMarkdownBlock,
  type SessionMarkdownBlockContext,
  type SessionMarkdownInlineNode,
  type SessionMarkdownListBlock,
  type SessionMarkdownListItem,
  type SessionMarkdownTableCell,
} from "../sessionThread/sessionMarkdownContract";
import {
  FencedCodeBlock,
  TokenizedInlineCode,
  forwardVerticalWheelToTranscript,
  joinClassNames,
  renderMarkdownLink,
  type MarkdownRenderOptions,
} from "./SessionPage.markdownInline";

function renderMarkdownImagePlaceholder(
  alt: string,
  title: string | null | undefined,
  key?: string,
): ReactNode {
  const label = (alt || title || "").trim();
  return (
    <span
      key={key}
      className="wb-md-image-placeholder"
      role="note"
      aria-label={label ? `Markdown image omitted: ${label}` : "Markdown image omitted"}
    >
      {label ? `Image omitted: ${label}` : "Image omitted"}
    </span>
  );
}

function renderInlineNodes(
  nodes: readonly SessionMarkdownInlineNode[],
  opts: MarkdownRenderOptions,
  keyPrefix: string,
): ReactNode[] {
  return nodes.map((node, index) => {
    const key = `${keyPrefix}-${index}`;
    switch (node.kind) {
      case "text":
        return node.text;
      case "break":
        return <br key={key} />;
      case "inlineCode":
        return (
          <TokenizedInlineCode
            key={key}
            codeString={node.text}
            codeParts={node.parts}
            enableLinks={opts.enableLinks}
            worktreeId={opts.worktreeId}
            onFileOpenError={opts.onFileOpenError}
          />
        );
      case "strong":
        return <strong key={key}>{renderInlineNodes(node.children, opts, `${key}-strong`)}</strong>;
      case "emphasis":
        return <em key={key}>{renderInlineNodes(node.children, opts, `${key}-emphasis`)}</em>;
      case "delete":
        return <del key={key}>{renderInlineNodes(node.children, opts, `${key}-delete`)}</del>;
      case "link":
        return renderMarkdownLink(
          node.href,
          renderInlineNodes(node.children, opts, `${key}-link`),
          opts,
          key,
        );
      case "image":
        return renderMarkdownImagePlaceholder(node.alt, node.title, key);
      default:
        return null;
    }
  });
}

function renderTableCellContent(
  cell: SessionMarkdownTableCell,
  isHeader: boolean,
  opts: MarkdownRenderOptions,
  keyPrefix: string,
): ReactNode {
  if (cell.blocks.length === 0) return null;
  return cell.blocks.map((block, index) => {
    const key = `${keyPrefix}-block-${index}`;
    switch (block.kind) {
      case "paragraph":
        return <Fragment key={key}>{renderInlineNodes(block.inlines, opts, `${key}-paragraph`)}</Fragment>;
      case "heading":
        switch (Math.max(1, Math.min(4, block.depth))) {
          case 1:
            return <h1 key={key}>{renderInlineNodes(block.inlines, opts, `${key}-heading`)}</h1>;
          case 2:
            return <h2 key={key}>{renderInlineNodes(block.inlines, opts, `${key}-heading`)}</h2>;
          case 3:
            return <h3 key={key}>{renderInlineNodes(block.inlines, opts, `${key}-heading`)}</h3>;
          default:
            return <h4 key={key}>{renderInlineNodes(block.inlines, opts, `${key}-heading`)}</h4>;
        }
      case "image":
        return renderMarkdownImagePlaceholder(block.alt, block.title, key);
      case "thematicBreak":
        return <Fragment key={key}>{isHeader ? "—" : "—"}</Fragment>;
      case "code":
        return <code key={key} className="codeblock-code">{block.code}</code>;
      default:
        return null;
    }
  });
}

function renderListItem(item: SessionMarkdownListItem, opts: MarkdownRenderOptions, keyPrefix: string): ReactNode {
  const checkbox =
    item.checked == null ? null : (
      <input
        type="checkbox"
        checked={item.checked}
        readOnly
        disabled
        aria-hidden="true"
        className="wb-md-task-checkbox"
      />
    );
  return (
    <li key={keyPrefix} className={item.checked != null ? "wb-md-task-item" : "wb-md-list-item"}>
      {checkbox ? (
        <div className="wb-md-task-row">
          {checkbox}
          <div className="wb-md-list-item-body wb-md-stack">
            {renderBlockStack(item.blocks, opts, "listItem", `${keyPrefix}-body`)}
          </div>
        </div>
      ) : (
        <div className="wb-md-list-row">
          <span className="wb-md-list-item-marker" aria-hidden="true">
            {item.markerText}
          </span>
          <div className="wb-md-list-item-body wb-md-stack">
            {renderBlockStack(item.blocks, opts, "listItem", `${keyPrefix}-body`)}
          </div>
        </div>
      )}
    </li>
  );
}

function renderListBlock(
  block: SessionMarkdownListBlock,
  opts: MarkdownRenderOptions,
  keyPrefix: string,
): ReactNode {
  const ListTag = (block.ordered ? "ol" : "ul") as "ol" | "ul";
  const style = {
    "--wb-md-marker-column-width": `${block.markerColumnWidthPx}px`,
  } as CSSProperties;
  return (
    <ListTag
      className={block.ordered ? "wb-md-ordered-list" : "wb-md-unordered-list"}
      style={style}
    >
      {block.items.map((item, index) => renderListItem(item, opts, `${keyPrefix}-item-${index}`))}
    </ListTag>
  );
}

function renderBlock(
  block: SessionMarkdownBlock,
  opts: MarkdownRenderOptions,
  context: SessionMarkdownBlockContext,
  marginTopPx: number,
  keyPrefix: string,
): ReactNode {
  const shellStyle = marginTopPx > 0 ? { marginTop: `${marginTopPx}px` } : undefined;
  switch (block.kind) {
    case "paragraph":
      return (
        <div key={keyPrefix} className="wb-md-block wb-md-block--paragraph" style={shellStyle}>
          <p>{renderInlineNodes(block.inlines, opts, `${keyPrefix}-paragraph`)}</p>
        </div>
      );
    case "image":
      return (
        <div key={keyPrefix} className="wb-md-block wb-md-block--image" style={shellStyle}>
          {renderMarkdownImagePlaceholder(block.alt, block.title)}
        </div>
      );
    case "heading":
      return (
        <div key={keyPrefix} className={`wb-md-block wb-md-block--heading wb-md-block--heading-${block.depth}`} style={shellStyle}>
          {Math.max(1, Math.min(4, block.depth)) === 1 ? (
            <h1>{renderInlineNodes(block.inlines, opts, `${keyPrefix}-heading`)}</h1>
          ) : Math.max(1, Math.min(4, block.depth)) === 2 ? (
            <h2>{renderInlineNodes(block.inlines, opts, `${keyPrefix}-heading`)}</h2>
          ) : Math.max(1, Math.min(4, block.depth)) === 3 ? (
            <h3>{renderInlineNodes(block.inlines, opts, `${keyPrefix}-heading`)}</h3>
          ) : (
            <h4>{renderInlineNodes(block.inlines, opts, `${keyPrefix}-heading`)}</h4>
          )}
        </div>
      );
    case "list":
      return (
        <div key={keyPrefix} className="wb-md-block wb-md-block--list" style={shellStyle}>
          {renderListBlock(block, opts, `${keyPrefix}-list`)}
        </div>
      );
    case "blockquote":
      return (
        <div key={keyPrefix} className="wb-md-block wb-md-block--blockquote" style={shellStyle}>
          <blockquote className="wb-md-blockquote">
            <div className="wb-md-stack">{renderBlockStack(block.blocks, opts, "root", `${keyPrefix}-blockquote`)}</div>
          </blockquote>
        </div>
      );
    case "code":
      return (
        <div key={keyPrefix} className="wb-md-block wb-md-block--code" style={shellStyle}>
          <FencedCodeBlock
            codeString={block.code}
            enableLinks={opts.enableLinks}
            worktreeId={opts.worktreeId}
            onFileOpenError={opts.onFileOpenError}
          />
        </div>
      );
    case "table":
      return (
        <div key={keyPrefix} className="wb-md-block wb-md-block--table" style={shellStyle}>
          <div className="wb-md-table-scroll" onWheelCapture={forwardVerticalWheelToTranscript}>
            <table className="wb-md-table">
              <tbody className="wb-md-table-body">
                {block.rows.map((row, rowIndex) => (
                  <tr key={`${keyPrefix}-row-${rowIndex}`} className="wb-md-table-row">
                    {row.cells.map((cell, cellIndex) => {
                      const isHeader = rowIndex === 0;
                      const CellTag = isHeader ? "th" : "td";
                      return (
                        <CellTag
                          key={`${keyPrefix}-row-${rowIndex}-cell-${cellIndex}`}
                          className={joinClassNames(
                            "wb-md-table-cell",
                            isHeader && "wb-md-table-cell-head",
                          )}
                        >
                          {renderTableCellContent(
                            cell,
                            isHeader,
                            opts,
                            `${keyPrefix}-row-${rowIndex}-cell-${cellIndex}`,
                          )}
                        </CellTag>
                      );
                    })}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      );
    case "thematicBreak":
      return (
        <div key={keyPrefix} className="wb-md-block wb-md-block--thematic-break" style={shellStyle}>
          <hr />
        </div>
      );
    default:
      return null;
  }
}

function renderBlockStack(
  blocks: readonly SessionMarkdownBlock[],
  opts: MarkdownRenderOptions,
  context: SessionMarkdownBlockContext,
  keyPrefix: string,
): ReactNode[] {
  return blocks.map((block, index) => {
    const marginTopPx =
      index === 0
        ? resolveSessionMarkdownBlockEntryGapPx(block.kind, context)
        : resolveSessionMarkdownBlockGapPx(blocks[index - 1]!.kind, block.kind, context);
    return renderBlock(block, opts, context, marginTopPx, `${keyPrefix}-${index}`);
  });
}

export function Markdown({
  content,
  linkifyFiles = false,
  worktreeId = null,
  onFileOpenError,
}: {
  content: string;
  linkifyFiles?: boolean;
  worktreeId?: string | null;
  onFileOpenError?: (message: string | null) => void;
}) {
  const blocks = useMemo(() => createSessionMarkdownDocument(content).blocks, [content]);
  const renderOptions = useMemo<MarkdownRenderOptions>(
    () => ({
      enableLinks: Boolean(linkifyFiles),
      worktreeId,
      onFileOpenError,
    }),
    [linkifyFiles, onFileOpenError, worktreeId],
  );

  return (
    <div className="wb-markdown-root wb-md-stack">
      {renderBlockStack(blocks, renderOptions, "root", "markdown")}
    </div>
  );
}

// Memoize markdown so selection isn't disrupted by unrelated re-renders.
export const MemoMarkdown = memo(
  Markdown,
  (prev, next) =>
    prev.content === next.content &&
    prev.linkifyFiles === next.linkifyFiles &&
    prev.worktreeId === next.worktreeId,
);
