import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";

// Monaco needs a real browser; mock the boundary so we can assert which editor
// the pane chooses and what content it hands the side-by-side view.
vi.mock("@monaco-editor/react", () => ({
  default: (props: { value?: string }) => <div data-testid="inline-editor">{props.value}</div>,
  DiffEditor: (props: { original?: string; modified?: string }) => (
    <div data-testid="diff-editor" data-original={props.original} data-modified={props.modified} />
  ),
}));

import { DiffReviewPane } from "./DiffReviewPane";

const MODIFIED_DIFF = [
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

describe("DiffReviewPane view mode", () => {
  test("renders the inline editor by default and switches to a side-by-side editor", async () => {
    render(<DiffReviewPane diff={MODIFIED_DIFF} />);

    const expandButton = await screen.findByRole("button", { name: /expand file diff/i });
    fireEvent.click(expandButton);

    expect(await screen.findByTestId("inline-editor")).toBeInTheDocument();
    expect(screen.queryByTestId("diff-editor")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /split/i }));

    const diffEditor = await screen.findByTestId("diff-editor");
    expect(diffEditor).toBeInTheDocument();
    expect(screen.queryByTestId("inline-editor")).not.toBeInTheDocument();
    expect(diffEditor).toHaveAttribute("data-original", "line one\nold second\nline three");
    expect(diffEditor).toHaveAttribute("data-modified", "line one\nnew second\nline three");
  });
});
