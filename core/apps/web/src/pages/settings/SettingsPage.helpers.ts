import type { WorkspaceAttachment } from "../../api/client";

export const formatAttachmentStatus = (status?: WorkspaceAttachment["status"]) => {
  switch (status) {
    case "pending":
      return "Pending";
    case "syncing":
      return "Syncing";
    case "ready":
      return "Ready";
    case "error":
      return "Error";
    default:
      return "Pending";
  }
};
