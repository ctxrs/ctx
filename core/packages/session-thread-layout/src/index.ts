export type {
  AskUserQuestionAnswerState,
  AskUserQuestionThreadItem,
  ScrollbarDragState,
  ThreadAssistantItem,
  ThreadItem,
  ThreadMessageItem,
  ThreadSpacerItem,
  ThreadThoughtItem,
  ThreadToolGroupItem,
  ThreadToolItem,
  ThreadTurnStatusItem,
  WorkbenchListItem,
  WorkbenchThreadView,
  WorkbenchTurnHeader,
} from "./transcriptTypes";

export { stripCitationMarkers } from "./citationMarkers";
export {
  type FileRef,
  type UrlRef,
  isAbsolutePath,
  parseFileRefToken,
  parseUrlToken,
  splitWhitespaceTokens,
} from "./codeTokenLinks";
export { isSealedInlineCodeFragment, splitInlineCodeFragments } from "./inlineCodeFragments";
export { markdownToPlainText } from "./markdownPlainText";
export {
  addPretextPerfBucket,
  hashPretextPerfValue,
  incrementPretextPerfCounter,
  initPretextPerfDiagnostics,
  isPretextPerfDiagnosticsEnabled,
  readPretextPerfQueryFlag,
  recordPretextPerfEvent,
  resetPretextPerfDiagnostics,
} from "./pretextPerfDiagnostics";
export type {
  PretextVirtualizerMeasuredHeight,
  PretextVirtualizerMeasurementHooks,
  PretextVirtualizerMessageLayout,
  PretextVirtualizerRowLayoutContext,
  PretextVirtualizerRowMeasurementRequest,
  PretextVirtualizerTextMeasurementRequest,
} from "./pretextVirtualizerRowLayout";
export {
  clearPretextVirtualizerRowLayoutCache,
  getPretextVirtualizerRowLayout,
} from "./pretextVirtualizerRowLayout";
export type {
  TranscriptLayoutPlanner,
  TranscriptLayoutPlannerProfileId,
  TranscriptRowPlan,
} from "./transcriptLayoutPlanner";
export {
  clearTranscriptLayoutPlannerCaches,
  defaultTranscriptLayoutPlanner,
  getTranscriptRowPlannedLayout,
  planTranscriptRowLayout,
  planTranscriptRows,
} from "./transcriptLayoutPlanner";
export { getWorkbenchTurnHeaderDisplayPlainText, normalizeTurnHeaderPlainText } from "./transcriptPlainText";
export type { WorkbenchListItemExpansionState, WorkbenchMessageCollapseState, WorkbenchMessageRenderMode, WorkbenchTurnHeaderTextState } from "./transcriptRowLayoutModel";
export {
  canCollapseMessageContent,
  getCollapsedMessageContent,
  getWorkbenchListItemLayoutState,
  getWorkbenchMessageCollapseState,
  getWorkbenchMessageLayoutState,
  getWorkbenchTurnHeaderDisplayPlainText as getWorkbenchTurnHeaderDisplayPlainTextFromLayoutModel,
  getWorkbenchTurnHeaderLayoutState,
  getWorkbenchTurnHeaderTextState,
  isExpandableMessageContent,
  isExpandableTurnHeaderPlainText,
  resolveWorkbenchMessageExpandedFromContent,
  resolveWorkbenchTurnHeaderExpandedFromPlainText,
} from "./transcriptRowLayoutModel";

export type { SessionThreadGeometrySpec } from "./sessionThreadGeometrySpec";
export {
  SESSION_THREAD_GEOMETRY_REVISION,
  SESSION_THREAD_GEOMETRY_SPEC,
  getSessionThreadGeometryRevision,
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
export {
  SESSION_MARKDOWN_MEASUREMENT_CONTRACT,
  SESSION_THREAD_MEASUREMENT_GEOMETRY_REVISION,
  SESSION_THREAD_ROW_MEASUREMENT_CONTRACT,
} from "./sessionThreadMeasurementContract";
export {
  SESSION_THREAD_ASSISTANT_ENTRY_PADDING_INLINE_PX,
  SESSION_THREAD_ASK_USER_ACTIONS_HEIGHT_PX,
  SESSION_THREAD_ASK_USER_CARD_GAP_PX,
  SESSION_THREAD_ASK_USER_CARD_MAX_WIDTH_PX,
  SESSION_THREAD_ASK_USER_CARD_MIN_WIDTH_PX,
  SESSION_THREAD_ASK_USER_CARD_PADDING_PX,
  SESSION_THREAD_ASK_USER_HINT_HEIGHT_PX,
  SESSION_THREAD_ASK_USER_MARGIN_VERTICAL_PX,
  SESSION_THREAD_ASK_USER_PANEL_HEIGHT_PX,
  SESSION_THREAD_ASK_USER_SHELL_HEIGHT_PX,
  SESSION_THREAD_ASK_USER_STATUS_HEIGHT_PX,
  SESSION_THREAD_ASK_USER_TABS_HEIGHT_PX,
  SESSION_THREAD_CONTENT_MAX_WIDTH_PX,
  SESSION_THREAD_HORIZONTAL_INSET_PX,
  SESSION_THREAD_INDENT_LEFT_PX,
  SESSION_THREAD_MARKDOWN_BLOCKQUOTE_BORDER_WIDTH_PX,
  SESSION_THREAD_MARKDOWN_BLOCKQUOTE_INSET_PX,
  SESSION_THREAD_MARKDOWN_BLOCKQUOTE_PADDING_INLINE_START_PX,
  SESSION_THREAD_MARKDOWN_BLOCK_MARGIN_BOTTOM_PX,
  SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY,
  SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX,
  SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
  SESSION_THREAD_MARKDOWN_CODE_BLOCK_BORDER_WIDTH_PX,
  SESSION_THREAD_MARKDOWN_CODE_BLOCK_FONT_SIZE_PX,
  SESSION_THREAD_MARKDOWN_CODE_BLOCK_LINE_HEIGHT_PX,
  SESSION_THREAD_MARKDOWN_CODE_BLOCK_PADDING_BOTTOM_PX,
  SESSION_THREAD_MARKDOWN_CODE_BLOCK_PADDING_TOP_PX,
  SESSION_THREAD_MARKDOWN_FONT_WEIGHT,
  SESSION_THREAD_MARKDOWN_HEADING_FONT_SIZE_PX_BY_DEPTH,
  SESSION_THREAD_MARKDOWN_HEADING_LINE_HEIGHT_PX_BY_DEPTH,
  SESSION_THREAD_MARKDOWN_HEADING_MARGIN_BOTTOM_PX,
  SESSION_THREAD_MARKDOWN_HEADING_MARGIN_TOP_PX,
  SESSION_THREAD_MARKDOWN_IMAGE_HEIGHT_PX,
  SESSION_THREAD_MARKDOWN_IMAGE_WIDTH_PX,
  SESSION_THREAD_MARKDOWN_INLINE_CODE_BORDER_RADIUS_PX,
  SESSION_THREAD_MARKDOWN_INLINE_CODE_BORDER_WIDTH_PX,
  SESSION_THREAD_MARKDOWN_INLINE_CODE_EDGE_BLOCK_PX,
  SESSION_THREAD_MARKDOWN_INLINE_CODE_EDGE_PX,
  SESSION_THREAD_MARKDOWN_INLINE_CODE_FONT_FAMILY,
  SESSION_THREAD_MARKDOWN_INLINE_CODE_FONT_SIZE_PX,
  SESSION_THREAD_MARKDOWN_INLINE_CODE_FRAGMENT_CHROME_HEIGHT_PX,
  SESSION_THREAD_MARKDOWN_INLINE_CODE_FRAGMENT_CHROME_WIDTH_PX,
  SESSION_THREAD_MARKDOWN_INLINE_CODE_PADDING_BLOCK_PX,
  SESSION_THREAD_MARKDOWN_INLINE_CODE_PADDING_INLINE_PX,
  SESSION_THREAD_MARKDOWN_LIST_GAP_PX,
  SESSION_THREAD_MARKDOWN_LIST_INDENT_PX,
  SESSION_THREAD_MARKDOWN_LIST_MARKER_ADVANCE_PX,
  SESSION_THREAD_MARKDOWN_LIST_MARKER_GAP_PX,
  SESSION_THREAD_MARKDOWN_LIST_MARKER_MIN_WIDTH_PX,
  SESSION_THREAD_MARKDOWN_TABLE_BORDER_WIDTH_PX,
  SESSION_THREAD_MARKDOWN_TABLE_CELL_PADDING_BLOCK_PX,
  SESSION_THREAD_MARKDOWN_TABLE_CELL_PADDING_INLINE_PX,
  SESSION_THREAD_MESSAGE_ATTACHMENT_GAP_PX,
  SESSION_THREAD_MESSAGE_ATTACHMENT_HEIGHT_PX,
  SESSION_THREAD_MESSAGE_ATTACHMENT_MARGIN_TOP_PX,
  SESSION_THREAD_MESSAGE_ATTACHMENT_WIDTH_PX,
  SESSION_THREAD_MESSAGE_BUBBLE_BORDER_WIDTH_PX,
  SESSION_THREAD_MESSAGE_BUBBLE_PADDING_BLOCK_PX,
  SESSION_THREAD_MESSAGE_BUBBLE_PADDING_INLINE_PX,
  SESSION_THREAD_MESSAGE_MAX_WIDTH_RATIO,
  SESSION_THREAD_MESSAGE_ROLE_FONT_SIZE_PX,
  SESSION_THREAD_MESSAGE_ROLE_LINE_HEIGHT_PX,
  SESSION_THREAD_MESSAGE_ROW_PADDING_BLOCK_PX,
  SESSION_THREAD_MESSAGE_TOGGLE_FONT_SIZE_PX,
  SESSION_THREAD_MESSAGE_TOGGLE_LINE_HEIGHT_PX,
  SESSION_THREAD_MESSAGE_TOGGLE_MARGIN_TOP_PX,
  SESSION_THREAD_ROW_MAX_WIDTH_PX,
  SESSION_THREAD_THOUGHT_FONT_FAMILY,
  SESSION_THREAD_THOUGHT_FONT_SIZE_PX,
  SESSION_THREAD_THOUGHT_LINE_HEIGHT_PX,
  SESSION_THREAD_THOUGHT_PADDING_BLOCK_PX,
  SESSION_THREAD_THOUGHT_PADDING_INLINE_PX,
  SESSION_THREAD_TOOL_GROUP_GAP_PX,
  SESSION_THREAD_TOOL_ITEM_GAP_PX,
  SESSION_THREAD_TOOL_LOADING_FONT_FAMILY,
  SESSION_THREAD_TOOL_LOADING_FONT_SIZE_PX,
  SESSION_THREAD_TOOL_LOADING_LINE_HEIGHT_PX,
  SESSION_THREAD_TOOL_SEPARATOR_PADDING_INLINE_PX,
  SESSION_THREAD_TOOL_STATUS_DOT_PADDING_INLINE_PX,
  SESSION_THREAD_TOOL_SUMMARY_FONT_FAMILY,
  SESSION_THREAD_TOOL_SUMMARY_FONT_SIZE_PX,
  SESSION_THREAD_TOOL_SUMMARY_LINE_HEIGHT_PX,
  SESSION_THREAD_TOOL_SUMMARY_PADDING_BLOCK_PX,
  SESSION_THREAD_TOOL_SUMMARY_PADDING_INLINE_PX,
  SESSION_THREAD_TOOL_THOUGHT_BODY_BORDER_WIDTH_PX,
  SESSION_THREAD_TOOL_THOUGHT_BODY_FONT_FAMILY,
  SESSION_THREAD_TOOL_THOUGHT_BODY_FONT_SIZE_PX,
  SESSION_THREAD_TOOL_THOUGHT_BODY_LINE_HEIGHT_PX,
  SESSION_THREAD_TOOL_THOUGHT_BODY_PADDING_PX,
  SESSION_THREAD_TOOL_THOUGHT_TITLE_FONT_FAMILY,
  SESSION_THREAD_TOOL_THOUGHT_TITLE_FONT_SIZE_PX,
  SESSION_THREAD_TOOL_THOUGHT_TITLE_LINE_HEIGHT_PX,
  SESSION_THREAD_TOOL_THOUGHT_TITLE_MARGIN_BOTTOM_PX,
  SESSION_THREAD_TURN_HEADER_BUBBLE_BORDER_WIDTH_PX,
  SESSION_THREAD_TURN_HEADER_BUBBLE_PADDING_BLOCK_PX,
  SESSION_THREAD_TURN_HEADER_BUBBLE_PADDING_INLINE_PX,
  SESSION_THREAD_TURN_HEADER_COLLAPSED_MAX_HEIGHT_PX,
  SESSION_THREAD_TURN_HEADER_COPY_GUTTER_PX,
  resolveSessionMarkdownListMarkerColumnWidthPx,
  resolveSessionThreadAskUserCardWidth,
  resolveSessionThreadAssistantTextWidth,
  resolveSessionThreadContentWidth,
  resolveSessionThreadIndentedContentWidth,
  resolveSessionThreadMessageBubbleBorderBoxWidth,
  resolveSessionThreadMessageTextWidth,
  resolveSessionThreadRowWidth,
  resolveSessionThreadTurnHeaderTextWidth,
} from "./sessionThreadLayoutTokens";

export {
  SESSION_TRANSCRIPT_LAYOUT_ENGINE_REVISION,
  clearSessionMarkdownMeasurementCaches,
  measureSessionMarkdownDocument,
} from "./sessionMarkdownMeasurement";
export {
  shouldUseExactRenderedPlainTextMeasurement,
  shouldUseRenderedAssistantRowMeasurement,
  shouldUseRenderedMarkdownMeasurement,
} from "./sessionMeasurementPolicy";
export type { SessionMarkdownNode } from "./sessionMarkdownShared";
export {
  parseSessionMarkdown,
  readMarkdownChecked,
  readMarkdownDepth,
  readMarkdownOrdered,
  readMarkdownStart,
} from "./sessionMarkdownShared";
export type {
  SessionMarkdownBlock,
  SessionMarkdownBlockContext,
  SessionMarkdownBlockquoteBlock,
  SessionMarkdownCodeBlock,
  SessionMarkdownDocument,
  SessionMarkdownHeadingBlock,
  SessionMarkdownImageBlock,
  SessionMarkdownInlineNode,
  SessionMarkdownInlineRun,
  SessionMarkdownListBlock,
  SessionMarkdownListItem,
  SessionMarkdownParagraphBlock,
  SessionMarkdownTableCell,
  SessionMarkdownTableBlock,
  SessionMarkdownTableRow,
  SessionMarkdownTextContent,
  SessionMarkdownTextRunStyle,
  SessionMarkdownThematicBreakBlock,
} from "./sessionMarkdownContract";
export {
  createSessionMarkdownDocument,
  nodeChildren,
  normalizeSessionMarkdownBlocks,
  resolveSessionMarkdownBlockEntryGapPx,
  resolveSessionMarkdownBlockGapPx,
} from "./sessionMarkdownContract";
export type { InlineCodeBoundaryFit, InlineCodeTrailingPlainInfo } from "./sessionMarkdownInlineCodeFit";
export {
  INLINE_CODE_DOTTED_CALL_CONTINUATION_MIN_SPARE_PX,
  INLINE_CODE_PATH_DELIMITER_CONTINUATION_MIN_SPARE_PX,
  allowsChromiumDottedBoundaryHang,
  createInlineCodeFitPlanner,
  resolveInlineCodeContinuationFitSlackPx,
  resolveInlineCodeProseStartSeamGuardPx,
  resolveInlineCodeWhitespaceSeparatedFragmentSlackPx,
  resolveInlineCodeWrapChromeWidth,
  shouldApplyInlineCodeSoftBreakTextStartGuard,
  shouldBreakBeforePartialDottedCallContinuation,
  shouldBreakBeforePathDelimiterNearFitContinuation,
  shouldBreakBeforeWhitespaceSeparatedInlineCodeFragment,
} from "./sessionMarkdownInlineCodeFit";
export type { InlineWrapMode, PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
export { prepareInlineLayoutItems } from "./sessionMarkdownInlineLayout";
export { measureInlineRunsHeight } from "./sessionMarkdownInlineMeasurement";
export type { SessionMarkdownInlineWrapBrowserProfile } from "./sessionMarkdownBrowserProfile";
export {
  browserAllowsInlineCodeLeadingHang,
  getSessionMarkdownInlineWrapBrowserProfile,
} from "./sessionMarkdownBrowserProfile";
export type {
  SessionMarkdownDebugWindow,
  TextBlockTypography,
  TextWhiteSpace,
} from "./sessionMarkdownMeasurementCore";
export {
  BODY_LINE_HEIGHT_PX,
  BODY_TYPOGRAPHY,
  CODE_BLOCK_VERTICAL_PADDING_PX,
  LINE_START_CURSOR,
  MONO_FONT,
  MONO_LINE_HEIGHT_PX,
  TABLE_HEADER_TYPOGRAPHY,
  buildHeadingTypography,
  clearSessionMarkdownMeasurementCaches as clearSessionMarkdownMeasurementCoreCaches,
  cursorsMatch,
  measureInlineSpaceWidth,
  parseMarkdown,
  resolveTextRunFont,
} from "./sessionMarkdownMeasurementCore";
export {
  buildPreparedContentKey,
  clearSessionTextMeasurementCaches,
  clampHeight,
  getPreparedText,
  getPreparedTextWithSegments,
  measureCollapsedSpaceWidth,
  measureSessionTextHeight,
  measureSingleLineLayout,
  measureTextHeight,
  normalizeHeight,
  pruneCache,
  segmentGraphemes,
  segmentImplicitWordBreaks,
  segmentWords,
} from "./sessionTextMeasurement";
export {
  containsStrongRtlText,
  isCompactPathTailContinuationAnchor,
  isCompactSlashDelimitedSeamAnchor,
  isHyphenatedTextBreakToken,
  isPathLikeOrDottedText,
  isPunctuationOnlySeamText,
  splitHyphenatedTextBreakToken,
} from "./sessionTextTokenClassifier";
export {
  clearSessionPlainTextMeasurementCaches,
  measureSessionPlainTextBlockHeight,
} from "./sessionPlainTextMeasurement";
export type { TranscriptMeasuredMessageLayout } from "./sessionTranscriptTextMeasurement";
export {
  clearSessionTranscriptTextMeasurementCaches,
  measureAssistantMarkdownTextHeight,
  measureMessageLayoutTextHeight,
  measureTurnHeaderPreviewTextHeight,
} from "./sessionTranscriptTextMeasurement";
export type {
  SessionMarkdownInlineCodeContinuationDecision,
  SessionMarkdownInlineCodeDebugPayload,
  SessionMarkdownInlineCodeSealedContinuationDecision,
  SessionMarkdownInlineCodeSegmentSeamAdjustment,
  SessionMarkdownInlineCodeStartDecision,
  SessionMarkdownInlineCodeWhitespaceDecision,
} from "./sessionMarkdownInlineMeasurementDebug";
export {
  createSessionMarkdownInlineMeasurementDebugProbe,
  serializeSessionMarkdownInlineMeasurementItems,
  shouldEnableSessionMarkdownInlineCodeDebug,
} from "./sessionMarkdownInlineMeasurementDebug";
export type { PretextWrapRuleEntry } from "./testdata/pretextWrapRuleCatalog";
export { PRETEXT_WRAP_RULE_CATALOG, getPretextWrapRuleById } from "./testdata/pretextWrapRuleCatalog";
