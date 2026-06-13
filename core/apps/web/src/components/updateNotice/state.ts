import { RESTART_READY_MESSAGE } from "./constants";

export type UpdateApplySource = "manual" | "desktop_auto" | "idle" | "forced";

export type NoticePhase =
  | "ready"
  | "checking"
  | "manual_installing"
  | "up_to_date"
  | "manual_failed"
  | "applying"
  | "restart_required";

export type NoticeUiState = {
  phase: NoticePhase;
  error: string | null;
  status: string | null;
  infoModalOpen: boolean;
};

export type NoticeUiAction =
  | { type: "apply_started" }
  | { type: "apply_failed"; message: string }
  | { type: "restart_failed"; message: string }
  | { type: "apply_completed" }
  | { type: "manual_check_started" }
  | { type: "manual_installing"; message: string }
  | { type: "manual_up_to_date"; message: string }
  | { type: "manual_failed"; message: string }
  | { type: "check_failed"; message: string }
  | { type: "check_recovered" }
  | { type: "restart_required"; message: string }
  | { type: "info_opened" }
  | { type: "info_closed" };

export const initialNoticeUiState: NoticeUiState = {
  phase: "ready",
  error: null,
  status: null,
  infoModalOpen: false,
};

export const noticeUiReducer = (
  state: NoticeUiState,
  action: NoticeUiAction,
): NoticeUiState => {
  switch (action.type) {
    case "apply_started":
      return { ...state, phase: "applying", error: null, status: null };
    case "apply_failed":
      return { ...state, phase: "ready", error: action.message, status: null };
    case "restart_failed":
      return {
        ...state,
        phase: "restart_required",
        error: action.message,
        status: RESTART_READY_MESSAGE,
      };
    case "apply_completed":
      return { ...state, phase: "ready", error: null, status: null };
    case "manual_check_started":
      return { ...state, phase: "checking", error: null, status: "Checking for updates..." };
    case "manual_installing":
      return { ...state, phase: "manual_installing", error: null, status: action.message };
    case "manual_up_to_date":
      return { ...state, phase: "up_to_date", error: null, status: action.message };
    case "manual_failed":
      return { ...state, phase: "manual_failed", error: action.message, status: null };
    case "check_failed":
      if (state.phase === "restart_required") return state;
      return { ...state, phase: "ready", error: action.message, status: null };
    case "check_recovered":
      if (state.phase !== "ready") return state;
      if (!state.error) return state;
      return { ...state, error: null };
    case "restart_required":
      return { ...state, phase: "restart_required", error: null, status: action.message };
    case "info_opened":
      return { ...state, infoModalOpen: true };
    case "info_closed":
      return { ...state, infoModalOpen: false };
    default:
      return state;
  }
};
