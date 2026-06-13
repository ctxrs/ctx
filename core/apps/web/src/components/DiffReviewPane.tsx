import { memo, useEffect, useRef, useState } from "react";
import Editor from "@monaco-editor/react";
import type * as Monaco from "monaco-editor";
import type { editor as MonacoEditor } from "monaco-editor";
import { ChevronDown, ChevronUp } from "lucide-react";
import { FileIcon } from "./FileIcon";
import { useThemeVariant } from "../utils/theme";
import type { GitPaneFileEntry, GitPaneModel } from "../pages/workbenchShell/worktreeGitPaneModel";
import { estimateDiffHeightPx, fileAccentClass, parseUnifiedDiff, type DiffFile } from "./diffReviewDiffParser";

type PendingDiffRequest = {
  resolve: (files: DiffFile[]) => void;
  reject: (error: Error) => void;
};

let diffWorker: Worker | null = null;
let diffWorkerFailed = false;
let diffWorkerSeq = 0;
const diffWorkerPending = new Map<number, PendingDiffRequest>();

const failDiffWorker = (error: Error) => {
  diffWorkerFailed = true;
  if (diffWorker) {
    diffWorker.terminate();
    diffWorker = null;
  }
  for (const pending of diffWorkerPending.values()) {
    pending.reject(error);
  }
  diffWorkerPending.clear();
};

const ensureDiffWorker = (): Worker | null => {
  if (diffWorkerFailed) return null;
  if (diffWorker) return diffWorker;
  if (typeof Worker === "undefined") return null;
  try {
    diffWorker = new Worker(new URL("../workers/diffParserWorker.ts", import.meta.url), { type: "module" });
    diffWorker.onmessage = (event) => {
      const payload = event.data as { id?: number; files?: DiffFile[] };
      if (!payload || typeof payload.id !== "number") return;
      const pending = diffWorkerPending.get(payload.id);
      if (!pending) return;
      diffWorkerPending.delete(payload.id);
      pending.resolve(Array.isArray(payload.files) ? payload.files : []);
    };
    diffWorker.onerror = () => {
      failDiffWorker(new Error("Diff worker failed."));
    };
    diffWorker.onmessageerror = () => {
      failDiffWorker(new Error("Diff worker message error."));
    };
    return diffWorker;
  } catch {
    diffWorkerFailed = true;
    diffWorker = null;
    return null;
  }
};

const parseDiffInWorker = (diff: string): Promise<DiffFile[]> => {
  const worker = ensureDiffWorker();
  if (!worker) return Promise.resolve(parseUnifiedDiff(diff));
  return new Promise((resolve, reject) => {
    const id = diffWorkerSeq + 1;
    diffWorkerSeq = id;
    diffWorkerPending.set(id, { resolve, reject });
    worker.postMessage({ id, diff });
  });
};

const DiffReviewPane = memo(function DiffReviewPane({
  diff,
  inventory,
  detail,
  labels,
}: {
  diff: string;
  inventory?: GitPaneModel;
  detail?: {
    loading: boolean;
    error?: string | null;
    tooLarge?: boolean;
    tooLargeLabel?: string | null;
  };
  labels?: Partial<{
    empty: string;
  }>;
}) {
  const [expandedFiles, setExpandedFiles] = useState<Record<string, boolean>>({});
  const [wrapLines, setWrapLines] = useState(true);
  const [files, setFiles] = useState<DiffFile[]>([]);
  const [parsing, setParsing] = useState(false);
  const themeVariant = useThemeVariant();
  const monacoTheme = themeVariant === "dark" ? "vs-dark" : "vs";

  useEffect(() => {
    setExpandedFiles({});
  }, [diff]);

  useEffect(() => {
    const diffText = String(diff ?? "");
    if (!diffText.trim()) {
      setFiles([]);
      setParsing(false);
      return;
    }
    let cancelled = false;
    setParsing(true);
    setFiles([]);
    parseDiffInWorker(diffText)
      .then((next) => {
        if (cancelled) return;
        setFiles(next);
      })
      .catch(() => {
        if (cancelled) return;
        setFiles(parseUnifiedDiff(diffText));
      })
      .finally(() => {
        if (cancelled) return;
        setParsing(false);
      });
    return () => {
      cancelled = true;
    };
  }, [diff]);

  const toggleFile = (key: string) => setExpandedFiles((prev) => ({ ...prev, [key]: !(prev[key] ?? false) }));

  if (inventory) {
    const hasInventory = inventory.totalCount > 0;
    return (
      <div className="diff-pane">
        {inventory.unavailableLabel && <div className="muted">{inventory.unavailableLabel}</div>}
        {!inventory.unavailableLabel && inventory.loading && !hasInventory && (
          <div className="muted">Loading changes...</div>
        )}
        {!inventory.unavailableLabel && !inventory.loading && !hasInventory && (
          <div className="muted">{inventory.computeError ?? labels?.empty ?? "No changed files."}</div>
        )}
        {!inventory.unavailableLabel && hasInventory && inventory.largeChangeSet && (
          <div className="cursor-diff">
            <div className="muted" style={{ padding: 12 }}>{inventory.largeChangeSetLabel}</div>
          </div>
        )}
        {!inventory.unavailableLabel && hasInventory && !inventory.largeChangeSet && (
          <div className="cursor-diff">
            <div className="cursor-diff-toolbar">
              <button
                type="button"
                className={`cursor-diff-toggle ${wrapLines ? "cursor-diff-toggle-active" : ""}`}
                aria-pressed={wrapLines}
                title={wrapLines ? "Disable line wrap" : "Enable line wrap"}
                onClick={() => setWrapLines((prev) => !prev)}
              >
                Wrap lines
              </button>
            </div>
            {detail?.error ? <div className="muted">{detail.error}</div> : null}
            {!inventory.listReady ? <div className="muted">Loading changed files...</div> : null}
            {inventory.fileListTruncatedLabel ? (
              <div className="muted" style={{ padding: "8px 12px" }}>
                {inventory.fileListTruncatedLabel}
              </div>
            ) : null}
            <div className="cursor-diff-list">
              {inventory.sections.map((section) => (
                <div key={section.key} className="cursor-diff-section">
                  <div className="cursor-diff-section-header">
                    <span className="cursor-diff-section-title">{section.label}</span>
                    <span className="cursor-diff-section-count">{section.count}</span>
                  </div>
                  {section.files.map((fileEntry) => {
                    const isOpen = expandedFiles[fileEntry.path] ?? false;
                    const parsedFile = findParsedDiffFile(files, fileEntry);
                    return (
                      <div key={fileEntry.path} className={`cursor-diff-file ${parsedFile ? fileAccentClass(parsedFile) : ""}`}>
                        <div className="cursor-diff-file-header">
                          <button
                            type="button"
                            className="cursor-diff-chevron"
                            onClick={() => toggleFile(fileEntry.path)}
                            aria-label={isOpen ? "Collapse file diff" : "Expand file diff"}
                            aria-expanded={isOpen}
                          >
                            {isOpen ? <ChevronDown size={14} /> : <ChevronUp size={14} />}
                          </button>
                          <FileIcon path={fileEntry.path} size={14} className="cursor-diff-file-icon" />
                          <span className="cursor-diff-file-path" title={fileEntry.path}>
                            {fileEntry.path}
                          </span>
                          {renderInventorySummary(fileEntry, parsedFile)}
                          <div className="cursor-diff-spacer" />
                        </div>
                        {isOpen ? (
                          <div className="cursor-diff-file-body" role="region" aria-label={`Diff for ${fileEntry.path}`}>
                            {detail?.tooLarge ? (
                              <div className="muted" style={{ padding: 12 }}>
                                {detail.tooLargeLabel ?? "Diff too large to display."}
                              </div>
                            ) : detail?.error ? (
                              <div className="muted" style={{ padding: 12 }}>
                                {detail.error}
                              </div>
                            ) : detail?.loading && !parsedFile ? (
                              <div className="muted" style={{ padding: 12 }}>
                                Loading diff...
                              </div>
                            ) : parsedFile?.isBinary ? (
                              <div className="muted" style={{ padding: 12 }}>
                                Binary or metadata-only diff.
                              </div>
                            ) : parsedFile ? (
                              <div className="cursor-diff-editor-shell">
                                <DecoratedDiffEditor file={parsedFile} wrapLines={wrapLines} monacoTheme={monacoTheme} />
                              </div>
                            ) : (
                              <div className="muted" style={{ padding: 12 }}>
                                No file diff available.
                              </div>
                            )}
                          </div>
                        ) : null}
                      </div>
                    );
                  })}
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    );
  }

  const hasChanges = diff.trim().length > 0;

  return (
    <div className="diff-pane">
      {!hasChanges && <div className="muted">{labels?.empty ?? "No changed files."}</div>}

      {hasChanges && parsing && <div className="muted">Parsing diff...</div>}

      {hasChanges && !parsing && (
        <div className="cursor-diff">
          <div className="cursor-diff-toolbar">
            <button
              type="button"
              className={`cursor-diff-toggle ${wrapLines ? "cursor-diff-toggle-active" : ""}`}
              aria-pressed={wrapLines}
              title={wrapLines ? "Disable line wrap" : "Enable line wrap"}
              onClick={() => setWrapLines((prev) => !prev)}
            >
              Wrap lines
            </button>
          </div>
          <div className="cursor-diff-list">
            {files.length === 0 && <div className="muted">No parsed file diffs yet.</div>}
            {files.map((f) => {
              const isOpen = expandedFiles[f.key] ?? false;

              const summary = (
                <span className="cursor-diff-summary" aria-label="Diff summary">
                  {f.isNew ? (
                    <>
                      <span className="cursor-diff-new">(New)</span>{" "}
                      <span className="cursor-diff-plus">+{f.addedLines}</span>
                    </>
                  ) : f.isDeleted ? (
                    <>
                      <span className="cursor-diff-deleted">(Deleted)</span>{" "}
                      <span className="cursor-diff-minus">-{f.deletedLines}</span>
                    </>
                  ) : (
                    <>
                      <span className="cursor-diff-plus">+{f.addedLines}</span>{" "}
                      <span className="cursor-diff-minus">-{f.deletedLines}</span>
                    </>
                  )}
                </span>
              );

              return (
                <div
                  key={f.key}
                  className={`cursor-diff-file ${fileAccentClass(f)}`}
                >
                  <div className="cursor-diff-file-header">
                    <button
                      type="button"
                      className="cursor-diff-chevron"
                      onClick={() => toggleFile(f.key)}
                      aria-label={isOpen ? "Collapse file diff" : "Expand file diff"}
                      aria-expanded={isOpen}
                    >
                      {isOpen ? <ChevronDown size={14} /> : <ChevronUp size={14} />}
                    </button>
                    <FileIcon path={f.filePath} size={14} className="cursor-diff-file-icon" />
                    <span className="cursor-diff-file-path" title={f.filePath}>
                      {f.filePath}
                    </span>
                    {summary}
                    <div className="cursor-diff-spacer" />
                  </div>

                  {isOpen && (
                    <div className="cursor-diff-file-body" role="region" aria-label={`Diff for ${f.filePath}`}>
                      {f.isBinary ? (
                        <div className="muted" style={{ padding: 12 }}>
                          Binary or metadata-only diff.
                        </div>
                      ) : (
                        <>
                          <div className="cursor-diff-editor-shell">
                            <DecoratedDiffEditor file={f} wrapLines={wrapLines} monacoTheme={monacoTheme} />
                          </div>
                        </>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
});

export { DiffReviewPane };

function findParsedDiffFile(files: DiffFile[], entry: GitPaneFileEntry): DiffFile | null {
  const origPath = entry.origPath ?? "";
  for (const file of files) {
    if (file.filePath === entry.path || file.newPath === entry.path || file.oldPath === entry.path) return file;
    if (origPath && (file.filePath === origPath || file.oldPath === origPath || file.newPath === origPath)) {
      return file;
    }
  }
  return null;
}

function renderInventorySummary(entry: GitPaneFileEntry, parsedFile: DiffFile | null) {
  if (parsedFile) {
    return (
      <span className="cursor-diff-summary" aria-label="Diff summary">
        {parsedFile.isNew ? (
          <>
            <span className="cursor-diff-new">(New)</span>{" "}
            <span className="cursor-diff-plus">+{parsedFile.addedLines}</span>
          </>
        ) : parsedFile.isDeleted ? (
          <>
            <span className="cursor-diff-deleted">(Deleted)</span>{" "}
            <span className="cursor-diff-minus">-{parsedFile.deletedLines}</span>
          </>
        ) : (
          <>
            <span className="cursor-diff-plus">+{parsedFile.addedLines}</span>{" "}
            <span className="cursor-diff-minus">-{parsedFile.deletedLines}</span>
          </>
        )}
      </span>
    );
  }
  return (
    <span className="cursor-diff-summary" aria-label="File status">
      <span className="cursor-diff-status-pill">
        {entry.section === "staged"
          ? "Staged"
          : entry.section === "unstaged"
            ? "Unstaged"
            : entry.section === "untracked"
              ? "Untracked"
              : "Changed"}
      </span>
    </span>
  );
}

const WRAP_BREAK_AFTER_CHARACTERS =
  "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_/-=+*.,:;|\\~!@#$%^&()[]{}<>?\"'";

function DecoratedDiffEditor({
  file,
  wrapLines,
  monacoTheme,
}: {
  file: DiffFile;
  wrapLines: boolean;
  monacoTheme: "vs" | "vs-dark";
}) {
  const decorationIdsRef = useRef<string[]>([]);
  const [editorHeight, setEditorHeight] = useState(() => estimateDiffHeightPx(file));
  const [editorInstance, setEditorInstance] = useState<MonacoEditor.IStandaloneCodeEditor | null>(null);
  const modelPath = `inmemory://diff/${encodeURIComponent(file.key)}`;

  useEffect(() => {
    setEditorHeight(estimateDiffHeightPx(file));
  }, [file.key, file.renderText]);

  useEffect(() => {
    if (!editorInstance) return;
    const updateHeight = () => {
      const contentHeight = editorInstance.getContentHeight();
      const minHeight = estimateDiffHeightPx(file);
      setEditorHeight(Math.ceil(Math.max(minHeight, contentHeight)));
    };
    updateHeight();
    const disposable = editorInstance.onDidContentSizeChange(updateHeight);
    return () => disposable.dispose();
  }, [editorInstance, file.key, file.renderText]);

  return (
    <Editor
      key={`${file.key}:${wrapLines ? "wrap" : "nowrap"}`}
      height={`${editorHeight}px`}
      language="diff"
      path={modelPath}
      value={file.renderText}
      theme={monacoTheme}
      options={{
        readOnly: true,
        minimap: { enabled: false },
        scrollbar: {
          vertical: "hidden",
          horizontal: wrapLines ? "hidden" : "auto",
          handleMouseWheel: false,
          alwaysConsumeMouseWheel: false,
        },
        scrollBeyondLastLine: false,
        overviewRulerLanes: 0,
        hideCursorInOverviewRuler: true,
        glyphMargin: false,
        folding: false,
        lineNumbersMinChars: 2,
        fontSize: 12,
        lineHeight: 22,
        renderLineHighlight: "none",
        renderValidationDecorations: "off",
        fixedOverflowWidgets: true,
        padding: { top: 10, bottom: 10 },
        wordWrap: wrapLines ? "on" : "off",
        wrappingStrategy: wrapLines ? "advanced" : "simple",
        wordWrapBreakAfterCharacters: wrapLines ? WRAP_BREAK_AFTER_CHARACTERS : undefined,
      }}
      onMount={(editor: MonacoEditor.IStandaloneCodeEditor, monaco: typeof Monaco) => {
        setEditorInstance(editor);
        const applyDecorations = () => {
          const decs = file.renderLineKinds.flatMap((kind, idx) => {
            if (kind === "ctx") return [];
            const range = new monaco.Range(idx + 1, 1, idx + 1, 1);
            return [{
              range,
              options: {
                isWholeLine: true,
                className: kind === "add" ? "cursor-diff-line-add" : "cursor-diff-line-del",
              },
            }];
          });
          decorationIdsRef.current = editor.deltaDecorations(decorationIdsRef.current, decs);
        };

        applyDecorations();
      }}
    />
  );
}
