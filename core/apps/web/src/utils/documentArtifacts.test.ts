import { describe, expect, it } from "vitest";
import type { Artifact } from "../api/client";
import {
  buildArtifactDocumentPreview,
  getArtifactDocumentFormat,
  normalizeMdxDocumentContent,
} from "./documentArtifacts";

const makeArtifact = (overrides: Partial<Artifact> = {}): Artifact =>
  ({
    id: "artifact-1",
    session_id: "session-1",
    task_id: "task-1",
    name: "artifact",
    mime_type: "application/octet-stream",
    bytes: 128,
    absolute_path: "/tmp/artifact.bin",
    missing: false,
    created_at: "2026-03-13T00:00:00.000Z",
    ...overrides,
  }) as Artifact;

const sampleMdx = `---
title: Merge queue for agents
description: Fast path
---

import Example from "./Example"

## Title

Paragraph before JSX.

<Callout tone="note">Keep this out of the preview.</Callout>

export const metadata = { ok: true };

Paragraph after JSX.
`;

describe("documentArtifacts", () => {
  it("detects markdown-like and text-like document formats", () => {
    expect(
      getArtifactDocumentFormat(
        makeArtifact({
          name: "post.mdx",
          absolute_path: "/tmp/post.mdx",
        }),
      ),
    ).toBe("mdx");

    expect(
      getArtifactDocumentFormat(
        makeArtifact({
          name: "notes.md",
          absolute_path: "/tmp/notes.md",
        }),
      ),
    ).toBe("markdown");

    expect(
      getArtifactDocumentFormat(
        makeArtifact({
          name: "server.log",
          absolute_path: "/tmp/server.log",
        }),
      ),
    ).toBe("text");

    expect(
      getArtifactDocumentFormat(
        makeArtifact({
          name: "main.ts",
          absolute_path: "/tmp/main.ts",
        }),
      ),
    ).toBe("text");
  });

  it("normalizes mdx into safe markdown", () => {
    const normalized = normalizeMdxDocumentContent(sampleMdx);

    expect(normalized).toContain("## Title");
    expect(normalized).toContain("Paragraph before JSX.");
    expect(normalized).toContain("Paragraph after JSX.");
    expect(normalized).not.toContain("title:");
    expect(normalized).not.toContain("description:");
    expect(normalized).not.toContain('import Example from "./Example"');
    expect(normalized).not.toContain("<Callout");
    expect(normalized).not.toContain("export const metadata");
  });

  it("builds markdown previews for mdx and raw text previews for invalid mdx", () => {
    const mdxArtifact = makeArtifact({
      name: "post.mdx",
      absolute_path: "/tmp/post.mdx",
    });

    expect(buildArtifactDocumentPreview(mdxArtifact, sampleMdx)).toEqual({
      format: "mdx",
      renderKind: "markdown",
      content: "## Title\n\nParagraph before JSX.\n\nParagraph after JSX.",
    });

    expect(
      buildArtifactDocumentPreview(
        mdxArtifact,
        "export const broken =\n\n# Heading",
      ),
    ).toEqual({
      format: "mdx",
      renderKind: "text",
      content: "export const broken =\n\n# Heading",
    });
  });
});
