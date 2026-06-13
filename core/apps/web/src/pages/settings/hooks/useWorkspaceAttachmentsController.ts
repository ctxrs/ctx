import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  createWorkspaceAttachment,
  deleteWorkspaceAttachment,
  idToString,
  syncWorkspaceAttachments,
  type WorkspaceAttachment,
} from "../../../api/client";
import { guessAttachmentName } from "../SettingsPage.utils";

type UseWorkspaceAttachmentsControllerArgs = {
  workspaceId: string | null;
  enabled: boolean;
};

type WorkspaceAttachmentsController = {
  attachmentSource: string;
  setAttachmentSource: (value: string) => void;
  attachmentName: string;
  setAttachmentName: (value: string) => void;
  attachmentRevision: string;
  setAttachmentRevision: (value: string) => void;
  handleAddAttachment: () => Promise<boolean>;
  attachmentBusy: boolean;
  syncWorkspaceAttachmentsNow: () => Promise<void>;
  attachmentSyncBusy: boolean;
  attachmentsLoading: boolean;
  attachments: WorkspaceAttachment[];
  attachmentDeleteBusy: Record<string, boolean>;
  handleRemoveAttachment: (attachment: WorkspaceAttachment) => Promise<void>;
  docsAttachmentSource: string;
  setDocsAttachmentSource: (value: string) => void;
  docsAttachmentName: string;
  setDocsAttachmentName: (value: string) => void;
  handleAddDocsAttachment: () => Promise<boolean>;
  docsAttachmentBusy: boolean;
  attachmentsError: string | null;
};

const isAttachmentSyncing = (status?: WorkspaceAttachment["status"]) => status === "pending" || status === "syncing";

const messageFromError = (error: unknown): string => {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return String(error);
};

export function useWorkspaceAttachmentsController({
  workspaceId,
  enabled,
}: UseWorkspaceAttachmentsControllerArgs): WorkspaceAttachmentsController {
  const [attachments, setAttachments] = useState<WorkspaceAttachment[]>([]);
  const [attachmentsLoading, setAttachmentsLoading] = useState(false);
  const [attachmentsError, setAttachmentsError] = useState<string | null>(null);

  const [attachmentName, setAttachmentName] = useState("");
  const [attachmentSource, setAttachmentSource] = useState("");
  const [attachmentRevision, setAttachmentRevision] = useState("");
  const [attachmentBusy, setAttachmentBusy] = useState(false);

  const [docsAttachmentName, setDocsAttachmentName] = useState("");
  const [docsAttachmentSource, setDocsAttachmentSource] = useState("");
  const [docsAttachmentBusy, setDocsAttachmentBusy] = useState(false);

  const [attachmentSyncBusy, setAttachmentSyncBusy] = useState(false);
  const [attachmentDeleteBusy, setAttachmentDeleteBusy] = useState<Record<string, boolean>>({});

  const pollTimeoutRef = useRef<number | null>(null);

  const refreshWorkspaceAttachments = useCallback(
    async (opts?: { refresh?: boolean; silent?: boolean }) => {
      if (!workspaceId) return;
      if (!opts?.silent) {
        setAttachmentsLoading(true);
        setAttachmentsError(null);
      }
      try {
        const next = await syncWorkspaceAttachments(workspaceId, Boolean(opts?.refresh));
        setAttachments(next);
      } catch (error) {
        if (!opts?.silent) {
          setAttachmentsError(messageFromError(error));
        }
      } finally {
        if (!opts?.silent) {
          setAttachmentsLoading(false);
        }
      }
    },
    [workspaceId],
  );

  const syncWorkspaceAttachmentsNow = useCallback(async () => {
    if (!workspaceId) return;
    setAttachmentSyncBusy(true);
    try {
      await refreshWorkspaceAttachments({ refresh: true });
    } finally {
      setAttachmentSyncBusy(false);
    }
  }, [refreshWorkspaceAttachments, workspaceId]);

  const handleAddAttachment = useCallback(async () => {
    if (!workspaceId) return false;
    const source = attachmentSource.trim();
    const revision = attachmentRevision.trim();
    const name = attachmentName.trim() || guessAttachmentName(source);
    if (!source) {
      setAttachmentsError("Repository URL is required.");
      return false;
    }
    if (!name) {
      setAttachmentsError("Attachment name is required.");
      return false;
    }
    setAttachmentBusy(true);
    setAttachmentsError(null);
    try {
      const next = await createWorkspaceAttachment(workspaceId, {
        kind: "reference_repo",
        name,
        source,
        revision: revision || null,
      });
      setAttachments(next);
      setAttachmentName("");
      setAttachmentSource("");
      setAttachmentRevision("");
      return true;
    } catch (error) {
      setAttachmentsError(messageFromError(error));
      return false;
    } finally {
      setAttachmentBusy(false);
    }
  }, [workspaceId, attachmentSource, attachmentRevision, attachmentName]);

  const handleAddDocsAttachment = useCallback(async () => {
    if (!workspaceId) return false;
    const source = docsAttachmentSource.trim();
    const name = docsAttachmentName.trim() || guessAttachmentName(source);
    if (!source) {
      setAttachmentsError("Docs URL is required.");
      return false;
    }
    if (!name) {
      setAttachmentsError("Attachment name is required.");
      return false;
    }
    setDocsAttachmentBusy(true);
    setAttachmentsError(null);
    try {
      const next = await createWorkspaceAttachment(workspaceId, {
        kind: "doc_mirror",
        name,
        source,
      });
      setAttachments(next);
      setDocsAttachmentName("");
      setDocsAttachmentSource("");
      return true;
    } catch (error) {
      setAttachmentsError(messageFromError(error));
      return false;
    } finally {
      setDocsAttachmentBusy(false);
    }
  }, [workspaceId, docsAttachmentSource, docsAttachmentName]);

  const handleRemoveAttachment = useCallback(
    async (attachment: WorkspaceAttachment) => {
      if (!workspaceId) return;
      const id = idToString(attachment.id);
      setAttachmentDeleteBusy((prev) => ({ ...prev, [id]: true }));
      setAttachmentsError(null);
      try {
        const next = await deleteWorkspaceAttachment(workspaceId, {
          kind: attachment.kind,
          name: attachment.name,
        });
        setAttachments(next);
      } catch (error) {
        setAttachmentsError(messageFromError(error));
      } finally {
        setAttachmentDeleteBusy((prev) => {
          const copy = { ...prev };
          delete copy[id];
          return copy;
        });
      }
    },
    [workspaceId],
  );

  useEffect(() => {
    setAttachments([]);
    setAttachmentsError(null);
  }, [workspaceId]);

  useEffect(() => {
    if (!enabled || !workspaceId) return;
    refreshWorkspaceAttachments().catch(() => {});
  }, [enabled, workspaceId, refreshWorkspaceAttachments]);

  const hasPendingAttachments = useMemo(
    () => attachments.some((attachment) => isAttachmentSyncing(attachment.status)),
    [attachments],
  );

  useEffect(() => {
    if (!enabled || !workspaceId) return;
    if (!hasPendingAttachments) {
      if (pollTimeoutRef.current) {
        window.clearTimeout(pollTimeoutRef.current);
        pollTimeoutRef.current = null;
      }
      return;
    }
    if (pollTimeoutRef.current) return;
    pollTimeoutRef.current = window.setTimeout(() => {
      pollTimeoutRef.current = null;
      refreshWorkspaceAttachments({ silent: true }).catch(() => {});
    }, 2000);
  }, [enabled, hasPendingAttachments, refreshWorkspaceAttachments, workspaceId]);

  useEffect(() => {
    return () => {
      if (pollTimeoutRef.current) {
        window.clearTimeout(pollTimeoutRef.current);
      }
    };
  }, []);

  return {
    attachmentSource,
    setAttachmentSource,
    attachmentName,
    setAttachmentName,
    attachmentRevision,
    setAttachmentRevision,
    handleAddAttachment,
    attachmentBusy,
    syncWorkspaceAttachmentsNow,
    attachmentSyncBusy,
    attachmentsLoading,
    attachments,
    attachmentDeleteBusy,
    handleRemoveAttachment,
    docsAttachmentSource,
    setDocsAttachmentSource,
    docsAttachmentName,
    setDocsAttachmentName,
    handleAddDocsAttachment,
    docsAttachmentBusy,
    attachmentsError,
  };
}
