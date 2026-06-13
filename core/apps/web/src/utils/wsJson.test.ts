import { describe, expect, it } from "vitest";
import { parseWsJson } from "./wsJson";
import { Blob as NodeBlob } from "buffer";

describe("parseWsJson", () => {
  it("parses string JSON", async () => {
    expect(await parseWsJson("{\"type\":\"ready\"}")).toEqual({ type: "ready" });
  });

  it("parses Blob JSON", async () => {
    const blob = new NodeBlob(["{\"type\":\"interim\",\"text\":\"hi\"}"], { type: "application/json" });
    expect(await parseWsJson(blob)).toEqual({ type: "interim", text: "hi" });
  });

  it("returns null on invalid JSON", async () => {
    expect(await parseWsJson("{nope")).toBeNull();
  });
});
