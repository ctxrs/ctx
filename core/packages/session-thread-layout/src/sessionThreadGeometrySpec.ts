export type SessionThreadHeadingDepth = 1 | 2 | 3 | 4;
export type SessionThreadMarkdownBlockKind =
  | "paragraph"
  | "image"
  | "heading"
  | "list"
  | "blockquote"
  | "code"
  | "table"
  | "thematicBreak";
export type SessionThreadFontStyle = "normal" | "italic";
export type SessionThreadTypographySpec = {
  fontFamily: string;
  fontSizePx: number;
  lineHeightPx: number;
  fontStyle?: SessionThreadFontStyle;
  fontWeight?: number;
};

export type SessionThreadGeometrySpec = {
  viewport: {
    rowMaxWidthPx: number;
    horizontalInsetPx: number;
    indentLeftPx: number;
  };
  markdown: {
    typography: {
      bodyFontSizePx: number;
      bodyLineHeightPx: number;
      bodyFontFamily: string;
      headingFontSizePxByDepth: Readonly<Record<SessionThreadHeadingDepth, number>>;
      headingLineHeightPxByDepth: Readonly<Record<SessionThreadHeadingDepth, number>>;
      inlineCodeFontSizePx: number;
      inlineCodeFontFamily: string;
      codeBlockFontSizePx: number;
      codeBlockLineHeightPx: number;
      fontWeight: {
        bodyStrong: number;
        heading: number;
        headingStrong: number;
        tableHeader: number;
        tableHeaderStrong: number;
      };
    };
    blockSpacing: {
      blockMarginBottomPx: number;
      headingMarginTopPx: number;
      headingMarginBottomPx: number;
      entryGapPxByContext: Readonly<
        Record<"root" | "listItem", Readonly<Record<SessionThreadMarkdownBlockKind, number>>>
      >;
      exitGapPxByContext: Readonly<
        Record<"root" | "listItem", Readonly<Record<SessionThreadMarkdownBlockKind, number>>>
      >;
    };
    list: {
      indentPx: number;
      gapPx: number;
      markerMinWidthPx: number;
      markerGapPx: number;
      markerAdvancePx: number;
      checkboxGutterPx: number;
    };
    inlineCode: {
      paddingBlockPx: number;
      paddingInlinePx: number;
      borderWidthPx: number;
      borderRadiusPx: number;
    };
    blockquote: {
      borderWidthPx: number;
      paddingInlineStartPx: number;
    };
    image: {
      widthPx: number;
      heightPx: number;
    };
    codeBlock: {
      borderWidthPx: number;
      paddingTopPx: number;
      paddingBottomPx: number;
    };
    table: {
      borderWidthPx: number;
      cellPaddingBlockPx: number;
      cellPaddingInlinePx: number;
    };
  };
  rows: {
    message: {
      rowPaddingBlockPx: number;
      bubblePaddingBlockPx: number;
      bubblePaddingInlinePx: number;
      bubbleBorderWidthPx: number;
      maxWidthRatio: number;
      roleFontSizePx: number;
      roleLineHeightPx: number;
      toggleMarginTopPx: number;
      toggleFontSizePx: number;
      toggleLineHeightPx: number;
      attachments: {
        widthPx: number;
        heightPx: number;
        gapPx: number;
        marginTopPx: number;
      };
    };
    assistant: {
      entryPaddingInlinePx: number;
      verticalPaddingPx: number;
    };
    turnHeader: {
      bubblePaddingBlockPx: number;
      bubblePaddingInlinePx: number;
      bubbleBorderWidthPx: number;
      copyGutterPx: number;
      collapsedMaxHeightPx: number;
      outerVerticalPx: number;
      attachments: {
        sizePx: number;
        gapPx: number;
        marginTopPx: number;
      };
    };
    askUser: {
      marginVerticalPx: number;
      cardMaxWidthPx: number;
      cardMinWidthPx: number;
      cardPaddingPx: number;
      cardGapPx: number;
      tabsHeightPx: number;
      panelHeightPx: number;
      statusHeightPx: number;
      actionsHeightPx: number;
      hintHeightPx: number;
    };
    thought: {
      paddingInlinePx: number;
      paddingBlockPx: number;
      typography: SessionThreadTypographySpec & {
        fontStyle: SessionThreadFontStyle;
      };
    };
    tools: {
      itemGapPx: number;
      groupGapPx: number;
      summary: {
        paddingInlinePx: number;
        paddingBlockPx: number;
        typography: SessionThreadTypographySpec & {
          fontWeight: number;
        };
        separatorPaddingInlinePx: number;
        statusDotPaddingInlinePx: number;
      };
      loading: {
        typography: SessionThreadTypographySpec;
      };
      thoughtTitle: {
        typography: SessionThreadTypographySpec;
        marginBottomPx: number;
      };
      thoughtBody: {
        paddingPx: number;
        borderWidthPx: number;
        typography: SessionThreadTypographySpec;
      };
    };
    fixed: {
      spacerHeightPx: number;
      turnStatusHeightPx: number;
    };
  };
};

export const SESSION_THREAD_GEOMETRY_SPEC: SessionThreadGeometrySpec = {
  viewport: {
    rowMaxWidthPx: 820,
    horizontalInsetPx: 12,
    indentLeftPx: 4,
  },
  markdown: {
    typography: {
      bodyFontSizePx: 13,
      bodyLineHeightPx: 21,
      bodyFontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
      headingFontSizePxByDepth: {
        1: 18,
        2: 16,
        3: 14,
        4: 13,
      },
      headingLineHeightPxByDepth: {
        1: 22,
        2: 20,
        3: 18,
        4: 16,
      },
      inlineCodeFontSizePx: 13,
      inlineCodeFontFamily: '"SF Mono", Menlo, Monaco, Consolas, "Courier New", monospace',
      codeBlockFontSizePx: 12,
      codeBlockLineHeightPx: 17,
      fontWeight: {
        bodyStrong: 700,
        heading: 600,
        headingStrong: 700,
        tableHeader: 600,
        tableHeaderStrong: 700,
      },
    },
    blockSpacing: {
      blockMarginBottomPx: 12,
      headingMarginTopPx: 16,
      headingMarginBottomPx: 8,
      entryGapPxByContext: {
        root: {
          paragraph: 0,
          image: 0,
          heading: 16,
          list: 0,
          blockquote: 0,
          code: 10,
          table: 0,
          thematicBreak: 0,
        },
        listItem: {
          paragraph: 0,
          image: 0,
          heading: 16,
          list: 0,
          blockquote: 0,
          code: 10,
          table: 0,
          thematicBreak: 0,
        },
      },
      exitGapPxByContext: {
        root: {
          paragraph: 12,
          image: 12,
          heading: 8,
          list: 12,
          blockquote: 12,
          code: 10,
          table: 12,
          thematicBreak: 12,
        },
        listItem: {
          paragraph: 0,
          image: 0,
          heading: 8,
          list: 12,
          blockquote: 12,
          code: 10,
          table: 12,
          thematicBreak: 12,
        },
      },
    },
    list: {
      indentPx: 16.25,
      gapPx: 4,
      markerMinWidthPx: 18,
      markerGapPx: 6,
      markerAdvancePx: 7.25,
      checkboxGutterPx: 18,
    },
    inlineCode: {
      paddingBlockPx: 1,
      paddingInlinePx: 6,
      borderWidthPx: 1,
      borderRadiusPx: 6,
    },
    blockquote: {
      borderWidthPx: 2,
      paddingInlineStartPx: 12,
    },
    image: {
      widthPx: 240,
      heightPx: 180,
    },
    codeBlock: {
      borderWidthPx: 1,
      paddingTopPx: 28,
      paddingBottomPx: 12,
    },
    table: {
      borderWidthPx: 1,
      cellPaddingBlockPx: 8,
      cellPaddingInlinePx: 12,
    },
  },
  rows: {
    message: {
      rowPaddingBlockPx: 6,
      bubblePaddingBlockPx: 10,
      bubblePaddingInlinePx: 12,
      bubbleBorderWidthPx: 1,
      maxWidthRatio: 0.92,
      roleFontSizePx: 11,
      roleLineHeightPx: 11,
      toggleMarginTopPx: 6,
      toggleFontSizePx: 13,
      toggleLineHeightPx: 16,
      attachments: {
        widthPx: 240,
        heightPx: 180,
        gapPx: 8,
        marginTopPx: 8,
      },
    },
    assistant: {
      entryPaddingInlinePx: 2,
      verticalPaddingPx: 20,
    },
    turnHeader: {
      bubblePaddingBlockPx: 8,
      bubblePaddingInlinePx: 10,
      bubbleBorderWidthPx: 1,
      copyGutterPx: 24,
      collapsedMaxHeightPx: 66,
      outerVerticalPx: 14,
      attachments: {
        sizePx: 44,
        gapPx: 6,
        marginTopPx: 8,
      },
    },
    askUser: {
      marginVerticalPx: 16,
      cardMaxWidthPx: 680,
      cardMinWidthPx: 280,
      cardPaddingPx: 12,
      cardGapPx: 12,
      tabsHeightPx: 32,
      panelHeightPx: 208,
      statusHeightPx: 16,
      actionsHeightPx: 34,
      hintHeightPx: 14,
    },
    thought: {
      paddingInlinePx: 4,
      paddingBlockPx: 8,
      typography: {
        fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
        fontSizePx: 12,
        lineHeightPx: 17,
        fontStyle: "italic",
      },
    },
    tools: {
      itemGapPx: 4,
      groupGapPx: 6,
      summary: {
        paddingInlinePx: 2,
        paddingBlockPx: 1,
        typography: {
          fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
          fontSizePx: 12,
          lineHeightPx: 16,
          fontWeight: 400,
        },
        separatorPaddingInlinePx: 4,
        statusDotPaddingInlinePx: 6,
      },
      loading: {
        typography: {
          fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
          fontSizePx: 12,
          lineHeightPx: 16,
        },
      },
      thoughtTitle: {
        typography: {
          fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
          fontSizePx: 11,
          lineHeightPx: 12,
        },
        marginBottomPx: 6,
      },
      thoughtBody: {
        paddingPx: 8,
        borderWidthPx: 1,
        typography: {
          fontFamily: '"SF Mono", Menlo, Monaco, Consolas, "Courier New", monospace',
          fontSizePx: 12,
          lineHeightPx: 17,
        },
      },
    },
    fixed: {
      spacerHeightPx: 1,
      turnStatusHeightPx: 24,
    },
  },
};

function fingerprintString(value: string): string {
  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `${value.length}:${(hash >>> 0).toString(36)}`;
}

export function getSessionThreadGeometryRevision(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): string {
  return fingerprintString(JSON.stringify(spec));
}

export const SESSION_THREAD_GEOMETRY_REVISION = getSessionThreadGeometryRevision();

export function resolveSessionThreadContentMaxWidthPx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return spec.viewport.rowMaxWidthPx - spec.viewport.horizontalInsetPx * 2;
}

export function resolveSessionThreadMarkdownInlineCodeEdgeBlockPx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return spec.markdown.inlineCode.paddingBlockPx + spec.markdown.inlineCode.borderWidthPx;
}

export function resolveSessionThreadMarkdownInlineCodeEdgeInlinePx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return spec.markdown.inlineCode.paddingInlinePx + spec.markdown.inlineCode.borderWidthPx;
}

export function resolveSessionThreadMarkdownInlineCodeFragmentChromeHeightPx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return resolveSessionThreadMarkdownInlineCodeEdgeBlockPx(spec) * 2;
}

export function resolveSessionThreadMarkdownInlineCodeFragmentChromeWidthPx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return resolveSessionThreadMarkdownInlineCodeEdgeInlinePx(spec) * 2;
}

export function resolveSessionThreadMarkdownBlockquoteInsetPx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return spec.markdown.blockquote.borderWidthPx + spec.markdown.blockquote.paddingInlineStartPx;
}

export function resolveSessionThreadAskUserShellHeightPx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return (
    spec.rows.askUser.cardPaddingPx * 2 +
    spec.rows.askUser.tabsHeightPx +
    spec.rows.askUser.cardGapPx +
    spec.rows.askUser.panelHeightPx +
    spec.rows.askUser.cardGapPx +
    spec.rows.askUser.statusHeightPx +
    spec.rows.askUser.cardGapPx +
    spec.rows.askUser.actionsHeightPx +
    spec.rows.askUser.cardGapPx +
    spec.rows.askUser.hintHeightPx
  );
}

export function resolveSessionThreadThoughtHorizontalChromePx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return spec.rows.thought.paddingInlinePx * 2;
}

export function resolveSessionThreadThoughtVerticalChromePx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return spec.rows.thought.paddingBlockPx * 2;
}

export function resolveSessionThreadToolSummaryRowHeightPx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return spec.rows.tools.summary.paddingBlockPx * 2 + spec.rows.tools.summary.typography.lineHeightPx;
}

export function resolveSessionThreadToolThoughtTitleHeightPx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return spec.rows.tools.thoughtTitle.typography.lineHeightPx + spec.rows.tools.thoughtTitle.marginBottomPx;
}

export function resolveSessionThreadToolThoughtBodyChromeWidthPx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return (spec.rows.tools.thoughtBody.paddingPx + spec.rows.tools.thoughtBody.borderWidthPx) * 2;
}

export function resolveSessionThreadToolThoughtBodyChromeHeightPx(
  spec: SessionThreadGeometrySpec = SESSION_THREAD_GEOMETRY_SPEC,
): number {
  return (spec.rows.tools.thoughtBody.paddingPx + spec.rows.tools.thoughtBody.borderWidthPx) * 2;
}
