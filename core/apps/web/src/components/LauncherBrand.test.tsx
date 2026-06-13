import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import LauncherBrand from "./LauncherBrand";

describe("LauncherBrand", () => {
  it("renders a terminal-style block cursor", () => {
    const { container } = render(<LauncherBrand />);
    const cursor = container.querySelector(".cursor.cursor--block");
    const terminalText = container.querySelector(".terminal-content")?.textContent;

    expect(cursor).toBeInTheDocument();
    expect(cursor).toBeEmptyDOMElement();
    expect(terminalText).toBe("ctx");
  });
});
