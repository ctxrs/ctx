import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { TextInput, Textarea } from "./text-input";

describe("TextInput", () => {
  it("defaults to IDE-style browser text behavior", () => {
    render(<TextInput aria-label="Name" defaultValue="demo" />);

    const input = screen.getByLabelText("Name");
    expect(input).toHaveAttribute("autocomplete", "off");
    expect(input).toHaveAttribute("autocorrect", "off");
    expect(input).toHaveAttribute("autocapitalize", "none");
    expect(input).toHaveAttribute("spellcheck", "false");
  });

  it("keeps IDE-style browser behavior for password inputs too", () => {
    render(<TextInput aria-label="Secret" type="password" />);

    const input = screen.getByLabelText("Secret");
    expect(input).toHaveAttribute("autocomplete", "off");
    expect(input).toHaveAttribute("autocorrect", "off");
    expect(input).toHaveAttribute("autocapitalize", "none");
    expect(input).toHaveAttribute("spellcheck", "false");
  });

  it("allows explicit prop overrides", () => {
    render(<TextInput aria-label="Override" autoComplete="current-password" spellCheck />);

    const input = screen.getByLabelText("Override");
    expect(input).toHaveAttribute("autocomplete", "current-password");
    expect(input).toHaveAttribute("spellcheck", "true");
  });
});

describe("Textarea", () => {
  it("defaults to IDE-style browser text behavior", () => {
    render(<Textarea aria-label="Prompt" defaultValue="demo" />);

    const textarea = screen.getByLabelText("Prompt");
    expect(textarea).toHaveAttribute("autocomplete", "off");
    expect(textarea).toHaveAttribute("autocorrect", "off");
    expect(textarea).toHaveAttribute("autocapitalize", "none");
    expect(textarea).toHaveAttribute("spellcheck", "false");
  });
});
