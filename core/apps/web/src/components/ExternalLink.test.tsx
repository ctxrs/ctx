import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ExternalLink } from "./ExternalLink";
import { isDesktopApp, openExternalLink } from "../utils/desktop";

vi.mock("../utils/desktop", () => ({
  isDesktopApp: vi.fn(() => false),
  openExternalLink: vi.fn(async () => true),
}));

describe("ExternalLink", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(isDesktopApp).mockReturnValue(false);
    vi.mocked(openExternalLink).mockResolvedValue(true);
  });

  it("routes desktop clicks through the desktop browser bridge", () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);

    render(<ExternalLink href="https://example.com/docs">Example Docs</ExternalLink>);

    fireEvent.click(screen.getByRole("link", { name: "Example Docs" }));

    expect(openExternalLink).toHaveBeenCalledWith("https://example.com/docs");
  });

  it("preserves native browser behavior outside the desktop app", () => {
    render(<ExternalLink href="https://example.com/docs">Example Docs</ExternalLink>);

    fireEvent.click(screen.getByRole("link", { name: "Example Docs" }));

    expect(openExternalLink).not.toHaveBeenCalled();
  });

  it("respects a caller-prevented click", () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);

    render(
      <ExternalLink
        href="https://example.com/docs"
        onClick={(event) => event.preventDefault()}
      >
        Example Docs
      </ExternalLink>,
    );

    fireEvent.click(screen.getByRole("link", { name: "Example Docs" }));

    expect(openExternalLink).not.toHaveBeenCalled();
  });
});
