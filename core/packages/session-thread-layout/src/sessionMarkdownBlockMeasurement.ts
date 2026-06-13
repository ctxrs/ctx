import {
  addPretextPerfBucket,
  hashPretextPerfValue,
  incrementPretextPerfCounter,
} from "./pretextPerfDiagnostics";
import {
  resolveSessionMarkdownBlockEntryGapPx,
  resolveSessionMarkdownBlockGapPx,
  type SessionMarkdownBlock,
  type SessionMarkdownBlockContext,
  type SessionMarkdownInlineRun,
} from "./sessionMarkdownContract";
import { measureInlineRunsHeight } from "./sessionMarkdownInlineMeasurement";
import type { InlineWrapMode } from "./sessionMarkdownInlineLayout";
import {
  BODY_LINE_HEIGHT_PX,
  BODY_TYPOGRAPHY,
  CODE_BLOCK_VERTICAL_PADDING_PX,
  MONO_LINE_HEIGHT_PX,
  TABLE_HEADER_TYPOGRAPHY,
  buildPreparedContentKey,
  buildHeadingTypography,
  clampHeight,
  measureTextHeight,
  normalizeHeight,
  parseMarkdown,
  type TextBlockTypography,
} from "./sessionMarkdownMeasurementCore";
import { measureSessionPlainTextBlockHeight } from "./sessionPlainTextMeasurement";
import { SESSION_MARKDOWN_MEASUREMENT_CONTRACT } from "./sessionThreadMeasurementContract";

function measureTextBlock(params: {
  text: {
    plainText: string;
    runs: readonly SessionMarkdownInlineRun[];
    hasInlineCode: boolean;
    hasHardBreak: boolean;
    hasStyledText: boolean;
    hasLink: boolean;
  };
  width: number;
  typography: TextBlockTypography;
  cacheKeyPrefix: string;
  wrapMode?: InlineWrapMode;
}): number {
  const text = params.text.plainText.trim();
  const useBreakWordInlineLayout =
    params.wrapMode === "break-word" || params.wrapMode === "anywhere";
  const hasSoftNewlines = text.includes("\n");
  if (!text) {
    return 0;
  }
  const plainTextHeight = () =>
    measureSessionPlainTextBlockHeight({
      cacheKey: buildPreparedContentKey(params.cacheKeyPrefix, text),
      text,
      font: params.typography.body,
      width: params.width,
      lineHeight: params.typography.lineHeight,
    });
  if (
    !useBreakWordInlineLayout &&
    params.text.hasHardBreak &&
    !params.text.hasInlineCode &&
    !params.text.hasStyledText &&
    !params.text.hasLink
  ) {
    return measureSessionPlainTextBlockHeight({
      cacheKey: `${params.cacheKeyPrefix}:plain-hardbreak`,
      text: params.text.plainText,
      font: params.typography.body,
      width: params.width,
      lineHeight: params.typography.lineHeight,
    });
  }
  if (
    !useBreakWordInlineLayout &&
    !params.text.hasInlineCode &&
    !params.text.hasHardBreak &&
    !hasSoftNewlines &&
    !params.text.hasStyledText &&
    !params.text.hasLink
  ) {
    return plainTextHeight();
  }
  const inlineRunsHeight = measureInlineRunsHeight({
    runs: params.text.runs,
    width: params.width,
    typography: params.typography,
    cacheKeyPrefix: params.cacheKeyPrefix,
    wrapMode: params.wrapMode,
  });
  return inlineRunsHeight;
}

function measureParagraph(block: Extract<SessionMarkdownBlock, { kind: "paragraph" }>, width: number): number {
  return measureTextBlock({
    text: block.text,
    width,
    typography: BODY_TYPOGRAPHY,
    cacheKeyPrefix: "paragraph-inline",
  });
}

function measureHeading(block: Extract<SessionMarkdownBlock, { kind: "heading" }>, width: number): number {
  return measureTextBlock({
    text: block.text,
    width,
    typography: buildHeadingTypography(block.depth),
    cacheKeyPrefix: `heading-inline:${block.depth}`,
  });
}

function measureCodeBlock(block: Extract<SessionMarkdownBlock, { kind: "code" }>): number {
  const lineCount = Math.max(1, block.code.replace(/\n$/, "").split("\n").length);
  const textHeight = clampHeight(lineCount * MONO_LINE_HEIGHT_PX);
  return (
    CODE_BLOCK_VERTICAL_PADDING_PX +
    textHeight +
    SESSION_MARKDOWN_MEASUREMENT_CONTRACT.codeBlock.borderWidthPx * 2
  );
}

function measureListItem(
  item: Extract<SessionMarkdownBlock, { kind: "list" }>["items"][number],
  width: number,
  bulletInsetPx: number,
): number {
  const bodyInsetPx = bulletInsetPx;
  const childWidth = Math.max(1, width - bodyInsetPx);
  if (item.blocks.length === 0) {
    return clampHeight(BODY_LINE_HEIGHT_PX);
  }
  return Math.max(clampHeight(BODY_LINE_HEIGHT_PX), measureBlockChildren(item.blocks, childWidth, "listItem"));
}

function measureList(block: Extract<SessionMarkdownBlock, { kind: "list" }>, width: number): number {
  let total = 0;
  for (let index = 0; index < block.items.length; index += 1) {
    const item = block.items[index]!;
    const markerInsetPx =
      item.checked != null
        ? SESSION_MARKDOWN_MEASUREMENT_CONTRACT.list.checkboxGutterPx
        : block.markerColumnWidthPx + SESSION_MARKDOWN_MEASUREMENT_CONTRACT.list.markerGapPx;
    total += measureListItem(item, width, markerInsetPx);
    if (index < block.items.length - 1) total += SESSION_MARKDOWN_MEASUREMENT_CONTRACT.list.gapPx;
  }
  return total;
}

function measureBlockQuote(block: Extract<SessionMarkdownBlock, { kind: "blockquote" }>, width: number): number {
  return measureBlockChildren(
    block.blocks,
    Math.max(1, width - SESSION_MARKDOWN_MEASUREMENT_CONTRACT.blockquote.insetPx),
    "root",
  );
}

function measureTableCellHeight(
  cell: Extract<SessionMarkdownBlock, { kind: "table" }>["rows"][number]["cells"][number] | null | undefined,
  width: number,
  isHeader: boolean,
): number {
  if (!cell) {
    return clampHeight(BODY_LINE_HEIGHT_PX);
  }
  if (cell.blocks.length === 0) {
    return clampHeight(BODY_LINE_HEIGHT_PX);
  }
  return cell.blocks.reduce((total, block, index) => {
    const marginTopPx =
      index === 0
        ? resolveSessionMarkdownBlockEntryGapPx(block.kind, "root")
        : resolveSessionMarkdownBlockGapPx(cell.blocks[index - 1]!.kind, block.kind, "root");
    let blockHeight: number;
    switch (block.kind) {
      case "paragraph":
        blockHeight = measureTextBlock({
          text: block.text,
          width,
          typography: isHeader ? TABLE_HEADER_TYPOGRAPHY : BODY_TYPOGRAPHY,
          cacheKeyPrefix: isHeader ? "table-header-inline" : "table-cell-inline",
          wrapMode: "normal",
        });
        break;
      case "heading":
        blockHeight = measureHeading(block, width);
        break;
      case "image":
        blockHeight = SESSION_MARKDOWN_MEASUREMENT_CONTRACT.image.heightPx;
        break;
      case "code":
        blockHeight = measureCodeBlock(block);
        break;
      case "thematicBreak":
        blockHeight = measureThematicBreak();
        break;
      default:
        blockHeight = measureBlock(block, width);
        break;
    }
    return total + marginTopPx + blockHeight;
  }, 0);
}

function estimateTableColumnContentWidths(
  availableContentWidth: number,
  columnCount: number,
): number[] {
  const equalWidth = Math.max(1, availableContentWidth / Math.max(1, columnCount));
  return Array.from({ length: columnCount }, () => equalWidth);
}

function measureTable(block: Extract<SessionMarkdownBlock, { kind: "table" }>, width: number): number {
  if (block.rows.length === 0) {
    return 0;
  }
  const columnCount = block.rows.reduce((max, row) => Math.max(max, row.cells.length), 0);
  if (columnCount <= 0) {
    return 0;
  }
  const borderWidthPx = SESSION_MARKDOWN_MEASUREMENT_CONTRACT.table.borderWidthPx;
  const cellPaddingInlinePx = SESSION_MARKDOWN_MEASUREMENT_CONTRACT.table.cellPaddingInlinePx;
  const cellPaddingBlockPx = SESSION_MARKDOWN_MEASUREMENT_CONTRACT.table.cellPaddingBlockPx;
  const totalBorderWidth = borderWidthPx * (columnCount + 1);
  const totalCellPaddingInlineWidth = columnCount * cellPaddingInlinePx * 2;
  const availableContentWidth = Math.max(
    1,
    Math.max(1, width) - totalBorderWidth - totalCellPaddingInlineWidth,
  );
  const columnContentWidths = estimateTableColumnContentWidths(availableContentWidth, columnCount);

  let height = borderWidthPx;
  for (let rowIndex = 0; rowIndex < block.rows.length; rowIndex += 1) {
    const row = block.rows[rowIndex]!;
    let rowHeight = clampHeight(BODY_LINE_HEIGHT_PX) + cellPaddingBlockPx * 2;
    for (let columnIndex = 0; columnIndex < columnCount; columnIndex += 1) {
      const cell = row.cells[columnIndex];
      const cellHeight =
        measureTableCellHeight(cell, columnContentWidths[columnIndex] ?? 1, rowIndex === 0) + cellPaddingBlockPx * 2;
      rowHeight = Math.max(rowHeight, cellHeight);
    }
    height += rowHeight + borderWidthPx;
  }
  return normalizeHeight(height);
}

function measureThematicBreak(): number {
  return 1;
}

function measureBlock(block: SessionMarkdownBlock, width: number): number {
  switch (block.kind) {
    case "heading":
      return measureHeading(block, width);
    case "list":
      return measureList(block, width);
    case "code":
      return measureCodeBlock(block);
    case "blockquote":
      return measureBlockQuote(block, width);
    case "table":
      return measureTable(block, width);
    case "thematicBreak":
      return measureThematicBreak();
    case "image":
      return SESSION_MARKDOWN_MEASUREMENT_CONTRACT.image.heightPx;
    case "paragraph":
    default:
      return measureParagraph(block, width);
  }
}

function measureBlockChildren(
  blocks: readonly SessionMarkdownBlock[],
  width: number,
  context: SessionMarkdownBlockContext,
): number {
  let total = 0;
  for (let index = 0; index < blocks.length; index += 1) {
    const block = blocks[index]!;
    total +=
      index === 0
        ? resolveSessionMarkdownBlockEntryGapPx(block.kind, context)
        : resolveSessionMarkdownBlockGapPx(blocks[index - 1]!.kind, block.kind, context);
    total += measureBlock(block, width);
  }
  return normalizeHeight(total);
}

export function measureSessionMarkdownDocument(markdown: string, width: number): number {
  const normalizedWidth = Math.max(1, width);
  const widthBucket = Math.round(normalizedWidth);
  incrementPretextPerfCounter("pretext_markdown_document_calls");
  addPretextPerfBucket(
    "pretext_markdown_document_key",
    `w${widthBucket}:${markdown.length}:${hashPretextPerfValue(markdown)}`,
  );
  const parsed = parseMarkdown(markdown);
  return measureBlockChildren(parsed.blocks, normalizedWidth, "root");
}
