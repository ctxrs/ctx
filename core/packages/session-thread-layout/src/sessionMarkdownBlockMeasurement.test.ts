import { beforeEach, describe, expect, it, vi } from "vitest";

const {
  measureInlineRunsHeightMock,
  measureSessionPlainTextBlockHeightMock,
  parseMarkdownMock,
} = vi.hoisted(() => ({
  measureInlineRunsHeightMock:
    vi.fn<(typeof import("./sessionMarkdownInlineMeasurement"))["measureInlineRunsHeight"]>(),
  measureSessionPlainTextBlockHeightMock:
    vi.fn<(typeof import("./sessionPlainTextMeasurement"))["measureSessionPlainTextBlockHeight"]>(),
  parseMarkdownMock: vi.fn<(typeof import("./sessionMarkdownMeasurementCore"))["parseMarkdown"]>(),
}));

vi.mock("./sessionMarkdownInlineMeasurement", async () => {
  const actual = await vi.importActual<typeof import("./sessionMarkdownInlineMeasurement")>(
    "./sessionMarkdownInlineMeasurement",
  );
  return {
    ...actual,
    measureInlineRunsHeight: measureInlineRunsHeightMock,
  };
});

vi.mock("./sessionPlainTextMeasurement", async () => {
  const actual = await vi.importActual<typeof import("./sessionPlainTextMeasurement")>(
    "./sessionPlainTextMeasurement",
  );
  return {
    ...actual,
    measureSessionPlainTextBlockHeight: measureSessionPlainTextBlockHeightMock,
  };
});

vi.mock("./sessionMarkdownMeasurementCore", async () => {
  const actual = await vi.importActual<typeof import("./sessionMarkdownMeasurementCore")>(
    "./sessionMarkdownMeasurementCore",
  );
  return {
    ...actual,
    parseMarkdown: parseMarkdownMock,
  };
});

import { measureSessionMarkdownDocument } from "./sessionMarkdownBlockMeasurement";

describe("sessionMarkdownBlockMeasurement", () => {
  beforeEach(() => {
    measureInlineRunsHeightMock.mockReset();
    measureSessionPlainTextBlockHeightMock.mockReset();
    parseMarkdownMock.mockReset();
  });

  it("trusts mixed inline layout instead of a plain-text slash fallback", () => {
    parseMarkdownMock.mockReturnValue({
      source: "probe",
      blocks: [
        {
          kind: "paragraph",
          node: { type: "paragraph" } as never,
          inlines: [],
          text: {
            plainText:
              "Pretext header probe stream deterministic marker command marker deterministic VPC/security group delta padding prix: browser summary.",
            runs: [
              {
                kind: "text",
                text: "Pretext header probe stream ",
                style: "body",
                deleted: false,
              },
              {
                kind: "text",
                text: "deterministic marker",
                style: "body",
                deleted: false,
              },
              {
                kind: "text",
                text: "command marker deterministic",
                style: "strong",
                deleted: false,
              },
              {
                kind: "text",
                text: " VPC/security group delta padding prix: browser summary.",
                style: "body",
                deleted: false,
              },
            ],
            hasInlineCode: false,
            hasHardBreak: false,
            hasStyledText: true,
            hasLink: false,
          },
        },
      ],
    });
    measureInlineRunsHeightMock.mockReturnValue(84);
    measureSessionPlainTextBlockHeightMock.mockReturnValue(105);

    const height = measureSessionMarkdownDocument("probe", 472);

    expect(height).toBe(84);
    expect(measureInlineRunsHeightMock).toHaveBeenCalledTimes(1);
    expect(measureSessionPlainTextBlockHeightMock).not.toHaveBeenCalled();
  });

  it("routes link-rich paragraphs through inline measurement instead of the plain-text fast path", () => {
    parseMarkdownMock.mockReturnValue({
      source: "probe",
      blocks: [
        {
          kind: "paragraph",
          node: { type: "paragraph" } as never,
          inlines: [],
          text: {
            plainText:
              "Parity turn message near the seam header token session browser padding stream.",
            runs: [
              {
                kind: "text",
                text: "Parity turn message near the seam ",
                style: "body",
                deleted: false,
              },
              {
                kind: "text",
                text: "header token session",
                style: "body",
                deleted: false,
              },
              {
                kind: "text",
                text: " browser padding stream.",
                style: "body",
                deleted: false,
              },
            ],
            hasInlineCode: false,
            hasHardBreak: false,
            hasStyledText: false,
            hasLink: true,
          },
        },
      ],
    });
    measureInlineRunsHeightMock.mockReturnValue(63);
    measureSessionPlainTextBlockHeightMock.mockReturnValue(84);

    const height = measureSessionMarkdownDocument("probe", 788);

    expect(height).toBe(63);
    expect(measureInlineRunsHeightMock).toHaveBeenCalledTimes(1);
    expect(measureSessionPlainTextBlockHeightMock).not.toHaveBeenCalled();
  });

  it("routes soft-newline-only paragraphs through inline measurement instead of pre-wrap plain text", () => {
    parseMarkdownMock.mockReturnValue({
      source: "probe",
      blocks: [
        {
          kind: "paragraph",
          node: { type: "paragraph" } as never,
          inlines: [],
          text: {
            plainText: "line 1 with enough words to wrap\nline 2 with enough words to wrap",
            runs: [
              {
                kind: "text",
                text: "line 1 with enough words to wrap\nline 2 with enough words to wrap",
                style: "body",
                deleted: false,
              },
            ],
            hasInlineCode: false,
            hasHardBreak: false,
            hasStyledText: false,
            hasLink: false,
          },
        },
      ],
    });
    measureInlineRunsHeightMock.mockReturnValue(63);
    measureSessionPlainTextBlockHeightMock.mockReturnValue(84);

    const height = measureSessionMarkdownDocument("probe", 520);

    expect(height).toBe(63);
    expect(measureInlineRunsHeightMock).toHaveBeenCalledTimes(1);
    expect(measureSessionPlainTextBlockHeightMock).not.toHaveBeenCalled();
  });

  it("measures table-cell paragraphs with normal inline token wrapping", () => {
    parseMarkdownMock.mockReturnValue({
      source: "probe",
      blocks: [
        {
          kind: "table",
          node: { type: "table" } as never,
          rows: [
            {
              cells: [
                {
                  blocks: [
                    {
                      kind: "paragraph",
                      node: { type: "paragraph" } as never,
                      inlines: [],
                      text: {
                        plainText: "browser context containerd/BuildKit/nerdctl padding stream.",
                        runs: [
                          {
                            kind: "text",
                            text: "browser context ",
                            style: "body",
                            deleted: false,
                          },
                          {
                            kind: "inlineCode",
                            text: "containerd/BuildKit/nerdctl",
                            parts: ["containerd/BuildKit/nerdctl"],
                          },
                          {
                            kind: "text",
                            text: " padding stream.",
                            style: "body",
                            deleted: false,
                          },
                        ],
                        hasInlineCode: true,
                        hasHardBreak: false,
                        hasStyledText: false,
                        hasLink: false,
                      },
                    },
                  ],
                },
              ],
            },
          ],
        },
      ],
    });
    measureInlineRunsHeightMock.mockReturnValue(63);

    measureSessionMarkdownDocument("probe", 540);

    expect(measureInlineRunsHeightMock).toHaveBeenCalledWith(
      expect.objectContaining({
        cacheKeyPrefix: "table-header-inline",
        wrapMode: "normal",
      }),
    );
  });
});
