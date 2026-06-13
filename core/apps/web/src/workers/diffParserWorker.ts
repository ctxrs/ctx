/// <reference lib="webworker" />

type DiffHunk = {
  key: string;
  headerLine: string;
  lines: string[];
};

type RawDiffFile = {
  key: string;
  oldPath: string;
  newPath: string;
  sectionLines: string[];
  headerLines: string[];
  hunks: DiffHunk[];
};

type DiffFile = RawDiffFile & {
  filePath: string;
  isNew: boolean;
  isDeleted: boolean;
  isBinary: boolean;
  addedLines: number;
  deletedLines: number;
  renderText: string;
  renderLineKinds: Array<"add" | "del" | "ctx">;
};

function parseUnifiedDiff(diffText: string): DiffFile[] {
  const lines = String(diffText ?? "").split("\n");
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  const files: DiffFile[] = [];
  let current: RawDiffFile | null = null;
  let inHeader = false;
  let currentHunk: DiffHunk | null = null;

  const pushCurrent = () => {
    if (!current) return;
    if (currentHunk) {
      current.hunks.push(currentHunk);
      currentHunk = null;
    }
    const file = finalizeFile(current);
    files.push(file);
    current = null;
    inHeader = false;
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (line.startsWith("diff --git ")) {
      pushCurrent();
      const match = /^diff --git a\/(.+?) b\/(.+)$/.exec(line);
      const oldPath = match?.[1] ?? "";
      const newPath = match?.[2] ?? "";
      const key = `${oldPath}=>${newPath}:${i}`;
      current = {
        key,
        oldPath,
        newPath,
        sectionLines: [line],
        headerLines: [line],
        hunks: [],
      };
      inHeader = true;
      continue;
    }

    if (!current) continue;
    current.sectionLines.push(line);

    if (line.startsWith("@@ ")) {
      if (currentHunk) current.hunks.push(currentHunk);
      currentHunk = { key: `${current.key}:h${current.hunks.length}:${i}`, headerLine: line, lines: [] };
      inHeader = false;
      continue;
    }

    if (inHeader) {
      current.headerLines.push(line);
    } else if (currentHunk) {
      currentHunk.lines.push(line);
    }
  }

  pushCurrent();
  return files.filter((file) => file.sectionLines.some((line) => line.trim().length > 0));
}

function finalizeFile(raw: RawDiffFile): DiffFile {
  const filePath =
    raw.newPath && raw.newPath !== "dev/null"
      ? raw.newPath
      : raw.oldPath && raw.oldPath !== "dev/null"
        ? raw.oldPath
        : "(unknown)";
  const headerText = raw.headerLines.join("\n");
  const isNew = raw.oldPath === "dev/null" || headerText.includes("new file mode") || headerText.includes("--- /dev/null");
  const isDeleted =
    raw.newPath === "dev/null" || headerText.includes("deleted file mode") || headerText.includes("+++ /dev/null");
  const patchText = raw.sectionLines.join("\n");
  const isBinary = patchText.includes("GIT binary patch") || patchText.includes("Binary files");

  let addedLines = 0;
  let deletedLines = 0;
  const renderLines: string[] = [];
  const renderLineKinds: Array<"add" | "del" | "ctx"> = [];

  for (const h of raw.hunks) {
    for (const line of h.lines) {
      if (!line) continue;
      const prefix = line[0];
      if (prefix === "+") {
        if (!line.startsWith("+++")) {
          addedLines += 1;
          renderLines.push(line.slice(1));
          renderLineKinds.push("add");
        }
        continue;
      }
      if (prefix === "-") {
        if (!line.startsWith("---")) {
          deletedLines += 1;
          renderLines.push(line.slice(1));
          renderLineKinds.push("del");
        }
        continue;
      }
      if (prefix === " ") {
        renderLines.push(line.slice(1));
        renderLineKinds.push("ctx");
      }
      // \ No newline at end of file
    }
  }

  return {
    ...raw,
    filePath,
    isNew,
    isDeleted,
    isBinary: isBinary || raw.hunks.length === 0,
    addedLines,
    deletedLines,
    renderText: renderLines.join("\n"),
    renderLineKinds,
  };
}

self.addEventListener("message", (event: MessageEvent) => {
  const payload = (event.data ?? {}) as { id?: number; diff?: string };
  if (typeof payload.id !== "number") return;
  const files = parseUnifiedDiff(String(payload.diff ?? ""));
  (self as DedicatedWorkerGlobalScope).postMessage({ id: payload.id, files });
});
