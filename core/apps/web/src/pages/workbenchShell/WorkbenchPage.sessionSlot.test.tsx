import React from "react";
import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { MessageAttachment } from "../../api/client";
import { WorkbenchSessionSlot } from "./WorkbenchPage.sessionSlot";
import { copyTextToClipboard } from "../../utils/clipboard";

const sessionViewSpy = vi.hoisted(() => vi.fn());
const sessionViewMountSpy = vi.hoisted(() => vi.fn());
const sessionViewUnmountSpy = vi.hoisted(() => vi.fn());
const setValueSpy = vi.hoisted(() => vi.fn());
const flushDraftSpy = vi.hoisted(() => vi.fn(async () => {}));

const initialAttachment: MessageAttachment = {
  kind: "image_ref",
  blob_id: "blob-1",
  mime_type: "image/png",
  name: "blob-1.png",
};

vi.mock("../sessionView", () => ({
  SessionView: (props: unknown) => {
    const sessionId = String((props as { sessionId?: string }).sessionId ?? "");
    sessionViewSpy(props);
    React.useEffect(() => {
      sessionViewMountSpy(sessionId);
      return () => {
        sessionViewUnmountSpy(sessionId);
      };
    }, [sessionId]);
    return <div data-testid="session-view" data-session-id={sessionId} />;
  },
}));

vi.mock("../../workbench/store", () => ({
  sessionDraftKey: (sessionId: string) => `session:${sessionId}`,
  useWorkbenchDraft: () => ({
    value: { text: "draft text", modeId: "default", attachments: [initialAttachment] },
    updatedAtMs: 0,
    setValue: setValueSpy,
  }),
  useWorkbenchStore: () => ({
    flushDraft: flushDraftSpy,
  }),
}));

vi.mock("../../utils/clipboard", () => ({
  copyTextToClipboard: vi.fn(async () => true),
}));

describe("WorkbenchSessionSlot", () => {
  beforeEach(() => {
    sessionViewSpy.mockClear();
    sessionViewMountSpy.mockClear();
    sessionViewUnmountSpy.mockClear();
    setValueSpy.mockClear();
    flushDraftSpy.mockClear();
  });

  it("remounts SessionView when the session id changes", () => {
    const rendered = render(<WorkbenchSessionSlot sessionId="session-1" />);

    expect(sessionViewMountSpy).toHaveBeenCalledWith("session-1");

    rendered.rerender(<WorkbenchSessionSlot sessionId="session-2" />);

    expect(sessionViewUnmountSpy).toHaveBeenCalledWith("session-1");
    expect(sessionViewMountSpy).toHaveBeenCalledWith("session-2");
  });

  it("passes attachment drafts through to SessionView and persists attachment edits", async () => {
    render(<WorkbenchSessionSlot sessionId="session-1" />);

    const props = sessionViewSpy.mock.calls.at(-1)?.[0] as
      | {
          draft: { text: string; modeId: string; attachments: MessageAttachment[] };
          onDraftAttachmentsChange: (attachments: MessageAttachment[]) => void;
        }
      | undefined;

    expect(props?.draft.attachments).toEqual([initialAttachment]);

    const nextAttachment: MessageAttachment = {
      kind: "image_ref",
      blob_id: "blob-2",
      mime_type: "image/png",
      name: "blob-2.png",
    };

    await act(async () => {
      props?.onDraftAttachmentsChange([nextAttachment]);
    });

    expect(setValueSpy).toHaveBeenCalledWith(expect.any(Function));
    const update = setValueSpy.mock.calls.at(-1)?.[0] as
      | ((prev: { text: string; modeId: string; attachments: MessageAttachment[] }) => {
          text: string;
          modeId: string;
          attachments: MessageAttachment[];
        })
      | undefined;
    expect(update?.({
      text: "draft text",
      modeId: "default",
      attachments: [initialAttachment],
    })).toEqual({
      text: "draft text",
      modeId: "default",
      attachments: [nextAttachment],
    });
  });

  it("uses functional draft updates so send-time clears cannot restore stale text", async () => {
    render(<WorkbenchSessionSlot sessionId="session-1" />);

    const props = sessionViewSpy.mock.calls.at(-1)?.[0] as
      | {
          onDraftChange: (text: string) => void;
          onDraftAttachmentsChange: (attachments: MessageAttachment[]) => void;
        }
      | undefined;

    await act(async () => {
      props?.onDraftChange("");
      props?.onDraftAttachmentsChange([]);
    });

    expect(setValueSpy).toHaveBeenNthCalledWith(1, expect.any(Function));
    expect(setValueSpy).toHaveBeenNthCalledWith(2, expect.any(Function));

    const firstUpdate = setValueSpy.mock.calls[0]?.[0] as
      | ((prev: { text: string; modeId: string; attachments: MessageAttachment[] }) => {
          text: string;
          modeId: string;
          attachments: MessageAttachment[];
        })
      | undefined;
    const secondUpdate = setValueSpy.mock.calls[1]?.[0] as
      | ((prev: { text: string; modeId: string; attachments: MessageAttachment[] }) => {
          text: string;
          modeId: string;
          attachments: MessageAttachment[];
        })
      | undefined;

    const afterTextClear = firstUpdate?.({
      text: "draft text",
      modeId: "default",
      attachments: [initialAttachment],
    });
    const afterAttachmentClear = secondUpdate?.(
      afterTextClear ?? {
        text: "draft text",
        modeId: "default",
        attachments: [initialAttachment],
      },
    );

    expect(afterTextClear).toEqual({
      text: "",
      modeId: "default",
      attachments: [initialAttachment],
    });
    expect(afterAttachmentClear).toEqual({
      text: "",
      modeId: "default",
      attachments: [],
    });
  });

  it("renders a dedicated failed-start surface instead of mounting SessionView", async () => {
    render(
      <WorkbenchSessionSlot
        sessionId="session-1"
        optimisticFailure={{ prompt: "hello", error: "model_id must be a concrete model id" }}
      />,
    );

    expect(sessionViewSpy).not.toHaveBeenCalled();
    expect(screen.getByRole("alert")).toHaveTextContent("Failed to start");
    expect(screen.getByRole("alert")).toHaveTextContent("model_id must be a concrete model id");

    await act(async () => {
      screen.getByRole("button", { name: "Copy prompt" }).click();
    });

    expect(vi.mocked(copyTextToClipboard)).toHaveBeenCalledWith("hello");
  });
});
