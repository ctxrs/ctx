import { CornerUpRight, Pencil, Trash2, ChevronDown } from "lucide-react";
import { type Message, type MessageAttachment, idToString } from "../../api/client";
import { attachmentDisplayName, markdownToPlainText } from "./SessionPage.helpers";

export const getQueuedAttachments = (message: Message): MessageAttachment[] => {
  return Array.isArray(message.attachments) ? message.attachments : [];
};

const formatQueuedPreview = (message: Message, attachments: MessageAttachment[]): string => {
  const base = markdownToPlainText(message.content ?? "");
  const compact = base.replace(/\s+/g, " ").trim();
  if (compact) return compact;
  if (attachments.length > 0) return "Message with attachments";
  return "Queued message";
};

const formatQueuedAttachmentMeta = (attachments: MessageAttachment[]) => {
  if (attachments.length === 0) return null;
  const names = attachments.map((attachment) => attachmentDisplayName(attachment.name));
  const label = attachments.length === 1 ? "1 attachment" : `${attachments.length} attachments`;
  const detail = names.slice(0, 2).join(", ");
  const overflow = names.length > 2 ? ` +${names.length - 2}` : "";
  return {
    label,
    detail: detail ? `${detail}${overflow}` : null,
    title: names.join(", "),
  };
};

export function SessionQueuePanel({
  queue,
  pendingQueueMessageIdSet,
  queueActionBusy,
  sendBusy,
  onSendQueuedNow,
  onEditQueued,
  onRemoveQueued,
}: {
  queue: Message[];
  pendingQueueMessageIdSet: Set<string>;
  queueActionBusy: boolean;
  sendBusy: boolean;
  onSendQueuedNow: (message: Message) => void | Promise<void>;
  onEditQueued: (message: Message) => void | Promise<void>;
  onRemoveQueued: (messageId: string) => void | Promise<void>;
}) {
  if (queue.length === 0) return null;

  return (
    <div className="queue-panel card" aria-label="Queued messages">
      <div className="queue-header">
        <ChevronDown size={14} aria-hidden="true" />
        <span className="queue-header-title">{queue.length} Queued</span>
      </div>
      <ul className="queue-list" role="list">
        {queue.map((message, index) => {
          const messageId = idToString(message.id);
          const rowKey = messageId || `queued-${index}`;
          const attachments = getQueuedAttachments(message);
          const preview = formatQueuedPreview(message, attachments);
          const attachmentMeta = formatQueuedAttachmentMeta(attachments);
          const isPending = !!messageId && pendingQueueMessageIdSet.has(messageId);
          const canInteract = !!messageId && !isPending;
          const canSendNow = index === 0 && canInteract;
          return (
            <li key={rowKey} className="queue-item">
              <span className="queue-item-dot" aria-hidden="true" />
              <div className="queue-item-body">
                <div className="queue-item-content" title={preview}>
                  {preview}
                </div>
                {attachmentMeta ? (
                  <div className="queue-item-meta" title={attachmentMeta.title}>
                    <span>{attachmentMeta.label}</span>
                    {attachmentMeta.detail ? (
                      <span className="queue-item-meta-detail">{attachmentMeta.detail}</span>
                    ) : null}
                  </div>
                ) : null}
              </div>
              <div className="queue-item-actions">
                {canSendNow ? (
                  <button
                    type="button"
                    className="queue-action"
                    disabled={queueActionBusy || sendBusy}
                    onClick={() => void onSendQueuedNow(message)}
                    aria-label="Send now"
                    title="Send now"
                  >
                    <CornerUpRight size={14} aria-hidden="true" />
                  </button>
                ) : null}
                <button
                  type="button"
                  className="queue-action"
                  disabled={queueActionBusy || !canInteract}
                  onClick={() => void onEditQueued(message)}
                  aria-label="Edit queued message"
                  title="Edit"
                >
                  <Pencil size={14} aria-hidden="true" />
                </button>
                <button
                  type="button"
                  className="queue-action"
                  disabled={queueActionBusy || !canInteract}
                  onClick={() => messageId && void onRemoveQueued(messageId)}
                  aria-label="Cancel queued message"
                  title="Cancel"
                >
                  <Trash2 size={14} aria-hidden="true" />
                </button>
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
