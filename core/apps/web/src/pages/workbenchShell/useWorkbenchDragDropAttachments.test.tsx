import React, { useEffect, useState } from "react";
import { render, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { MessageAttachment } from "../../api/client";
import { useWorkbenchDragDropAttachments } from "./useWorkbenchDragDropAttachments";

const registerDropScopeMock = vi.hoisted(() => vi.fn(() => () => {}));

vi.mock("../../utils/dragDropScopes", () => ({
  registerDropScope: registerDropScopeMock,
}));

function Harness({
  visible,
  onRendered,
}: {
  visible: boolean;
  onRendered?: () => void;
}) {
  const [scopeElement, setScopeElement] = useState<HTMLDivElement | null>(null);
  const [, setDraftAttachments] = useState<MessageAttachment[]>([]);

  useWorkbenchDragDropAttachments({
    scopeElement,
    activeTaskId: visible ? null : "task-1",
    setDraftAttachments,
  });

  useEffect(() => {
    onRendered?.();
  }, [onRendered, visible]);

  return visible ? <div data-testid="drop-scope" ref={setScopeElement} /> : null;
}

describe("useWorkbenchDragDropAttachments", () => {
  const getRegisteredElements = (): HTMLDivElement[] =>
    registerDropScopeMock.mock.calls.flatMap((call) => {
      const entry = (call as unknown[])[0] as { element?: HTMLDivElement | null } | undefined;
      return entry?.element instanceof HTMLDivElement ? [entry.element] : [];
    });

  beforeEach(() => {
    registerDropScopeMock.mockClear();
  });

  it("registers a fresh drop scope when the new-task composer remounts", async () => {
    const view = render(<Harness visible={false} />);
    expect(registerDropScopeMock).not.toHaveBeenCalled();

    view.rerender(<Harness visible />);

    await waitFor(() => {
      expect(registerDropScopeMock).toHaveBeenCalled();
    });
    const mountedElementsAfterFirstShow = getRegisteredElements();
    const firstElement = mountedElementsAfterFirstShow[0];
    expect(firstElement).toBeInstanceOf(HTMLDivElement);

    view.rerender(<Harness visible={false} />);
    view.rerender(<Harness visible />);

    await waitFor(() => {
      const mountedElements = getRegisteredElements();
      expect(mountedElements.length).toBeGreaterThanOrEqual(2);
    });
    const mountedElements = getRegisteredElements();
    const secondElement = mountedElements[mountedElements.length - 1];
    expect(secondElement).toBeInstanceOf(HTMLDivElement);
    expect(secondElement).not.toBe(firstElement);
  });
});
