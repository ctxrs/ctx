import { useMemo } from "react";
import { useSearchParams } from "react-router-dom";
import { DiffReviewPane } from "../components/DiffReviewPane";

export default function CursorDiffDemoPage() {
  const [params] = useSearchParams();
  const state = params.get("state") ?? "1";
  const extraLines = clampInt(params.get("lines"), 0, 4000);

  const { fullDiff, cursorMetaOnlyDiff, longDiff } = useMemo(() => buildCursorDemoDiffs(extraLines), [extraLines]);
  const diff = state === "big" ? longDiff : state === "1" ? fullDiff : cursorMetaOnlyDiff;

  return (
    <div className="cursor-demo">
      {state === "1" && (
        <div className="cursor-demo-top">
          <div className="cursor-demo-tabs">
            <div className="cursor-demo-tab cursor-demo-tab-active">All Changes</div>
            <div className="cursor-demo-pill">6 Pending Changes</div>
          </div>
          <div className="cursor-demo-actions">
            <button type="button" className="cursor-demo-action">
              Find Issues <span aria-hidden="true">▾</span>
            </button>
            <button type="button" className="cursor-demo-action cursor-demo-action-primary">
              Commit
            </button>
            <button type="button" className="cursor-demo-action cursor-demo-action-ghost" aria-label="More">
              …
            </button>
          </div>
        </div>
      )}

      <div className="cursor-demo-content">
        <DiffReviewPane diff={diff} />
      </div>
    </div>
  );
}

function clampInt(value: string | null, min: number, max: number): number {
  const parsed = Number.parseInt(String(value ?? ""), 10);
  if (!Number.isFinite(parsed)) return min;
  return Math.min(max, Math.max(min, parsed));
}

function buildCursorDemoDiffs(extraLines: number): { fullDiff: string; cursorMetaOnlyDiff: string; longDiff: string } {
  const header = (path: string, added: number) => `diff --git a/dev/null b/${path}
new file mode 100644
index 0000000..e69de29
--- /dev/null
+++ b/${path}
@@ -0,0 +1,${added} @@`;

  const newFile = (path: string, lines: string[]) => `${header(path, lines.length)}
${lines.map((l) => `+${l}`).join("\n")}
`;

  const helloPy = newFile("hello.py", ['print("Hello, World!")', "", ""]);
  const helloJs = newFile("hello.js", ['console.log("Hello, World!");', "", ""]);
  const helloTxt = newFile("hello.txt", ["Hello, World!", "", ""]);
  const helloSh = newFile("hello.sh", ["#!/bin/bash", 'echo "Hello, World!"', "", ""]);
  const helloMd = newFile("hello.md", ["# Hello, World!", "", "Hello, World!", ""]);
  const longLines = Array.from({ length: extraLines }, (_v, i) => `line ${String(i + 1).padStart(4, "0")}: ` + "x".repeat(80));
  const longDiff = newFile("big.txt", longLines);

  const cursorMeta = `diff --git a/cursor-meta.json b/cursor-meta.json
index 1111111..2222222 100644
--- a/cursor-meta.json
+++ b/cursor-meta.json
@@ -1,7 +1,9 @@
 {
   "appBundle": "/Applications/Cursor.app",
   "bundleId": "com.todesktop.230313mz14w4u92",
   "version": "2.2.20",
   "build": "2.2.20",
-  "generatedAt": "2025-12-15T17:26:34Z"
+  "generatedAt": "2025-12-15T17:26:34Z",
+  "randomField": "hello from random change"
 }

`;

  const fullDiff = [helloPy, helloJs, helloTxt, helloSh, helloMd, cursorMeta].join("\n");
  return { fullDiff, cursorMetaOnlyDiff: cursorMeta, longDiff };
}
