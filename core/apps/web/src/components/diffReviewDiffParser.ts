export type DiffFile = {
  key: string;
  oldPath: string;
  newPath: string;
  filePath: string;
  sectionLines: string[];
  headerLines: string[];
  hunks: DiffHunk[];
  isNew: boolean;
  isDeleted: boolean;
  isBinary: boolean;
  addedLines: number;
  deletedLines: number;
  renderText: string;
  renderLineKinds: Array<"add" | "del" | "ctx">;
};

type DiffHunk = {
  key: string;
  headerLine: string;
  lines: string[];
};

type RawDiffFile = Omit<
  DiffFile,
  "filePath" | "isNew" | "isDeleted" | "isBinary" | "addedLines" | "deletedLines" | "renderText" | "renderLineKinds"
>;

export function parseUnifiedDiff(diffText: string): DiffFile[] {
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
    const file: DiffFile = finalizeFile(current);
    files.push(file);
    current = null;
    inHeader = false;
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (line.startsWith("diff --git ")) {
      pushCurrent();
      const { oldPath, newPath } = parseDiffHeaderPaths(line);
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

function parseDiffHeaderPaths(line: string): { oldPath: string; newPath: string } {
  const remainder = line.trim().replace(/^diff --git\s+/, "");
  const parts = splitDiffHeaderTokens(remainder);
  const oldRaw = parts[0] ?? "";
  const newRaw = parts[1] ?? "";
  return {
    oldPath: normalizeDiffPath(oldRaw),
    newPath: normalizeDiffPath(newRaw),
  };
}

function splitDiffHeaderTokens(input: string): string[] {
  const tokens: string[] = [];
  let i = 0;
  while (i < input.length && tokens.length < 2) {
    while (i < input.length && /\s/.test(input[i])) i++;
    if (i >= input.length) break;
    if (input[i] === "\"") {
      i++;
      let token = "";
      while (i < input.length) {
        const ch = input[i];
        if (ch === "\"") {
          i++;
          break;
        }
        if (ch === "\\" && i + 1 < input.length) {
          i++;
          token += input[i];
          i++;
          continue;
        }
        token += ch;
        i++;
      }
      tokens.push(token);
      continue;
    }
    let token = "";
    while (i < input.length && !/\s/.test(input[i])) {
      token += input[i];
      i++;
    }
    tokens.push(token);
  }
  return tokens;
}

function normalizeDiffPath(value: string): string {
  let out = value.replace(/^"+|"+$/g, "");
  if (out.startsWith("a/") || out.startsWith("b/")) {
    out = out.slice(2);
  }
  return out;
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
  const isDeleted = raw.newPath === "dev/null" || headerText.includes("deleted file mode") || headerText.includes("+++ /dev/null");
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

export function estimateDiffHeightPx(file: DiffFile): number {
  const visibleLines = Math.max(3, file.renderText ? file.renderText.split("\n").length : 0);
  const lineHeight = 22;
  const paddingTopBottom = 20;
  const safetyLines = 2;
  return (visibleLines + safetyLines) * lineHeight + paddingTopBottom;
}

export function fileAccentClass(file: DiffFile): string {
  if (file.isDeleted && !file.isNew) return "cursor-diff-file-deleted";
  if (file.isNew) return "cursor-diff-file-new";
  if (file.deletedLines > 0 && file.addedLines === 0) return "cursor-diff-file-deleted";
  return "cursor-diff-file-modified";
}
