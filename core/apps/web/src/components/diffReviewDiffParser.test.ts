import { describe, expect, test } from "vitest";
import { parseUnifiedDiff } from "./diffReviewDiffParser";

describe("parseUnifiedDiff side-by-side reconstruction", () => {
  test("reconstructs old and new file text for a modified file", () => {
    const diff = [
      "diff --git a/foo.txt b/foo.txt",
      "index 1111111..2222222 100644",
      "--- a/foo.txt",
      "+++ b/foo.txt",
      "@@ -1,3 +1,3 @@",
      " line one",
      "-old second",
      "+new second",
      " line three",
    ].join("\n");

    const [file] = parseUnifiedDiff(diff);

    expect(file.oldText).toBe("line one\nold second\nline three");
    expect(file.newText).toBe("line one\nnew second\nline three");
  });

  test("leaves old text empty for a newly added file", () => {
    const diff = [
      "diff --git a/new.txt b/new.txt",
      "new file mode 100644",
      "index 0000000..2222222",
      "--- /dev/null",
      "+++ b/new.txt",
      "@@ -0,0 +1,2 @@",
      "+alpha",
      "+beta",
    ].join("\n");

    const [file] = parseUnifiedDiff(diff);

    expect(file.oldText).toBe("");
    expect(file.newText).toBe("alpha\nbeta");
  });

  test("leaves new text empty for a deleted file", () => {
    const diff = [
      "diff --git a/gone.txt b/gone.txt",
      "deleted file mode 100644",
      "index 2222222..0000000",
      "--- a/gone.txt",
      "+++ /dev/null",
      "@@ -1,2 +0,0 @@",
      "-x",
      "-y",
    ].join("\n");

    const [file] = parseUnifiedDiff(diff);

    expect(file.oldText).toBe("x\ny");
    expect(file.newText).toBe("");
  });
});
