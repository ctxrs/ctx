import { describe, expect, it } from "vitest";
import { createSessionMarkdownDocument, type SessionMarkdownBlock } from "./sessionMarkdownContract";
import { prepareInlineLayoutItems, type PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import { shouldDropLeadingCollapsedSpaceAtWrap } from "./sessionMarkdownInlineMeasurementContext";
import { BODY_TYPOGRAPHY, segmentImplicitWordBreaks } from "./sessionMarkdownMeasurementCore";

function findParagraphBlock(markdown: string, text: string): Extract<SessionMarkdownBlock, { kind: "paragraph" }> {
  const document = createSessionMarkdownDocument(markdown);
  const visitBlocks = (
    blocks: readonly SessionMarkdownBlock[],
  ): Extract<SessionMarkdownBlock, { kind: "paragraph" }> | null => {
    for (const block of blocks) {
      if (block.kind === "paragraph" && block.text.plainText.includes(text)) {
        return block;
      }
      if (block.kind === "blockquote") {
        const nested = visitBlocks(block.blocks);
        if (nested != null) {
          return nested;
        }
      }
      if (block.kind === "list") {
        for (const item of block.items) {
          const nested = visitBlocks(item.blocks);
          if (nested != null) {
            return nested;
          }
        }
      }
      if (block.kind === "table") {
        for (const row of block.rows) {
          for (const cell of row.cells) {
            if (cell == null) {
              continue;
            }
            const nested = visitBlocks(cell.blocks);
            if (nested != null) {
              return nested;
            }
          }
        }
      }
    }
    return null;
  };
  const paragraph = visitBlocks(document.blocks);
  if (paragraph == null) {
    throw new Error(`Paragraph containing ${JSON.stringify(text)} not found`);
  }
  return paragraph;
}

function prepareParagraphItems(
  markdown: string,
  text: string,
  wrapMode?: "normal" | "break-word" | "anywhere",
): PreparedInlineLayoutItem[] {
  const paragraph = findParagraphBlock(markdown, text);
  return prepareInlineLayoutItems({
    runs: paragraph.text.runs,
    typography: BODY_TYPOGRAPHY,
    cacheKeyPrefix: "session-markdown-inline-layout-test",
    wrapMode,
  });
}

describe("sessionMarkdownInlineLayout", () => {
  it("keeps slash-delimited prose tokens as standalone text segments", () => {
    const items = prepareParagraphItems(
      "I’ve got a second class now: several of the failures in `new-workspace 9` are plain network-denied installs from a containerized/sandboxed path, not just bad metadata. I’m checking whether those attempts were targeting the sandbox runtime rather than host, because that would explain the `pip` `[Errno 101] Network is unreachable` pattern across multiple providers.",
      "containerized/sandboxed",
    );

    const textSegments = items
      .filter((item): item is Extract<PreparedInlineLayoutItem, { kind: "segment" }> => item.kind === "segment")
      .filter((item) => item.codeGroupId == null)
      .map((item) => item.text);

    expect(textSegments).toContain("containerized/sandboxed");
    expect(textSegments.some((text) => text.endsWith("containerized/"))).toBe(false);
  });

  it("prevents styled-seam prose from dropping its leading collapsed wrap space", () => {
    const items = prepareParagraphItems(
      [
        "**Important caveat**",
        "Do not make the provider package age gate too small unless you also require stronger checks for that fast path:",
      ].join("\n"),
      "Do not make the provider package age gate too small",
    );

    const target = items.find(
      (item): item is Extract<PreparedInlineLayoutItem, { kind: "segment" }> =>
        item.kind === "segment" &&
        item.codeGroupId == null &&
        item.text.startsWith("Do not make the provider package age gate too small"),
    );

    expect(target).toBeDefined();
    expect(target?.startsAfterStyledTextSeam).toBe(true);
    expect(
      shouldDropLeadingCollapsedSpaceAtWrap({
        item: target!,
        codeGroupId: null,
        lineHasContent: true,
        cursor: null,
        pendingSpaceWidth: 8,
      }),
    ).toBe(false);
    expect(
      shouldDropLeadingCollapsedSpaceAtWrap({
        item: {
          ...target!,
          startsAfterInlineCodeSeam: false,
          startsAfterStyledTextSeam: false,
          startsStyledTextAfterInlineCodeSeam: false,
          startsStyledTextAfterBodySeam: false,
        },
        codeGroupId: null,
        lineHasContent: true,
        cursor: null,
        pendingSpaceWidth: 8,
      }),
    ).toBe(true);
  });

  it("uses implicit Thai word boundaries for styled-seam min-start width", () => {
    const implicitSegments = segmentImplicitWordBreaks("ทดสอบการตัดคำ");
    const items = prepareParagraphItems(
      "Lead [parity](https://example.com) *token* ทดสอบการตัดคำ fragment session one host/workspace.",
      "ทดสอบการตัดคำ fragment session one",
    );

    const target = items.find(
      (item): item is Extract<PreparedInlineLayoutItem, { kind: "segment" }> =>
        item.kind === "segment" &&
        item.codeGroupId == null &&
        item.text === "ทดสอบการตัดคำ",
    );

    expect(target).toBeDefined();
    expect(implicitSegments.length).toBeGreaterThan(1);
    expect(implicitSegments.join("")).toBe("ทดสอบการตัดคำ");
    expect(target?.text).toBe("ทดสอบการตัดคำ");
  });

  it("disables prose min-start guards for anywhere table-cell text", () => {
    const items = prepareParagraphItems(
      "| Kind | Note |\n|---|---|\n| summary | browser context containerd/BuildKit/nerdctl padding stream. |",
      "browser context containerd/BuildKit/nerdctl padding stream.",
      "anywhere",
    );

    const target = items.find(
      (item): item is Extract<PreparedInlineLayoutItem, { kind: "segment" }> =>
        item.kind === "segment" &&
        item.codeGroupId == null &&
        item.text.includes("containerd/BuildKit/nerdctl"),
    );

    expect(target).toBeDefined();
    expect(target?.allowsBreakWord).toBe(true);
    expect(target?.minStartTextWidth).toBe(0);
  });

  it("keeps surrounding prose attached to slash tokens in anywhere table-cell text", () => {
    const items = prepareParagraphItems(
      "| Kind | Note |\n|---|---|\n| summary | browser context containerd/BuildKit/nerdctl padding stream. |",
      "browser context containerd/BuildKit/nerdctl padding stream.",
      "anywhere",
    );

    const textSegments = items
      .filter((item): item is Extract<PreparedInlineLayoutItem, { kind: "segment" }> => item.kind === "segment")
      .filter((item) => item.codeGroupId == null)
      .map((item) => item.text);

    expect(
      textSegments.some(
        (text) =>
          text.startsWith("browser context ") &&
          text.includes("containerd/BuildKit/nerdctl") &&
          text.endsWith(" padding stream."),
      ),
    ).toBe(true);
    expect(textSegments).not.toContain("containerd/BuildKit/nerdctl");
  });

  it("tokenizes slash-sensitive prose runs at word boundaries in normal wrap mode", () => {
    const items = prepareParagraphItems(
      "**Remote execution**: On-Demand EC2 only, Ubuntu/Debian, fast `gp3` or local NVMe depending on workload, containerd/BuildKit/nerdctl installed, strict VPC/security group egress controls, one host/workspace isolation model as needed.",
      "containerd/BuildKit/nerdctl",
    );

    const textSegments = items
      .filter((item): item is Extract<PreparedInlineLayoutItem, { kind: "segment" }> => item.kind === "segment")
      .filter((item) => item.codeGroupId == null)
      .map((item) => item.text);

    expect(textSegments).toContain("containerd/BuildKit/nerdctl");
    expect(textSegments).toContain("VPC/security");
    expect(textSegments).toContain("host/workspace");
    expect(textSegments).toContain("needed.");
    expect(textSegments).not.toContain("isolation model as needed.");
  });

  it("preserves hyphen break opportunities when slash-sensitive prose tokenization is active", () => {
    const items = prepareParagraphItems(
      [
        "**Use active heads for first paint**",
        "If `active_heads` has a compatible non-empty bounded head, render it immediately as bootstrap/pending-authority. Do not let it overwrite newer authoritative replica state.",
      ].join("\n"),
      "non-empty bounded head",
    );

    const textSegments = items
      .filter((item): item is Extract<PreparedInlineLayoutItem, { kind: "segment" }> => item.kind === "segment")
      .filter((item) => item.codeGroupId == null)
      .map((item) => item.text);

    expect(textSegments).toContain("non-");
    expect(textSegments).toContain("empty");
    expect(textSegments).toContain("bootstrap/pending-");
    expect(textSegments).toContain("authority.");
    expect(textSegments).not.toContain("non-empty");
    expect(textSegments).not.toContain("bootstrap/pending-authority.");
  });

  it("splits a trailing plain hyphenated path tail into deterministic code fragments", () => {
    const items = prepareParagraphItems(
      "Probe `pages/apps/turn-header`: trailing prose keeps wrapping.",
      "Probe",
    );

    const codeSegments = items
      .filter((item): item is Extract<PreparedInlineLayoutItem, { kind: "segment" }> => item.kind === "segment")
      .filter((item) => item.codeGroupId != null)
      .map((item) => item.text);

    expect(codeSegments).toContain("turn-");
    expect(codeSegments).toContain("header");
    expect(codeSegments).not.toContain("turn-header");
  });
});
