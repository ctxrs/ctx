import React, { useState } from "react";
import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { copyTextToClipboardMock } = vi.hoisted(() => ({
  copyTextToClipboardMock: vi.fn(async () => true),
}));

vi.mock("../../utils/clipboard", () => ({
  copyTextToClipboard: copyTextToClipboardMock,
}));

import { AssistantEntry, ThreadItemView, WorkbenchToolRow, WorkbenchTurnHeaderView } from "./SessionThreadItemViews";

function TestHeader({ plainText = "line 1\nline 2\nline 3\nline 4\nline 5" }: { plainText?: string }) {
  const [expanded, setExpanded] = useState(false);
  return (
    <div>
      <p data-testid="outside">outside selected text</p>
      <WorkbenchTurnHeaderView
        header={{
          id: "header-1",
          content: plainText,
          attachments: [],
          created_at: "2025-01-01T00:00:00.000Z",
        }}
        plainText={plainText}
        expanded={expanded}
        onToggle={() => setExpanded((value) => !value)}
      />
    </div>
  );
}

function selectNodeContents(node: Node) {
  const range = document.createRange();
  range.selectNodeContents(node);
  const selection = window.getSelection();
  selection?.removeAllRanges();
  selection?.addRange(range);
}

function getHeader(): HTMLDivElement {
  const header = document.querySelector(".wb-turn-header");
  if (!(header instanceof HTMLDivElement)) {
    throw new Error("Expected .wb-turn-header to be rendered");
  }
  return header;
}

describe("WorkbenchTurnHeaderView", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    window.getSelection()?.removeAllRanges();
    copyTextToClipboardMock.mockClear();
  });

  it("expands when clicked even if unrelated text elsewhere is selected", () => {
    render(<TestHeader />);

    const outside = screen.getByTestId("outside");
    selectNodeContents(outside);
    expect(window.getSelection()?.toString()).toContain("outside selected text");

    const header = getHeader();
    expect(header).toHaveAttribute("aria-expanded", "false");

    fireEvent.mouseDown(header);
    fireEvent.click(header);

    expect(header).toHaveAttribute("aria-expanded", "true");
  });

  it("does not expand when the current interaction is selecting header text", () => {
    render(<TestHeader plainText="hello world" />);

    const header = getHeader();
    const headerText = screen.getByText("hello world");
    expect(header).toHaveAttribute("aria-expanded", "false");

    fireEvent.mouseDown(header);
    selectNodeContents(headerText);
    fireEvent.click(header);

    expect(window.getSelection()?.toString()).toContain("hello world");
    expect(header).toHaveAttribute("aria-expanded", "false");
  });

  it("preserves multiline header text without expanding it into per-line DOM nodes", () => {
    const { container } = render(<TestHeader plainText={"line 1\nline 2\nline 3"} />);

    const content = container.querySelector(".wb-turn-header-content");
    expect(content?.textContent).toBe("line 1\nline 2\nline 3");
    expect(content?.querySelectorAll("br")).toHaveLength(0);
  });

  it("copies the message when the copy button is clicked without expanding the header", async () => {
    render(<TestHeader plainText="copy me" />);

    const header = getHeader();
    const copyButton = screen.getByRole("button", { name: "Copy message" });
    expect(header).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(copyButton);

    expect(copyTextToClipboardMock).toHaveBeenCalledWith("copy me");
    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
  });

  it("does not expand when the copy button interaction starts with mouse down", async () => {
    render(<TestHeader plainText="copy me" />);

    const header = getHeader();
    const copyButton = screen.getByRole("button", { name: "Copy message" });
    expect(header).toHaveAttribute("aria-expanded", "false");

    fireEvent.mouseDown(copyButton);
    fireEvent.click(copyButton);

    expect(copyTextToClipboardMock).toHaveBeenCalledWith("copy me");
    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
  });

  it("does not expand when the icon glyph itself is clicked", async () => {
    render(<TestHeader plainText="copy me" />);

    const header = getHeader();
    const copyButton = screen.getByRole("button", { name: "Copy message" });
    const icon = copyButton.querySelector("svg");
    if (!(icon instanceof SVGElement)) {
      throw new Error("Expected copy icon svg to be rendered");
    }
    expect(header).toHaveAttribute("aria-expanded", "false");

    fireEvent.mouseDown(icon);
    fireEvent.click(icon);

    expect(copyTextToClipboardMock).toHaveBeenCalledWith("copy me");
    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
  });
});

describe("ThreadItemView", () => {
  it("wraps message rows in an explicit transcript shell", () => {
    const { container } = render(
      <ThreadItemView
        item={{
          kind: "message",
          id: "message-1",
          role: "user",
          content: Array.from({ length: 24 }, (_, index) => `line ${index + 1}`).join("\n"),
          attachments: [],
          created_at: "2025-01-01T00:00:00.000Z",
        }}
        worktreeId={null}
        onFileOpenError={() => {}}
        messageExpanded={false}
        onToggleMessageExpanded={() => {}}
      />,
    );

    const row = container.querySelector(".wb-message-row");
    expect(row).not.toBeNull();
    expect(row?.querySelector(".msg")).not.toBeNull();
    expect(screen.getByRole("button", { name: "Show more" })).toBeInTheDocument();
  });

  it("does not render a collapse toggle when long wrapped content would not actually truncate", () => {
    const wrappedParagraph = ["there are a few different issues on latest ctx app that i want to investigate with you", "", "wrapped paragraph ".repeat(160)].join("\n");

    render(
      <ThreadItemView
        item={{
          kind: "message",
          id: "message-wrapped",
          role: "user",
          content: wrappedParagraph,
          attachments: [],
          created_at: "2025-01-01T00:00:00.000Z",
        }}
        worktreeId={null}
        onFileOpenError={() => {}}
        messageExpanded={false}
        onToggleMessageExpanded={() => {}}
      />,
    );

    expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
  });

  it("renders giant expanded user messages as plain text instead of markdown", () => {
    const hugeTranscript = ["# Reference", "", ...Array.from({ length: 2200 }, (_, index) => `reference line ${index + 1}`)].join(
      "\n",
    );

    const { container } = render(
      <ThreadItemView
        item={{
          kind: "message",
          id: "message-huge",
          role: "user",
          content: hugeTranscript,
          attachments: [],
          created_at: "2025-01-01T00:00:00.000Z",
        }}
        worktreeId={null}
        onFileOpenError={() => {}}
        messageExpanded
        onToggleMessageExpanded={() => {}}
      />,
    );

    expect(container.querySelector(".wb-message-plain-text")?.textContent).toContain("reference line 2200");
    expect(container.querySelector(".wb-md-unordered-list")).toBeNull();
    expect(screen.getByRole("button", { name: "Show less" })).toBeInTheDocument();
  });
});

describe("AssistantEntry", () => {
  it("renders structured markdown for partial-looking assistant list content", () => {
    const { container } = render(
      <AssistantEntry
        content={"Before\n\n- partial item"}
        worktreeId={null}
        onFileOpenError={() => {}}
      />,
    );

    expect(container.querySelector("ul.wb-md-unordered-list")).not.toBeNull();
    expect(container.querySelector(".wb-md-list-item-marker")?.textContent).toBe("•");
    expect(container.textContent).toContain("partial item");
  });
});

describe("WorkbenchToolRow", () => {
  it("keeps action-style tool summaries on the primary line", () => {
    const onToggle = vi.fn();

    const { container } = render(
      <WorkbenchToolRow
        item={{
          kind: "tool",
          id: "tool-1",
          tool_call_id: "tool-call-1",
          created_at: "2025-01-01T00:00:00.000Z",
          updated_at: "2025-01-01T00:00:01.000Z",
          tool_kind: "execute",
          provider_tool_name: "Bash",
          title: "Ran",
          subtitle: "/bin/zsh -lc pwd",
          status: "running",
          locations: [],
          input: { command: "pwd" },
          output_text: "",
          raw: null,
          updates_seen: 1,
          has_details: true,
        }}
        verbosity="default"
        expanded={false}
        onToggle={onToggle}
      />,
    );

    const mainline = container.querySelector(".wb-tool-mainline");
    expect(mainline?.textContent).toContain("Ran");
    expect(mainline?.textContent).toContain("/bin/zsh -lc pwd");
    expect(container.querySelector(".wb-tool-description")).toBeNull();
  });

  it("keeps the tool description inline on the first line", () => {
    const onToggle = vi.fn();

    const { container } = render(
      <WorkbenchToolRow
        item={{
          kind: "tool",
          id: "tool-1",
          tool_call_id: "tool-call-1",
          created_at: "2025-01-01T00:00:00.000Z",
          updated_at: "2025-01-01T00:00:01.000Z",
          tool_kind: "execute",
          provider_tool_name: "Bash",
          title: "Bash",
          subtitle: "Get current working directory",
          status: "running",
          locations: [],
          input: { command: "pwd" },
          output_text: "",
          raw: null,
          updates_seen: 1,
          has_details: true,
        }}
        verbosity="default"
        expanded={false}
        onToggle={onToggle}
      />,
    );

    const mainline = container.querySelector(".wb-tool-mainline");
    expect(mainline?.textContent).toContain("Bash");
    expect(mainline?.textContent).toContain("Get current working directory");
    expect(container.querySelector(".wb-tool-description")).toBeNull();
    expect(container.querySelector("button")).toBeNull();
  });

  it("does not render tool input or output details even when expanded", () => {
    const onToggle = vi.fn();

    const { container } = render(
      <WorkbenchToolRow
        item={{
          kind: "tool",
          id: "tool-1",
          tool_call_id: "tool-call-1",
          created_at: "2025-01-01T00:00:00.000Z",
          updated_at: "2025-01-01T00:00:01.000Z",
          tool_kind: "execute",
          provider_tool_name: "Bash",
          title: "Ran",
          subtitle: "pwd",
          status: "completed",
          locations: [],
          input: { command: "pwd" },
          output_text: "output",
          raw: null,
          updates_seen: 1,
          has_details: true,
        }}
        verbosity="verbose"
        expanded={true}
        onToggle={onToggle}
      />,
    );

    expect(container.textContent).not.toContain("Input");
    expect(container.textContent).not.toContain("Output");
    expect(container.querySelector(".wb-tool-details")).toBeNull();
  });

  it("renders Claude Agent tool labels as Subagent when a provider label is available", () => {
    const onToggle = vi.fn();

    const { container } = render(
      <WorkbenchToolRow
        item={{
          kind: "tool",
          id: "tool-unknown-agent",
          tool_call_id: "tool-call-unknown-agent",
          created_at: "2025-01-01T00:00:00.000Z",
          updated_at: "2025-01-01T00:00:01.000Z",
          tool_kind: "unknown",
          provider_tool_name: "Agent",
          title: "unknown",
          subtitle: "Read agent basics context",
          status: "running",
          locations: [],
          input: { description: "Read agent basics context" },
          output_text: "",
          raw: null,
          updates_seen: 1,
          has_details: true,
        }}
        verbosity="default"
        expanded={false}
        onToggle={onToggle}
      />,
    );

    const mainline = container.querySelector(".wb-tool-mainline");
    expect(mainline?.textContent).toContain("Subagent");
    expect(mainline?.textContent).toContain("Read agent basics context");
    expect(mainline?.textContent).not.toContain("Agent");
    expect(mainline?.textContent?.toLowerCase()).not.toContain("unknown");
  });

  it("renders top-level Agent tool titles as Subagent with a single short preview", () => {
    const onToggle = vi.fn();

    const { container } = render(
      <WorkbenchToolRow
        item={{
          kind: "tool",
          id: "tool-agent-title",
          tool_call_id: "tool-call-agent-title",
          created_at: "2025-01-01T00:00:00.000Z",
          updated_at: "2025-01-01T00:00:01.000Z",
          tool_kind: "Agent",
          provider_tool_name: "Agent",
          title: "Agent",
          subtitle: "Explore repo structure",
          status: "running",
          locations: [],
          input: { description: "Explore repo structure" },
          output_text: "",
          raw: null,
          updates_seen: 1,
          has_details: true,
        }}
        verbosity="default"
        expanded={false}
        onToggle={onToggle}
      />,
    );

    const mainline = container.querySelector(".wb-tool-mainline");
    expect(mainline?.textContent).toContain("Subagent");
    expect(mainline?.textContent).toContain("Explore repo structure");
    expect(mainline?.textContent).not.toContain("Agent");
  });

  it("falls back to a generic tool label instead of rendering bare unknown", () => {
    const onToggle = vi.fn();

    const { container } = render(
      <WorkbenchToolRow
        item={{
          kind: "tool",
          id: "tool-generic-unknown",
          tool_call_id: "tool-call-generic-unknown",
          created_at: "2025-01-01T00:00:00.000Z",
          updated_at: "2025-01-01T00:00:01.000Z",
          tool_kind: "unknown",
          provider_tool_name: "unknown",
          title: "unknown",
          subtitle: "Read specs context",
          status: "running",
          locations: [],
          input: { description: "Read specs context" },
          output_text: "",
          raw: null,
          updates_seen: 1,
          has_details: true,
        }}
        verbosity="default"
        expanded={false}
        onToggle={onToggle}
      />,
    );

    const mainline = container.querySelector(".wb-tool-mainline");
    expect(mainline?.textContent).toContain("Tool");
    expect(mainline?.textContent).toContain("Read specs context");
    expect(mainline?.textContent?.toLowerCase()).not.toContain("unknown");
  });
});

describe("AssistantEntry", () => {
  it("always renders long completed assistant content without a collapse toggle", () => {
    const content = Array.from({ length: 24 }, (_, index) => `assistant line ${index + 1}`).join("\n");

    const { container } = render(
      <AssistantEntry
        content={content}
        worktreeId={null}
        onFileOpenError={() => {}}
      />,
    );

    expect(container.textContent).toContain("assistant line 1");
    expect(container.textContent).toContain("assistant line 24");
    expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
  });
});
