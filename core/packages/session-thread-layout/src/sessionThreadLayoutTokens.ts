import {
  SESSION_THREAD_GEOMETRY_SPEC,
  resolveSessionThreadAskUserShellHeightPx,
  resolveSessionThreadContentMaxWidthPx,
  resolveSessionThreadMarkdownBlockquoteInsetPx,
  resolveSessionThreadMarkdownInlineCodeEdgeBlockPx,
  resolveSessionThreadMarkdownInlineCodeEdgeInlinePx,
  resolveSessionThreadMarkdownInlineCodeFragmentChromeHeightPx,
  resolveSessionThreadMarkdownInlineCodeFragmentChromeWidthPx,
  resolveSessionThreadThoughtHorizontalChromePx,
  resolveSessionThreadThoughtVerticalChromePx,
  resolveSessionThreadToolSummaryRowHeightPx,
  resolveSessionThreadToolThoughtBodyChromeHeightPx,
  resolveSessionThreadToolThoughtBodyChromeWidthPx,
  resolveSessionThreadToolThoughtTitleHeightPx,
} from "./sessionThreadGeometrySpec";

const spec = SESSION_THREAD_GEOMETRY_SPEC;

export const SESSION_THREAD_ROW_MAX_WIDTH_PX = spec.viewport.rowMaxWidthPx;
export const SESSION_THREAD_HORIZONTAL_INSET_PX = spec.viewport.horizontalInsetPx;
export const SESSION_THREAD_INDENT_LEFT_PX = spec.viewport.indentLeftPx;
export const SESSION_THREAD_CONTENT_MAX_WIDTH_PX = resolveSessionThreadContentMaxWidthPx(spec);
export const SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX = spec.markdown.typography.bodyFontSizePx;
export const SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX =
  spec.markdown.typography.bodyLineHeightPx;
export const SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY = spec.markdown.typography.bodyFontFamily;
export const SESSION_THREAD_MARKDOWN_HEADING_FONT_SIZE_PX_BY_DEPTH =
  spec.markdown.typography.headingFontSizePxByDepth;
export const SESSION_THREAD_MARKDOWN_HEADING_LINE_HEIGHT_PX_BY_DEPTH =
  spec.markdown.typography.headingLineHeightPxByDepth;
export const SESSION_THREAD_MARKDOWN_FONT_WEIGHT = spec.markdown.typography.fontWeight;
export const SESSION_THREAD_MARKDOWN_BLOCK_MARGIN_BOTTOM_PX =
  spec.markdown.blockSpacing.blockMarginBottomPx;
export const SESSION_THREAD_MARKDOWN_HEADING_MARGIN_TOP_PX =
  spec.markdown.blockSpacing.headingMarginTopPx;
export const SESSION_THREAD_MARKDOWN_HEADING_MARGIN_BOTTOM_PX =
  spec.markdown.blockSpacing.headingMarginBottomPx;
export const SESSION_THREAD_MARKDOWN_LIST_INDENT_PX = spec.markdown.list.indentPx;
export const SESSION_THREAD_MARKDOWN_LIST_GAP_PX = spec.markdown.list.gapPx;
export const SESSION_THREAD_MARKDOWN_LIST_MARKER_MIN_WIDTH_PX =
  spec.markdown.list.markerMinWidthPx;
export const SESSION_THREAD_MARKDOWN_LIST_MARKER_GAP_PX = spec.markdown.list.markerGapPx;
export const SESSION_THREAD_MARKDOWN_LIST_MARKER_ADVANCE_PX =
  spec.markdown.list.markerAdvancePx;
export const SESSION_THREAD_MARKDOWN_INLINE_CODE_PADDING_BLOCK_PX =
  spec.markdown.inlineCode.paddingBlockPx;
export const SESSION_THREAD_MARKDOWN_INLINE_CODE_PADDING_INLINE_PX =
  spec.markdown.inlineCode.paddingInlinePx;
export const SESSION_THREAD_MARKDOWN_INLINE_CODE_BORDER_WIDTH_PX =
  spec.markdown.inlineCode.borderWidthPx;
export const SESSION_THREAD_MARKDOWN_INLINE_CODE_BORDER_RADIUS_PX =
  spec.markdown.inlineCode.borderRadiusPx;
export const SESSION_THREAD_MARKDOWN_INLINE_CODE_FONT_SIZE_PX =
  spec.markdown.typography.inlineCodeFontSizePx;
export const SESSION_THREAD_MARKDOWN_INLINE_CODE_FONT_FAMILY =
  spec.markdown.typography.inlineCodeFontFamily;
export const SESSION_THREAD_MARKDOWN_INLINE_CODE_EDGE_BLOCK_PX =
  resolveSessionThreadMarkdownInlineCodeEdgeBlockPx(spec);
export const SESSION_THREAD_MARKDOWN_INLINE_CODE_EDGE_PX =
  resolveSessionThreadMarkdownInlineCodeEdgeInlinePx(spec);
export const SESSION_THREAD_MARKDOWN_INLINE_CODE_FRAGMENT_CHROME_HEIGHT_PX =
  resolveSessionThreadMarkdownInlineCodeFragmentChromeHeightPx(spec);
export const SESSION_THREAD_MARKDOWN_INLINE_CODE_FRAGMENT_CHROME_WIDTH_PX =
  resolveSessionThreadMarkdownInlineCodeFragmentChromeWidthPx(spec);
export const SESSION_THREAD_MARKDOWN_CODE_BLOCK_FONT_SIZE_PX =
  spec.markdown.typography.codeBlockFontSizePx;
export const SESSION_THREAD_MARKDOWN_CODE_BLOCK_LINE_HEIGHT_PX =
  spec.markdown.typography.codeBlockLineHeightPx;
export const SESSION_THREAD_MARKDOWN_BLOCKQUOTE_BORDER_WIDTH_PX =
  spec.markdown.blockquote.borderWidthPx;
export const SESSION_THREAD_MARKDOWN_BLOCKQUOTE_PADDING_INLINE_START_PX =
  spec.markdown.blockquote.paddingInlineStartPx;
export const SESSION_THREAD_MARKDOWN_BLOCKQUOTE_INSET_PX =
  resolveSessionThreadMarkdownBlockquoteInsetPx(spec);
export const SESSION_THREAD_MARKDOWN_IMAGE_WIDTH_PX = spec.markdown.image.widthPx;
export const SESSION_THREAD_MARKDOWN_IMAGE_HEIGHT_PX = spec.markdown.image.heightPx;
export const SESSION_THREAD_MARKDOWN_CODE_BLOCK_BORDER_WIDTH_PX =
  spec.markdown.codeBlock.borderWidthPx;
export const SESSION_THREAD_MARKDOWN_CODE_BLOCK_PADDING_TOP_PX =
  spec.markdown.codeBlock.paddingTopPx;
export const SESSION_THREAD_MARKDOWN_CODE_BLOCK_PADDING_BOTTOM_PX =
  spec.markdown.codeBlock.paddingBottomPx;
export const SESSION_THREAD_MARKDOWN_TABLE_BORDER_WIDTH_PX =
  spec.markdown.table.borderWidthPx;
export const SESSION_THREAD_MARKDOWN_TABLE_CELL_PADDING_BLOCK_PX =
  spec.markdown.table.cellPaddingBlockPx;
export const SESSION_THREAD_MARKDOWN_TABLE_CELL_PADDING_INLINE_PX =
  spec.markdown.table.cellPaddingInlinePx;
export const SESSION_THREAD_MESSAGE_ROW_PADDING_BLOCK_PX = spec.rows.message.rowPaddingBlockPx;
export const SESSION_THREAD_MESSAGE_BUBBLE_PADDING_BLOCK_PX =
  spec.rows.message.bubblePaddingBlockPx;
export const SESSION_THREAD_MESSAGE_BUBBLE_PADDING_INLINE_PX =
  spec.rows.message.bubblePaddingInlinePx;
export const SESSION_THREAD_MESSAGE_BUBBLE_BORDER_WIDTH_PX =
  spec.rows.message.bubbleBorderWidthPx;
export const SESSION_THREAD_MESSAGE_MAX_WIDTH_RATIO = spec.rows.message.maxWidthRatio;
export const SESSION_THREAD_MESSAGE_ROLE_FONT_SIZE_PX = spec.rows.message.roleFontSizePx;
export const SESSION_THREAD_MESSAGE_ROLE_LINE_HEIGHT_PX = spec.rows.message.roleLineHeightPx;
export const SESSION_THREAD_MESSAGE_TOGGLE_MARGIN_TOP_PX = spec.rows.message.toggleMarginTopPx;
export const SESSION_THREAD_MESSAGE_TOGGLE_FONT_SIZE_PX = spec.rows.message.toggleFontSizePx;
export const SESSION_THREAD_MESSAGE_TOGGLE_LINE_HEIGHT_PX = spec.rows.message.toggleLineHeightPx;
export const SESSION_THREAD_MESSAGE_ATTACHMENT_WIDTH_PX =
  spec.rows.message.attachments.widthPx;
export const SESSION_THREAD_MESSAGE_ATTACHMENT_HEIGHT_PX =
  spec.rows.message.attachments.heightPx;
export const SESSION_THREAD_MESSAGE_ATTACHMENT_GAP_PX = spec.rows.message.attachments.gapPx;
export const SESSION_THREAD_MESSAGE_ATTACHMENT_MARGIN_TOP_PX =
  spec.rows.message.attachments.marginTopPx;
export const SESSION_THREAD_ASSISTANT_ENTRY_PADDING_INLINE_PX =
  spec.rows.assistant.entryPaddingInlinePx;
export const SESSION_THREAD_TURN_HEADER_BUBBLE_PADDING_BLOCK_PX =
  spec.rows.turnHeader.bubblePaddingBlockPx;
export const SESSION_THREAD_TURN_HEADER_BUBBLE_PADDING_INLINE_PX =
  spec.rows.turnHeader.bubblePaddingInlinePx;
export const SESSION_THREAD_TURN_HEADER_BUBBLE_BORDER_WIDTH_PX =
  spec.rows.turnHeader.bubbleBorderWidthPx;
export const SESSION_THREAD_TURN_HEADER_COPY_GUTTER_PX = spec.rows.turnHeader.copyGutterPx;
export const SESSION_THREAD_TURN_HEADER_COLLAPSED_MAX_HEIGHT_PX =
  spec.rows.turnHeader.collapsedMaxHeightPx;
export const SESSION_THREAD_ASK_USER_MARGIN_VERTICAL_PX = spec.rows.askUser.marginVerticalPx;
export const SESSION_THREAD_ASK_USER_CARD_MAX_WIDTH_PX = spec.rows.askUser.cardMaxWidthPx;
export const SESSION_THREAD_ASK_USER_CARD_MIN_WIDTH_PX = spec.rows.askUser.cardMinWidthPx;
export const SESSION_THREAD_ASK_USER_CARD_PADDING_PX = spec.rows.askUser.cardPaddingPx;
export const SESSION_THREAD_ASK_USER_CARD_GAP_PX = spec.rows.askUser.cardGapPx;
export const SESSION_THREAD_ASK_USER_TABS_HEIGHT_PX = spec.rows.askUser.tabsHeightPx;
export const SESSION_THREAD_ASK_USER_PANEL_HEIGHT_PX = spec.rows.askUser.panelHeightPx;
export const SESSION_THREAD_ASK_USER_STATUS_HEIGHT_PX = spec.rows.askUser.statusHeightPx;
export const SESSION_THREAD_ASK_USER_ACTIONS_HEIGHT_PX = spec.rows.askUser.actionsHeightPx;
export const SESSION_THREAD_ASK_USER_HINT_HEIGHT_PX = spec.rows.askUser.hintHeightPx;
export const SESSION_THREAD_ASK_USER_SHELL_HEIGHT_PX =
  resolveSessionThreadAskUserShellHeightPx(spec);
export const SESSION_THREAD_THOUGHT_PADDING_INLINE_PX = spec.rows.thought.paddingInlinePx;
export const SESSION_THREAD_THOUGHT_PADDING_BLOCK_PX = spec.rows.thought.paddingBlockPx;
export const SESSION_THREAD_THOUGHT_FONT_FAMILY = spec.rows.thought.typography.fontFamily;
export const SESSION_THREAD_THOUGHT_FONT_SIZE_PX = spec.rows.thought.typography.fontSizePx;
export const SESSION_THREAD_THOUGHT_LINE_HEIGHT_PX = spec.rows.thought.typography.lineHeightPx;
export const SESSION_THREAD_TOOL_SUMMARY_PADDING_INLINE_PX =
  spec.rows.tools.summary.paddingInlinePx;
export const SESSION_THREAD_TOOL_SUMMARY_PADDING_BLOCK_PX =
  spec.rows.tools.summary.paddingBlockPx;
export const SESSION_THREAD_TOOL_SUMMARY_FONT_FAMILY =
  spec.rows.tools.summary.typography.fontFamily;
export const SESSION_THREAD_TOOL_SUMMARY_FONT_SIZE_PX =
  spec.rows.tools.summary.typography.fontSizePx;
export const SESSION_THREAD_TOOL_SUMMARY_LINE_HEIGHT_PX =
  spec.rows.tools.summary.typography.lineHeightPx;
export const SESSION_THREAD_TOOL_SEPARATOR_PADDING_INLINE_PX =
  spec.rows.tools.summary.separatorPaddingInlinePx;
export const SESSION_THREAD_TOOL_STATUS_DOT_PADDING_INLINE_PX =
  spec.rows.tools.summary.statusDotPaddingInlinePx;
export const SESSION_THREAD_TOOL_LOADING_FONT_FAMILY =
  spec.rows.tools.loading.typography.fontFamily;
export const SESSION_THREAD_TOOL_LOADING_FONT_SIZE_PX =
  spec.rows.tools.loading.typography.fontSizePx;
export const SESSION_THREAD_TOOL_LOADING_LINE_HEIGHT_PX =
  spec.rows.tools.loading.typography.lineHeightPx;
export const SESSION_THREAD_TOOL_ITEM_GAP_PX = spec.rows.tools.itemGapPx;
export const SESSION_THREAD_TOOL_GROUP_GAP_PX = spec.rows.tools.groupGapPx;
export const SESSION_THREAD_TOOL_THOUGHT_TITLE_FONT_FAMILY =
  spec.rows.tools.thoughtTitle.typography.fontFamily;
export const SESSION_THREAD_TOOL_THOUGHT_TITLE_FONT_SIZE_PX =
  spec.rows.tools.thoughtTitle.typography.fontSizePx;
export const SESSION_THREAD_TOOL_THOUGHT_TITLE_LINE_HEIGHT_PX =
  spec.rows.tools.thoughtTitle.typography.lineHeightPx;
export const SESSION_THREAD_TOOL_THOUGHT_TITLE_MARGIN_BOTTOM_PX =
  spec.rows.tools.thoughtTitle.marginBottomPx;
export const SESSION_THREAD_TOOL_THOUGHT_BODY_PADDING_PX = spec.rows.tools.thoughtBody.paddingPx;
export const SESSION_THREAD_TOOL_THOUGHT_BODY_BORDER_WIDTH_PX =
  spec.rows.tools.thoughtBody.borderWidthPx;
export const SESSION_THREAD_TOOL_THOUGHT_BODY_FONT_FAMILY =
  spec.rows.tools.thoughtBody.typography.fontFamily;
export const SESSION_THREAD_TOOL_THOUGHT_BODY_FONT_SIZE_PX =
  spec.rows.tools.thoughtBody.typography.fontSizePx;
export const SESSION_THREAD_TOOL_THOUGHT_BODY_LINE_HEIGHT_PX =
  spec.rows.tools.thoughtBody.typography.lineHeightPx;

export function resolveSessionThreadRowWidth(viewportWidth: number): number {
  return Math.max(1, Math.min(SESSION_THREAD_ROW_MAX_WIDTH_PX, Math.floor(viewportWidth)));
}

export function resolveSessionThreadContentWidth(viewportWidth: number): number {
  return Math.max(
    1,
    resolveSessionThreadRowWidth(viewportWidth) - SESSION_THREAD_HORIZONTAL_INSET_PX * 2,
  );
}

export function resolveSessionThreadIndentedContentWidth(viewportWidth: number): number {
  return Math.max(1, resolveSessionThreadContentWidth(viewportWidth) - SESSION_THREAD_INDENT_LEFT_PX);
}

export function resolveSessionThreadMessageBubbleBorderBoxWidth(viewportWidth: number): number {
  return Math.max(
    1,
    resolveSessionThreadIndentedContentWidth(viewportWidth) * SESSION_THREAD_MESSAGE_MAX_WIDTH_RATIO,
  );
}

export function resolveSessionThreadMessageTextWidth(viewportWidth: number): number {
  return Math.max(
    1,
    resolveSessionThreadMessageBubbleBorderBoxWidth(viewportWidth) -
      SESSION_THREAD_MESSAGE_BUBBLE_PADDING_INLINE_PX * 2 -
      SESSION_THREAD_MESSAGE_BUBBLE_BORDER_WIDTH_PX * 2,
  );
}

export function resolveSessionThreadAssistantTextWidth(viewportWidth: number): number {
  return Math.max(
    1,
    resolveSessionThreadIndentedContentWidth(viewportWidth) -
      SESSION_THREAD_ASSISTANT_ENTRY_PADDING_INLINE_PX * 2,
  );
}

export function resolveSessionThreadTurnHeaderTextWidth(viewportWidth: number): number {
  return Math.max(
    1,
    resolveSessionThreadContentWidth(viewportWidth) -
      SESSION_THREAD_TURN_HEADER_BUBBLE_BORDER_WIDTH_PX * 2 -
      SESSION_THREAD_TURN_HEADER_BUBBLE_PADDING_INLINE_PX * 2 -
      SESSION_THREAD_TURN_HEADER_COPY_GUTTER_PX,
  );
}

export function resolveSessionThreadAskUserCardWidth(viewportWidth: number): number {
  return Math.max(
    SESSION_THREAD_ASK_USER_CARD_MIN_WIDTH_PX,
    Math.min(
      resolveSessionThreadIndentedContentWidth(viewportWidth),
      SESSION_THREAD_ASK_USER_CARD_MAX_WIDTH_PX,
    ),
  );
}

export function resolveSessionMarkdownListMarkerColumnWidthPx(
  markerTexts: readonly string[],
): number {
  const maxTextLength = markerTexts.reduce(
    (max, text) => Math.max(max, String(text ?? "").length),
    0,
  );
  return Math.max(
    SESSION_THREAD_MARKDOWN_LIST_MARKER_MIN_WIDTH_PX,
    Math.ceil(maxTextLength * SESSION_THREAD_MARKDOWN_LIST_MARKER_ADVANCE_PX),
  );
}

export const SESSION_THREAD_THOUGHT_HORIZONTAL_CHROME_PX =
  resolveSessionThreadThoughtHorizontalChromePx(spec);
export const SESSION_THREAD_THOUGHT_VERTICAL_CHROME_PX =
  resolveSessionThreadThoughtVerticalChromePx(spec);
export const SESSION_THREAD_TOOL_SUMMARY_ROW_HEIGHT_PX =
  resolveSessionThreadToolSummaryRowHeightPx(spec);
export const SESSION_THREAD_TOOL_THOUGHT_TITLE_HEIGHT_PX =
  resolveSessionThreadToolThoughtTitleHeightPx(spec);
export const SESSION_THREAD_TOOL_THOUGHT_BODY_CHROME_WIDTH_PX =
  resolveSessionThreadToolThoughtBodyChromeWidthPx(spec);
export const SESSION_THREAD_TOOL_THOUGHT_BODY_CHROME_HEIGHT_PX =
  resolveSessionThreadToolThoughtBodyChromeHeightPx(spec);
