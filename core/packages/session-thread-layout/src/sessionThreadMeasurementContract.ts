import {
  SESSION_THREAD_GEOMETRY_REVISION,
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
const geometryRevision = SESSION_THREAD_GEOMETRY_REVISION;
const contentMaxWidthPx = resolveSessionThreadContentMaxWidthPx(spec);
const inlineCodeEdgeBlockPx = resolveSessionThreadMarkdownInlineCodeEdgeBlockPx(spec);
const inlineCodeEdgeInlinePx = resolveSessionThreadMarkdownInlineCodeEdgeInlinePx(spec);
const inlineCodeFragmentChromeHeightPx =
  resolveSessionThreadMarkdownInlineCodeFragmentChromeHeightPx(spec);
const inlineCodeFragmentChromeWidthPx =
  resolveSessionThreadMarkdownInlineCodeFragmentChromeWidthPx(spec);
const markdownBlockquoteInsetPx = resolveSessionThreadMarkdownBlockquoteInsetPx(spec);
const askUserShellHeightPx = resolveSessionThreadAskUserShellHeightPx(spec);
const thoughtHorizontalChromePx = resolveSessionThreadThoughtHorizontalChromePx(spec);
const thoughtVerticalChromePx = resolveSessionThreadThoughtVerticalChromePx(spec);
const toolSummaryRowHeightPx = resolveSessionThreadToolSummaryRowHeightPx(spec);
const toolThoughtTitleHeightPx = resolveSessionThreadToolThoughtTitleHeightPx(spec);
const toolThoughtBodyChromeWidthPx = resolveSessionThreadToolThoughtBodyChromeWidthPx(spec);
const toolThoughtBodyChromeHeightPx = resolveSessionThreadToolThoughtBodyChromeHeightPx(spec);

export const SESSION_THREAD_MEASUREMENT_GEOMETRY_REVISION = geometryRevision;

export const SESSION_MARKDOWN_MEASUREMENT_CONTRACT = {
  geometryRevision,
  typography: {
    bodyFontFamily: spec.markdown.typography.bodyFontFamily,
    bodyFontSizePx: spec.markdown.typography.bodyFontSizePx,
    bodyLineHeightPx: spec.markdown.typography.bodyLineHeightPx,
    headingFontSizePxByDepth: spec.markdown.typography.headingFontSizePxByDepth,
    inlineCodeFontFamily: spec.markdown.typography.inlineCodeFontFamily,
    inlineCodeFontSizePx: spec.markdown.typography.inlineCodeFontSizePx,
    codeBlockFontSizePx: spec.markdown.typography.codeBlockFontSizePx,
    codeBlockLineHeightPx: spec.markdown.typography.codeBlockLineHeightPx,
    headingLineHeightPxByDepth: spec.markdown.typography.headingLineHeightPxByDepth,
    fontWeight: spec.markdown.typography.fontWeight,
  },
  inlineCode: {
    paddingBlockPx: spec.markdown.inlineCode.paddingBlockPx,
    paddingInlinePx: spec.markdown.inlineCode.paddingInlinePx,
    borderWidthPx: spec.markdown.inlineCode.borderWidthPx,
    borderRadiusPx: spec.markdown.inlineCode.borderRadiusPx,
    edgeBlockPx: inlineCodeEdgeBlockPx,
    edgeInlinePx: inlineCodeEdgeInlinePx,
    fragmentChromeHeightPx: inlineCodeFragmentChromeHeightPx,
    fragmentChromeWidthPx: inlineCodeFragmentChromeWidthPx,
  },
  blockSpacing: {
    blockMarginBottomPx: spec.markdown.blockSpacing.blockMarginBottomPx,
    headingMarginTopPx: spec.markdown.blockSpacing.headingMarginTopPx,
    headingMarginBottomPx: spec.markdown.blockSpacing.headingMarginBottomPx,
    entryGapPxByContext: spec.markdown.blockSpacing.entryGapPxByContext,
    exitGapPxByContext: spec.markdown.blockSpacing.exitGapPxByContext,
  },
  list: {
    indentPx: spec.markdown.list.indentPx,
    gapPx: spec.markdown.list.gapPx,
    markerMinWidthPx: spec.markdown.list.markerMinWidthPx,
    markerGapPx: spec.markdown.list.markerGapPx,
    markerAdvancePx: spec.markdown.list.markerAdvancePx,
    checkboxGutterPx: spec.markdown.list.checkboxGutterPx,
  },
  blockquote: {
    borderWidthPx: spec.markdown.blockquote.borderWidthPx,
    paddingInlineStartPx: spec.markdown.blockquote.paddingInlineStartPx,
    insetPx: markdownBlockquoteInsetPx,
  },
  codeBlock: {
    borderWidthPx: spec.markdown.codeBlock.borderWidthPx,
    paddingTopPx: spec.markdown.codeBlock.paddingTopPx,
    paddingBottomPx: spec.markdown.codeBlock.paddingBottomPx,
  },
  image: {
    widthPx: spec.markdown.image.widthPx,
    heightPx: spec.markdown.image.heightPx,
  },
  table: {
    borderWidthPx: spec.markdown.table.borderWidthPx,
    cellPaddingBlockPx: spec.markdown.table.cellPaddingBlockPx,
    cellPaddingInlinePx: spec.markdown.table.cellPaddingInlinePx,
  },
} as const;

export const SESSION_THREAD_ROW_MEASUREMENT_CONTRACT = {
  geometryRevision,
  viewport: {
    rowMaxWidthPx: spec.viewport.rowMaxWidthPx,
    contentMaxWidthPx,
    horizontalInsetPx: spec.viewport.horizontalInsetPx,
    indentLeftPx: spec.viewport.indentLeftPx,
  },
  assistant: {
    entryPaddingInlinePx: spec.rows.assistant.entryPaddingInlinePx,
    verticalPaddingPx: spec.rows.assistant.verticalPaddingPx,
  },
  message: {
    rowPaddingBlockPx: spec.rows.message.rowPaddingBlockPx,
    bubblePaddingBlockPx: spec.rows.message.bubblePaddingBlockPx,
    bubblePaddingInlinePx: spec.rows.message.bubblePaddingInlinePx,
    bubbleBorderWidthPx: spec.rows.message.bubbleBorderWidthPx,
    maxWidthRatio: spec.rows.message.maxWidthRatio,
    roleFontSizePx: spec.rows.message.roleFontSizePx,
    roleLineHeightPx: spec.rows.message.roleLineHeightPx,
    toggleMarginTopPx: spec.rows.message.toggleMarginTopPx,
    toggleFontSizePx: spec.rows.message.toggleFontSizePx,
    toggleLineHeightPx: spec.rows.message.toggleLineHeightPx,
    attachments: {
      widthPx: spec.rows.message.attachments.widthPx,
      heightPx: spec.rows.message.attachments.heightPx,
      gapPx: spec.rows.message.attachments.gapPx,
      marginTopPx: spec.rows.message.attachments.marginTopPx,
    },
  },
  turnHeader: {
    bubblePaddingBlockPx: spec.rows.turnHeader.bubblePaddingBlockPx,
    bubblePaddingInlinePx: spec.rows.turnHeader.bubblePaddingInlinePx,
    bubbleBorderWidthPx: spec.rows.turnHeader.bubbleBorderWidthPx,
    collapsedMaxHeightPx: spec.rows.turnHeader.collapsedMaxHeightPx,
    copyGutterPx: spec.rows.turnHeader.copyGutterPx,
    outerVerticalPx: spec.rows.turnHeader.outerVerticalPx,
    attachments: {
      sizePx: spec.rows.turnHeader.attachments.sizePx,
      gapPx: spec.rows.turnHeader.attachments.gapPx,
      marginTopPx: spec.rows.turnHeader.attachments.marginTopPx,
    },
  },
  askUser: {
    marginVerticalPx: spec.rows.askUser.marginVerticalPx,
    cardMinWidthPx: spec.rows.askUser.cardMinWidthPx,
    cardMaxWidthPx: spec.rows.askUser.cardMaxWidthPx,
    cardPaddingPx: spec.rows.askUser.cardPaddingPx,
    cardGapPx: spec.rows.askUser.cardGapPx,
    tabsHeightPx: spec.rows.askUser.tabsHeightPx,
    panelHeightPx: spec.rows.askUser.panelHeightPx,
    statusHeightPx: spec.rows.askUser.statusHeightPx,
    actionsHeightPx: spec.rows.askUser.actionsHeightPx,
    hintHeightPx: spec.rows.askUser.hintHeightPx,
    shellHeightPx: askUserShellHeightPx,
    outerHeightPx: spec.rows.askUser.marginVerticalPx + askUserShellHeightPx,
  },
  thought: {
    paddingInlinePx: spec.rows.thought.paddingInlinePx,
    paddingBlockPx: spec.rows.thought.paddingBlockPx,
    horizontalChromePx: thoughtHorizontalChromePx,
    verticalChromePx: thoughtVerticalChromePx,
    typography: spec.rows.thought.typography,
  },
  tools: {
    itemGapPx: spec.rows.tools.itemGapPx,
    groupGapPx: spec.rows.tools.groupGapPx,
    summary: {
      rowHeightPx: toolSummaryRowHeightPx,
      paddingInlinePx: spec.rows.tools.summary.paddingInlinePx,
      paddingBlockPx: spec.rows.tools.summary.paddingBlockPx,
      separatorPaddingInlinePx: spec.rows.tools.summary.separatorPaddingInlinePx,
      statusDotPaddingInlinePx: spec.rows.tools.summary.statusDotPaddingInlinePx,
      typography: spec.rows.tools.summary.typography,
    },
    loading: {
      typography: spec.rows.tools.loading.typography,
    },
    thoughtTitle: {
      heightPx: toolThoughtTitleHeightPx,
      marginBottomPx: spec.rows.tools.thoughtTitle.marginBottomPx,
      typography: spec.rows.tools.thoughtTitle.typography,
    },
    thoughtBody: {
      paddingPx: spec.rows.tools.thoughtBody.paddingPx,
      borderWidthPx: spec.rows.tools.thoughtBody.borderWidthPx,
      chromeWidthPx: toolThoughtBodyChromeWidthPx,
      chromeHeightPx: toolThoughtBodyChromeHeightPx,
      typography: spec.rows.tools.thoughtBody.typography,
    },
  },
  fixed: {
    spacerHeightPx: spec.rows.fixed.spacerHeightPx,
    turnStatusHeightPx: spec.rows.fixed.turnStatusHeightPx,
  },
} as const;
