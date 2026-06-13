import React, { createRef } from "react";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { PretextVirtualizerListMethods } from "@pretext-virtualizer/interface";
import type { WorkbenchListItem } from "../sessionView/SessionPage.types";
import type { WorkbenchMessageListContext } from "../sessionThread";
import { SessionThreadPane } from "./SessionThreadPane";

const sessionThreadMessageListSpy = vi.hoisted(() => vi.fn());
const sessionThreadMessageListMountSpy = vi.hoisted(() => vi.fn());
const sessionThreadMessageListUnmountSpy = vi.hoisted(() => vi.fn());

vi.mock("../SessionThreadMessageList", () => ({
  SessionThreadMessageList: (props: { sessionId: string }) => {
    sessionThreadMessageListSpy(props);
    React.useEffect(() => {
      sessionThreadMessageListMountSpy();
      return () => {
        sessionThreadMessageListUnmountSpy();
      };
    }, []);
    return <div data-testid="session-thread-message-list" data-session-id={props.sessionId} />;
  },
}));

const context: WorkbenchMessageListContext = {
  loaded: false,
  loadingOlder: false,
  expandedTurnHeaders: {},
  expandedTurnDetailsById: {},
  expandedToolById: {},
  expandedMessageById: {},
  turnToolsLoading: [],
  verbosity: "default",
};

const noopMethodsRef =
  createRef<PretextVirtualizerListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>();

describe("SessionThreadPane", () => {
  it("keeps the transcript list mounted while switching session ids", () => {
    const rendered = render(
      <SessionThreadPane
        sessionId="session-1"
        isActive
        style={{ height: 400 }}
        initialData={[
          {
            kind: "message",
            id: "message-1",
            role: "user",
            content: "hello",
            attachments: [],
            created_at: "2026-04-15T00:00:00Z",
          },
        ]}
        itemContent={() => null}
        itemIdentity={(item) => item.id}
        itemKey={(item) => item.id}
        increaseViewportBy={240}
        initialLocation={null}
        threadProjectionOp={{
          kind: "noop",
          projectionRevision: 1,
          changedItemIds: [],
          remeasureItemIds: [],
        }}
        context={{ ...context, loaded: true }}
        onScroll={() => {}}
        onRenderedDataChange={() => {}}
        methodsRef={noopMethodsRef}
        licenseKey=""
        shortSizeAlign="top"
      >
        <div />
      </SessionThreadPane>,
    );

    expect(sessionThreadMessageListMountSpy).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("session-thread-message-list")).toHaveAttribute("data-session-id", "session-1");

    rendered.rerender(
      <SessionThreadPane
        sessionId="session-2"
        isActive
        style={{ height: 400 }}
        initialData={[
          {
            kind: "message",
            id: "message-2",
            role: "assistant",
            content: "world",
            attachments: [],
            created_at: "2026-04-15T00:00:01Z",
          },
        ]}
        itemContent={() => null}
        itemIdentity={(item) => item.id}
        itemKey={(item) => item.id}
        increaseViewportBy={240}
        initialLocation={null}
        threadProjectionOp={{
          kind: "noop",
          projectionRevision: 2,
          changedItemIds: [],
          remeasureItemIds: [],
        }}
        context={{ ...context, loaded: true }}
        onScroll={() => {}}
        onRenderedDataChange={() => {}}
        methodsRef={noopMethodsRef}
        licenseKey=""
        shortSizeAlign="top"
      >
        <div />
      </SessionThreadPane>,
    );

    expect(sessionThreadMessageListSpy).toHaveBeenLastCalledWith(
      expect.objectContaining({ sessionId: "session-2" }),
    );
    expect(screen.getByTestId("session-thread-message-list")).toHaveAttribute("data-session-id", "session-2");
    expect(sessionThreadMessageListMountSpy).toHaveBeenCalledTimes(1);
    expect(sessionThreadMessageListUnmountSpy).not.toHaveBeenCalled();
  });

  it("keeps the pretext scroller unmounted while the transcript is still hydrating with no initial items", () => {
    const { container } = render(
      <SessionThreadPane
        sessionId="session-1"
        isActive
        style={{ height: 400 }}
        initialData={[]}
        itemContent={() => null}
        itemIdentity={(item) => item.id}
        itemKey={(item) => item.id}
        increaseViewportBy={240}
        initialLocation={null}
        threadProjectionOp={{
          kind: "noop",
          projectionRevision: 0,
          changedItemIds: [],
          remeasureItemIds: [],
        }}
        context={context}
        onScroll={() => {}}
        onRenderedDataChange={() => {}}
        methodsRef={noopMethodsRef}
        licenseKey=""
        shortSizeAlign="top"
      >
        <div data-testid="bottom" />
      </SessionThreadPane>,
    );

    expect(screen.queryByTestId("session-thread-message-list")).not.toBeInTheDocument();
    expect(container.querySelector(".wb-session-thread-hydrating")).not.toBeNull();
    expect(screen.getByTestId("bottom")).toBeInTheDocument();
  });

  it("keeps the pretext scroller hidden for an empty loaded transcript", () => {
    render(
      <SessionThreadPane
        sessionId="session-1"
        isActive
        style={{ height: 400 }}
        initialData={[]}
        itemContent={() => null}
        itemIdentity={(item) => item.id}
        itemKey={(item) => item.id}
        increaseViewportBy={240}
        initialLocation={null}
        threadProjectionOp={{
          kind: "noop",
          projectionRevision: 0,
          changedItemIds: [],
          remeasureItemIds: [],
        }}
        context={{ ...context, loaded: true }}
        onScroll={() => {}}
        onRenderedDataChange={() => {}}
        methodsRef={noopMethodsRef}
        licenseKey=""
        shortSizeAlign="top"
      >
        <div />
      </SessionThreadPane>,
    );

    expect(screen.queryByTestId("session-thread-message-list")).not.toBeInTheDocument();
  });

  it("mounts the pretext scroller once the transcript has initial items", () => {
    render(
      <SessionThreadPane
        sessionId="session-1"
        isActive
        style={{ height: 400 }}
        initialData={[
          {
            kind: "message",
            id: "message-1",
            role: "user",
            content: "hello",
            attachments: [],
            created_at: "2026-04-15T00:00:00Z",
          },
        ]}
        itemContent={() => null}
        itemIdentity={(item) => item.id}
        itemKey={(item) => item.id}
        increaseViewportBy={240}
        initialLocation={null}
        threadProjectionOp={{
          kind: "noop",
          projectionRevision: 1,
          changedItemIds: [],
          remeasureItemIds: [],
        }}
        context={{ ...context, loaded: true }}
        onScroll={() => {}}
        onRenderedDataChange={() => {}}
        methodsRef={noopMethodsRef}
        licenseKey=""
        shortSizeAlign="top"
      >
        <div />
      </SessionThreadPane>,
    );

    expect(screen.getByTestId("session-thread-message-list")).toBeInTheDocument();
  });
});
