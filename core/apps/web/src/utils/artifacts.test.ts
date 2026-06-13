import { describe, expect, it } from "vitest";
import type { Artifact } from "../api/client";
import { getArtifactPreviewKind, isPreviewableArtifact } from "./artifacts";

const makeArtifact = (overrides: Partial<Artifact> = {}): Artifact =>
  ({
    id: "artifact-1",
    session_id: "session-1",
    task_id: "task-1",
    turn_id: null,
    name: "artifact",
    mime_type: "application/octet-stream",
    bytes: 128,
    absolute_path: "/tmp/artifact.bin",
    missing: false,
    created_at: "2026-03-13T00:00:00.000Z",
    ...overrides,
  }) as Artifact;

describe("artifact preview classification", () => {
  it("keeps csv artifacts non-previewable", () => {
    const artifact = makeArtifact({
      name: "report.csv",
      mime_type: "text/csv",
      absolute_path: "/tmp/report.csv",
    });

    expect(getArtifactPreviewKind(artifact)).toBe("none");
    expect(isPreviewableArtifact(artifact)).toBe(false);
  });

  it("classifies markdown by extension", () => {
    expect(
      getArtifactPreviewKind(
        makeArtifact({
          name: "notes.md",
          absolute_path: "/tmp/notes.md",
        }),
      ),
    ).toBe("markdown");
  });

  it("classifies mdx by extension even when the mime falls back to octet-stream", () => {
    expect(
      getArtifactPreviewKind(
        makeArtifact({
          name: "post.mdx",
          mime_type: "application/octet-stream",
          absolute_path: "/tmp/post.mdx",
        }),
      ),
    ).toBe("markdown");
  });

  it("classifies plain text and json artifacts as text", () => {
    expect(
      getArtifactPreviewKind(
        makeArtifact({
          name: "server.log",
          absolute_path: "/tmp/server.log",
        }),
      ),
    ).toBe("text");

    expect(
      getArtifactPreviewKind(
        makeArtifact({
          name: "report.json",
          mime_type: "application/json",
          absolute_path: "/tmp/report.json",
        }),
      ),
    ).toBe("text");

    expect(
      getArtifactPreviewKind(
        makeArtifact({
          name: "main.ts",
          absolute_path: "/tmp/main.ts",
        }),
      ),
    ).toBe("text");
  });
});
