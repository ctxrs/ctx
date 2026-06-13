// @vitest-environment jsdom

import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { MeasuredPretextRow } from "./pretextVirtualizerMeasuredRow";

const resizeObserverInstances: Array<{ callback: ResizeObserverCallback }> = [];

class ResizeObserverStub {
  callback: ResizeObserverCallback;

  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
    resizeObserverInstances.push({ callback });
  }

  observe(): void {}

  disconnect(): void {}
}

function createRect(height: number): DOMRect {
  return {
    x: 0,
    y: 0,
    width: 100,
    height,
    top: 0,
    left: 0,
    right: 100,
    bottom: height,
    toJSON: () => undefined,
  } satisfies DOMRect;
}

describe("MeasuredPretextRow", () => {
  beforeEach(() => {
    resizeObserverInstances.length = 0;
    Object.defineProperty(globalThis, "ResizeObserver", {
      configurable: true,
      value: ResizeObserverStub,
    });
    Object.defineProperty(globalThis, "requestAnimationFrame", {
      configurable: true,
      value: (callback: FrameRequestCallback) => {
        callback(0);
        return 1;
      },
    });
    Object.defineProperty(globalThis, "cancelAnimationFrame", {
      configurable: true,
      value: () => undefined,
    });
    Object.defineProperty(HTMLElement.prototype, "getBoundingClientRect", {
      configurable: true,
      value: () => createRect(40),
    });
  });

  it("emits mounted height mismatches through the production measurement wrapper", () => {
    const onHeightMismatch = vi.fn();

    render(
      <div data-pretext-virtualizer-row-shell="1">
        <MeasuredPretextRow
          id="row-1"
          itemKind="assistant"
          itemKey="row-1"
          plannedHeight={40}
          onHeightMismatch={onHeightMismatch}
        >
          <div>content</div>
        </MeasuredPretextRow>
      </div>,
    );

    const listItem = screen.getByRole("listitem");
    const shell = listItem.closest("[data-pretext-virtualizer-row-shell='1']") as HTMLElement;
    Object.defineProperty(listItem, "getBoundingClientRect", {
      configurable: true,
      value: () => createRect(60),
    });
    Object.defineProperty(shell, "getBoundingClientRect", {
      configurable: true,
      value: () => createRect(60),
    });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    expect(onHeightMismatch).toHaveBeenCalledWith({
      actualHeight: 60,
      plannedHeight: 40,
    });
  });

  it("keeps the observer stable when only the mismatch callback identity changes", () => {
    const firstMismatchHandler = vi.fn();
    const secondMismatchHandler = vi.fn();

    const { rerender } = render(
      <div data-pretext-virtualizer-row-shell="1">
        <MeasuredPretextRow
          id="row-1"
          itemKind="assistant"
          itemKey="row-1"
          plannedHeight={40}
          onHeightMismatch={firstMismatchHandler}
        >
          <div>content</div>
        </MeasuredPretextRow>
      </div>,
    );

    expect(resizeObserverInstances).toHaveLength(1);

    rerender(
      <div data-pretext-virtualizer-row-shell="1">
        <MeasuredPretextRow
          id="row-1"
          itemKind="assistant"
          itemKey="row-1"
          plannedHeight={40}
          onHeightMismatch={secondMismatchHandler}
        >
          <div>content</div>
        </MeasuredPretextRow>
      </div>,
    );

    expect(resizeObserverInstances).toHaveLength(1);

    const listItem = screen.getByRole("listitem");
    const shell = listItem.closest("[data-pretext-virtualizer-row-shell='1']") as HTMLElement;
    Object.defineProperty(listItem, "getBoundingClientRect", {
      configurable: true,
      value: () => createRect(60),
    });
    Object.defineProperty(shell, "getBoundingClientRect", {
      configurable: true,
      value: () => createRect(60),
    });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    expect(firstMismatchHandler).not.toHaveBeenCalled();
    expect(secondMismatchHandler).toHaveBeenCalledWith({
      actualHeight: 60,
      plannedHeight: 40,
    });
  });
});
