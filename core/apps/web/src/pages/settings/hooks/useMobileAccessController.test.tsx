import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { useMobileAccessController } from "./useMobileAccessController";

describe("useMobileAccessController", () => {
  it("reports managed mobile access as unavailable in the public export", async () => {
    const { result } = renderHook(() => useMobileAccessController({ getAuthToken: null }));

    await act(async () => {
      await result.current.handleEnableMobile();
    });

    expect(result.current.mobileEnableError).toBe(
      "Managed mobile access is not included in the public ADE export.",
    );
    expect(result.current.mobileQr).toBeNull();
    expect(result.current.mobileStatus).toBeNull();
  });
});
