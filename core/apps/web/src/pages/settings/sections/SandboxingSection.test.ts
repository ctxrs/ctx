import { render, screen } from "@testing-library/react";
import { createElement } from "react";
import { describe, expect, it } from "vitest";

import { formatResolvedMachineMemory, MACHINE_MEMORY_DESCRIPTION, SandboxingSection } from "./SandboxingSection";

describe("SandboxingSection", () => {
  it("shows resolved machine memory as read-only display data", () => {
    render(
      createElement(SandboxingSection, {
        loaded: true,
        resolvedMachineMemoryMb: 6144,
        idleShutdownSeconds: "900",
        onIdleShutdownSecondsChange: () => {},
        hostPressureSwapThresholdMb: "1024",
        onHostPressureSwapThresholdMbChange: () => {},
        canSaveMachineSettings: true,
      }),
    );

    expect(screen.getByText("Machine memory target")).toBeTruthy();
    expect(screen.getByText("6 GiB")).toBeTruthy();
    expect(screen.queryByText("Provider control")).toBeNull();
    expect(screen.queryByText(/memory profile/i)).toBeNull();
    expect(screen.queryByText(/custom memory/i)).toBeNull();
    expect(screen.getByDisplayValue("900")).toBeTruthy();
    expect(screen.getByDisplayValue("1024")).toBeTruthy();
  });
});

describe("formatResolvedMachineMemory", () => {
  it("renders a backend-resolved sandbox memory amount without exposing formulas", () => {
    expect(formatResolvedMachineMemory(6144)).toBe("6 GiB");
  });

  it("falls back to an automatic label when the backend does not provide a resolved amount", () => {
    expect(formatResolvedMachineMemory(null)).toBe("Automatic");
  });

  it("keeps the memory description platform-neutral", () => {
    expect(MACHINE_MEMORY_DESCRIPTION).not.toMatch(/\bMac\b/i);
    expect(MACHINE_MEMORY_DESCRIPTION).toContain("local sandbox runtime");
  });
});
