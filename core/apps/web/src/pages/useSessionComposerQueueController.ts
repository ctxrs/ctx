import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import { flushSync } from "react-dom";
import {
  deleteMessage,
  type Message,
  type MessageAttachment,
  postMessage,
  type Session,
  idToString,
  interruptSession,
} from "../api/client";
import type { SessionSupervisor } from "../state/sessionSupervisor";
import { randomUuid } from "../utils/randomUuid";
import { errorMessage } from "../utils/errorMessage";
import { buildOptimisticUserMessage } from "./sessionView/SessionPage.optimisticMessage";
import { getQueuedAttachments } from "./sessionView/SessionQueuePanel";
import {
  clearInterruptPendingMetric,
  noteInterruptClicked,
  noteInterruptPendingVisible,
} from "../state/foregroundFreshnessTelemetry";

type OptimisticOverlaySupervisor = Pick<
  SessionSupervisor,
  | "addOptimisticQueueRemovalId"
  | "removeOptimisticQueueRemovalId"
  | "removeOptimisticQueuedMessage"
  | "removeOptimisticThreadMessage"
  | "upsertOptimisticQueuedMessage"
  | "upsertOptimisticThreadMessage"
>;

type Params = {
  sessionId: string;
  session: Session | null;
  supervisor: OptimisticOverlaySupervisor;
  input: string;
  setInput: (next: string) => void;
  draftAttachments: MessageAttachment[];
  setDraftAttachments: Dispatch<SetStateAction<MessageAttachment[]>>;
  optimisticThreadMessages: Message[];
  optimisticQueuedMessages: Message[];
  messageCount: number;
  turnCount: number;
  hasActiveTurn: boolean;
  queuedMessagesEnabled: boolean;
  currentModelId: string;
  interruptSessionId: string;
  resolveSendText: () => Promise<string>;
  setAtBottom: Dispatch<SetStateAction<boolean>>;
  onDraftPersistNow?: (() => void | Promise<void>) | null;
  onSendStarted?: (() => void) | null;
};

type Result = {
  sendBusy: boolean;
  sendError: string | null;
  setSendError: (next: string | null) => void;
  queueActionBusy: boolean;
  interruptPending: boolean;
  pendingQueueMessageIdSet: Set<string>;
  sendNow: () => Promise<void>;
  onRemoveQueued: (messageId: string) => Promise<void>;
  onEditQueued: (message: Message) => Promise<void>;
  onSendQueuedNow: (message: Message) => Promise<void>;
  onInterruptSession: (() => Promise<void>) | null;
};

type PendingSessionHandoff = {
  fromSessionId: string;
  messageIds: string[];
};

const shouldKeepQueueRemovalOnError = (error: unknown) => {
  const msg = errorMessage(error);
  return msg.startsWith("400") || msg.startsWith("404");
};

function shouldCarryPendingMessagesAcrossSessionChange({
  previousSessionId,
  nextSessionId,
  handoff,
  optimisticThreadMessages,
  messageCount,
  turnCount,
}: {
  previousSessionId: string;
  nextSessionId: string;
  handoff: PendingSessionHandoff | null;
  optimisticThreadMessages: Message[];
  messageCount: number;
  turnCount: number;
}): boolean {
  if (!previousSessionId || previousSessionId === nextSessionId) return false;
  if (!handoff || handoff.fromSessionId !== previousSessionId) return false;
  if (messageCount > 0 || turnCount > 0) return false;
  const optimisticIds = new Set(
    optimisticThreadMessages
      .map((message) => idToString(message.id))
      .filter((messageId): messageId is string => messageId.length > 0),
  );
  return handoff.messageIds.some((messageId) => optimisticIds.has(messageId));
}

export function useSessionComposerQueueController(params: Params): Result {
  const {
    sessionId,
    session,
    supervisor,
    input,
    setInput,
    draftAttachments,
    setDraftAttachments,
    optimisticThreadMessages,
    optimisticQueuedMessages,
    messageCount,
    turnCount,
    hasActiveTurn,
    queuedMessagesEnabled,
    currentModelId,
    interruptSessionId,
    resolveSendText,
    setAtBottom,
    onDraftPersistNow,
    onSendStarted,
  } = params;
  const [sendBusy, setSendBusy] = useState(false);
  const sendBusyRef = useRef(false);
  const [sendError, setSendErrorState] = useState<string | null>(null);
  const [queueActionBusyId, setQueueActionBusyId] = useState<string | null>(null);
  const [interruptPending, setInterruptPending] = useState(false);
  const previousSessionIdRef = useRef(sessionId);
  const supervisorRef = useRef(supervisor);
  const optimisticThreadMessagesRef = useRef(optimisticThreadMessages);
  const messageCountRef = useRef(messageCount);
  const turnCountRef = useRef(turnCount);
  const pendingSessionHandoffRef = useRef<PendingSessionHandoff | null>(null);
  const pendingInterruptSessionIdRef = useRef<string | null>(null);
  const latestInterruptClickSessionIdRef = useRef<string | null>(null);

  useEffect(() => {
    supervisorRef.current = supervisor;
  }, [supervisor]);

  useEffect(() => {
    messageCountRef.current = messageCount;
  }, [messageCount]);

  useEffect(() => {
    turnCountRef.current = turnCount;
  }, [turnCount]);

  useEffect(() => {
    const previousSessionId = previousSessionIdRef.current;
    previousSessionIdRef.current = sessionId;
    const handoff = pendingSessionHandoffRef.current;
    const shouldCarryPendingMessages = shouldCarryPendingMessagesAcrossSessionChange({
      previousSessionId,
      nextSessionId: sessionId,
      handoff,
      optimisticThreadMessages: optimisticThreadMessagesRef.current,
      messageCount: messageCountRef.current,
      turnCount: turnCountRef.current,
    });
    if (shouldCarryPendingMessages && handoff) {
      const handoffIds = new Set(handoff.messageIds);
      const carriedMessages: Message[] = [];
      for (const message of optimisticThreadMessagesRef.current) {
        const messageId = idToString(message.id);
        if (!messageId || !handoffIds.has(messageId)) continue;
        const carriedMessage = {
          ...message,
          session_id: sessionId,
        };
        supervisorRef.current.removeOptimisticThreadMessage(previousSessionId, messageId);
        supervisorRef.current.upsertOptimisticThreadMessage(sessionId, carriedMessage);
        carriedMessages.push(carriedMessage);
      }
      optimisticThreadMessagesRef.current = carriedMessages;
      pendingSessionHandoffRef.current = {
        fromSessionId: sessionId,
        messageIds: handoff.messageIds,
      };
    } else {
      optimisticThreadMessagesRef.current = optimisticThreadMessages;
      pendingSessionHandoffRef.current = null;
    }
    setSendBusy(false);
    sendBusyRef.current = false;
    setSendErrorState(null);
    setQueueActionBusyId(null);
    const pendingInterruptSessionId = pendingInterruptSessionIdRef.current;
    if (pendingInterruptSessionId) {
      clearInterruptPendingMetric(pendingInterruptSessionId);
      pendingInterruptSessionIdRef.current = null;
      if (latestInterruptClickSessionIdRef.current === pendingInterruptSessionId) {
        latestInterruptClickSessionIdRef.current = null;
      }
    }
    setInterruptPending(false);
  }, [sessionId]);

  useEffect(() => {
    optimisticThreadMessagesRef.current = optimisticThreadMessages;
  }, [optimisticThreadMessages]);

  useEffect(() => {
    const handoff = pendingSessionHandoffRef.current;
    if (!handoff || handoff.fromSessionId !== sessionId) return;
    const optimisticIds = new Set(
      optimisticThreadMessages
        .map((message) => idToString(message.id))
        .filter((messageId): messageId is string => messageId.length > 0),
    );
    if (handoff.messageIds.some((messageId) => optimisticIds.has(messageId))) return;
    pendingSessionHandoffRef.current = null;
  }, [optimisticThreadMessages, sessionId]);

  useEffect(() => {
    if (!hasActiveTurn) {
      const pendingInterruptSessionId =
        pendingInterruptSessionIdRef.current ?? latestInterruptClickSessionIdRef.current;
      if (pendingInterruptSessionId) {
        clearInterruptPendingMetric(pendingInterruptSessionId);
        pendingInterruptSessionIdRef.current = null;
        if (latestInterruptClickSessionIdRef.current === pendingInterruptSessionId) {
          latestInterruptClickSessionIdRef.current = null;
        }
      }
      setInterruptPending(false);
    }
  }, [hasActiveTurn]);

  useLayoutEffect(() => {
    const pendingInterruptSessionId =
      pendingInterruptSessionIdRef.current ?? latestInterruptClickSessionIdRef.current;
    if (!pendingInterruptSessionId) return;
    if (interruptPending) {
      noteInterruptPendingVisible(pendingInterruptSessionId);
      pendingInterruptSessionIdRef.current = null;
      latestInterruptClickSessionIdRef.current = null;
    }
  }, [interruptPending]);

  const showInterruptPending = (targetSessionId: string) => {
    flushSync(() => {
      setInterruptPending(true);
    });
    noteInterruptPendingVisible(targetSessionId);
    pendingInterruptSessionIdRef.current = null;
    if (latestInterruptClickSessionIdRef.current === targetSessionId) {
      latestInterruptClickSessionIdRef.current = null;
    }
  };

  const pendingQueueMessageIdSet = useMemo(() => {
    return new Set(
      optimisticQueuedMessages
        .map((message) => idToString(message.id))
        .filter((messageId): messageId is string => messageId.length > 0),
    );
  }, [optimisticQueuedMessages]);

  const setSendBusySafe = (next: boolean) => {
    sendBusyRef.current = next;
    setSendBusy(next);
  };
  const setSendError = (next: string | null) => {
    setSendErrorState(next);
  };
  const queueActionBusy = queueActionBusyId !== null;
  const upsertOptimisticThreadMessageRef = (message: Message) => {
    const nextMessageId = idToString(message.id);
    if (!nextMessageId) {
      optimisticThreadMessagesRef.current = [...optimisticThreadMessagesRef.current, message];
      return;
    }
    const nextMessages = optimisticThreadMessagesRef.current.slice();
    const existingIndex = nextMessages.findIndex((entry) => idToString(entry.id) === nextMessageId);
    if (existingIndex >= 0) {
      nextMessages[existingIndex] = message;
    } else {
      nextMessages.push(message);
    }
    optimisticThreadMessagesRef.current = nextMessages;
  };
  const removeOptimisticThreadMessageRef = (messageId: string) => {
    if (!messageId) return;
    optimisticThreadMessagesRef.current = optimisticThreadMessagesRef.current.filter(
      (entry) => idToString(entry.id) !== messageId,
    );
  };
  const markQueueOptimisticallyRemoved = (messageId: string) => {
    if (!messageId) return;
    supervisor.addOptimisticQueueRemovalId(sessionId, messageId);
  };
  const rollbackOptimisticQueueRemoval = (messageId: string) => {
    if (!messageId) return;
    supervisor.removeOptimisticQueueRemovalId(sessionId, messageId);
  };

  const sendNow = async () => {
    if (!sessionId) return;
    if (sendBusyRef.current) return;
    if (hasActiveTurn && !queuedMessagesEnabled) {
      setSendErrorState("A turn is already running. Stop it or wait for it to finish.");
      return;
    }
    setSendBusySafe(true);
    let text = "";
    try {
      text = (await resolveSendText()).trim();
    } catch (error: unknown) {
      setSendErrorState(errorMessage(error));
      setSendBusySafe(false);
      return;
    }
    if (!text) {
      setSendBusySafe(false);
      return;
    }
    onSendStarted?.();
    const attachmentsToSend = draftAttachments.slice();
    const shouldQueue = hasActiveTurn && queuedMessagesEnabled;
    const requestedDelivery = shouldQueue ? "queued" : undefined;
    const messageId = randomUuid();
    const turnId = randomUuid();
    const optimisticMessage: Message = buildOptimisticUserMessage({
      messageId,
      sessionId,
      taskId: String(session?.task_id ?? ""),
      turnId,
      content: text,
      attachments: attachmentsToSend,
      delivery: shouldQueue ? "queued" : "immediate",
    });
    setSendErrorState(null);
    if (shouldQueue) {
      supervisor.upsertOptimisticQueuedMessage(sessionId, optimisticMessage);
      pendingSessionHandoffRef.current = null;
    } else {
      upsertOptimisticThreadMessageRef(optimisticMessage);
      supervisor.upsertOptimisticThreadMessage(sessionId, optimisticMessage);
      pendingSessionHandoffRef.current =
        messageCount === 0 && turnCount === 0
          ? {
              fromSessionId: sessionId,
              messageIds: [messageId],
            }
          : null;
    }
    setAtBottom(true);
    setInput("");
    setDraftAttachments([]);
    try {
      const posted = await postMessage(sessionId, text, requestedDelivery, attachmentsToSend, {
        id: messageId,
        turn_id: turnId,
        analytics: {
          providerId: session?.provider_id ?? undefined,
          modelId: currentModelId || undefined,
          reasoningEffort: session?.reasoning_effort ?? null,
          executionEnvironment: session?.execution_environment ?? undefined,
          sessionKind:
            session?.parent_session_id || session?.relationship === "sub_agent"
              ? "subagent"
              : "primary",
        },
      });
      if (shouldQueue) {
        supervisor.upsertOptimisticQueuedMessage(sessionId, posted);
      } else {
        upsertOptimisticThreadMessageRef(posted);
        supervisor.upsertOptimisticThreadMessage(sessionId, posted);
      }
      try {
        await onDraftPersistNow?.();
      } catch {
        // best-effort
      }
    } catch (error: unknown) {
      if (shouldQueue) {
        supervisor.removeOptimisticQueuedMessage(sessionId, messageId);
      } else {
        removeOptimisticThreadMessageRef(messageId);
        supervisor.removeOptimisticThreadMessage(sessionId, messageId);
        const handoff = pendingSessionHandoffRef.current;
        if (handoff?.messageIds.includes(messageId)) {
          pendingSessionHandoffRef.current = null;
        }
      }
      setInput(text);
      setDraftAttachments(attachmentsToSend);
      setSendErrorState(errorMessage(error));
    } finally {
      setSendBusySafe(false);
    }
  };

  const onRemoveQueued = async (messageId: string) => {
    if (!sessionId || !messageId || queueActionBusy) return;
    markQueueOptimisticallyRemoved(messageId);
    setQueueActionBusyId(messageId);
    setSendErrorState(null);
    try {
      await deleteMessage(sessionId, messageId);
      supervisor.removeOptimisticQueuedMessage(sessionId, messageId);
    } catch (error: unknown) {
      if (!shouldKeepQueueRemovalOnError(error)) {
        rollbackOptimisticQueueRemoval(messageId);
      }
      setSendErrorState(errorMessage(error));
    } finally {
      setQueueActionBusyId(null);
    }
  };

  const onEditQueued = async (message: Message) => {
    if (!sessionId || queueActionBusy) return;
    const messageId = idToString(message.id);
    if (!messageId) return;
    const attachments = getQueuedAttachments(message);
    setInput(message.content ?? "");
    setDraftAttachments(attachments);
    markQueueOptimisticallyRemoved(messageId);
    setQueueActionBusyId(messageId);
    setSendErrorState(null);
    try {
      await deleteMessage(sessionId, messageId);
      supervisor.removeOptimisticQueuedMessage(sessionId, messageId);
    } catch (error: unknown) {
      if (!shouldKeepQueueRemovalOnError(error)) {
        rollbackOptimisticQueueRemoval(messageId);
      }
      setSendErrorState(errorMessage(error));
    } finally {
      setQueueActionBusyId(null);
    }
  };

  const onSendQueuedNow = async (message: Message) => {
    if (!sessionId || queueActionBusy || sendBusyRef.current) return;
    const messageId = idToString(message.id);
    if (!messageId) return;
    const targetSessionId = interruptSessionId || sessionId;
    const attachments = getQueuedAttachments(message);
    const content = message.content ?? "";
    onSendStarted?.();
    markQueueOptimisticallyRemoved(messageId);
    setQueueActionBusyId(messageId);
    setSendErrorState(null);
    pendingSessionHandoffRef.current = null;
    try {
      pendingInterruptSessionIdRef.current = targetSessionId;
      latestInterruptClickSessionIdRef.current = targetSessionId;
      noteInterruptClicked(targetSessionId, "queued_action");
      showInterruptPending(targetSessionId);
      await interruptSession(targetSessionId);
    } catch (error: unknown) {
      clearInterruptPendingMetric(targetSessionId);
      if (latestInterruptClickSessionIdRef.current === targetSessionId) {
        latestInterruptClickSessionIdRef.current = null;
      }
      if (pendingInterruptSessionIdRef.current === targetSessionId) {
        pendingInterruptSessionIdRef.current = null;
      }
      setInterruptPending(false);
      rollbackOptimisticQueueRemoval(messageId);
      setSendErrorState(errorMessage(error));
      setQueueActionBusyId(null);
      return;
    }
    try {
      await deleteMessage(sessionId, messageId);
      supervisor.removeOptimisticQueuedMessage(sessionId, messageId);
    } catch (error: unknown) {
      clearInterruptPendingMetric(targetSessionId);
      if (latestInterruptClickSessionIdRef.current === targetSessionId) {
        latestInterruptClickSessionIdRef.current = null;
      }
      if (pendingInterruptSessionIdRef.current === targetSessionId) {
        pendingInterruptSessionIdRef.current = null;
      }
      setInterruptPending(false);
      if (!shouldKeepQueueRemovalOnError(error)) {
        rollbackOptimisticQueueRemoval(messageId);
      }
      setSendErrorState(errorMessage(error));
      setQueueActionBusyId(null);
      return;
    }
    setSendBusySafe(true);

    const optimisticMessageId = randomUuid();
    const turnId = randomUuid();
    const optimisticMessage: Message = buildOptimisticUserMessage({
      messageId: optimisticMessageId,
      sessionId,
      taskId: String(session?.task_id ?? ""),
      turnId,
      content,
      attachments,
      delivery: "immediate",
    });
    upsertOptimisticThreadMessageRef(optimisticMessage);
    supervisor.upsertOptimisticThreadMessage(sessionId, optimisticMessage);
    setAtBottom(true);

    try {
      const posted = await postMessage(sessionId, content, "immediate", attachments, {
        id: optimisticMessageId,
        turn_id: turnId,
        analytics: {
          providerId: session?.provider_id ?? undefined,
          modelId: currentModelId || undefined,
          reasoningEffort: session?.reasoning_effort ?? null,
          executionEnvironment: session?.execution_environment ?? undefined,
          sessionKind:
            session?.parent_session_id || session?.relationship === "sub_agent"
              ? "subagent"
              : "primary",
        },
      });
      upsertOptimisticThreadMessageRef(posted);
      supervisor.upsertOptimisticThreadMessage(sessionId, posted);
    } catch (error: unknown) {
      removeOptimisticThreadMessageRef(optimisticMessageId);
      supervisor.removeOptimisticThreadMessage(sessionId, optimisticMessageId);
      setSendErrorState(errorMessage(error));
    } finally {
      setSendBusySafe(false);
      setQueueActionBusyId(null);
    }
  };

  const onInterruptSession =
    interruptSessionId || interruptPending
      ? async () => {
        const targetSessionId = interruptSessionId || pendingInterruptSessionIdRef.current;
        if (!targetSessionId || interruptPending) return;
        pendingInterruptSessionIdRef.current = targetSessionId;
        latestInterruptClickSessionIdRef.current = targetSessionId;
        noteInterruptClicked(targetSessionId, "thread_header");
        showInterruptPending(targetSessionId);
        try {
          await interruptSession(targetSessionId);
        } catch (error: unknown) {
          clearInterruptPendingMetric(targetSessionId);
          if (latestInterruptClickSessionIdRef.current === targetSessionId) {
            latestInterruptClickSessionIdRef.current = null;
          }
          if (pendingInterruptSessionIdRef.current === targetSessionId) {
            pendingInterruptSessionIdRef.current = null;
          }
          setInterruptPending(false);
          setSendErrorState(errorMessage(error));
        }
      }
      : null;

  return {
    sendBusy,
    sendError,
    setSendError,
    queueActionBusy,
    interruptPending,
    pendingQueueMessageIdSet,
    sendNow,
    onRemoveQueued,
    onEditQueued,
    onSendQueuedNow,
    onInterruptSession,
  };
}
