import { describe, expect, it } from "vitest";
import { createSessionMarkdownDocument } from "./sessionMarkdownContract";

describe("sessionMarkdownContract", () => {
  it("keeps mixed inline table-cell content in one paragraph", () => {
    const document = createSessionMarkdownDocument(
      [
        "| Use | Provider A monthly | Provider B monthly |",
        "|---|---:|---:|",
        "| 16 vCPU / 64 GB remote box | `large-dedicated`: ~$589 compute + disk | `balanced-dedicated`: ~$153 |",
      ].join("\n"),
    );

    const [table] = document.blocks;
    expect(table?.kind).toBe("table");
    if (table?.kind !== "table") {
      throw new Error("expected parsed markdown table");
    }

    const awsCell = table.rows[1]?.cells[1];
    expect(awsCell?.blocks).toHaveLength(1);
    const [paragraph] = awsCell?.blocks ?? [];
    expect(paragraph?.kind).toBe("paragraph");
    if (paragraph?.kind !== "paragraph") {
      throw new Error("expected table cell paragraph");
    }

    expect(paragraph.text.plainText).toBe("large-dedicated: ~$589 compute + disk");
    expect(paragraph.text.runs.map((run) => run.kind)).toEqual(["inlineCode", "text"]);
  });
});
