import { normalizeAskUserQuestions } from "./askUserQuestionShared";
import {
  SESSION_THREAD_ASK_USER_ACTIONS_HEIGHT_PX,
  SESSION_THREAD_ASK_USER_CARD_GAP_PX,
  SESSION_THREAD_ASK_USER_CARD_PADDING_PX,
  SESSION_THREAD_ASK_USER_HINT_HEIGHT_PX,
  SESSION_THREAD_ASK_USER_MARGIN_VERTICAL_PX,
  SESSION_THREAD_ASK_USER_PANEL_HEIGHT_PX,
  SESSION_THREAD_ASK_USER_SHELL_HEIGHT_PX,
  SESSION_THREAD_ASK_USER_STATUS_HEIGHT_PX,
  SESSION_THREAD_ASK_USER_TABS_HEIGHT_PX,
  resolveSessionThreadAskUserCardWidth,
} from "../pages/sessionThread/sessionThreadLayoutTokens";

export type AskUserQuestionShellLayout = {
  cardWidth: number;
  outerHeight: number;
  shellHeight: number;
  tabsHeight: number;
  panelHeight: number;
  statusHeight: number;
  actionsHeight: number;
  hintHeight: number;
  cardGap: number;
  cardPadding: number;
};

export function resolveAskUserQuestionShellLayout(viewportWidth: number): AskUserQuestionShellLayout {
  return {
    cardWidth: resolveSessionThreadAskUserCardWidth(viewportWidth),
    outerHeight: SESSION_THREAD_ASK_USER_MARGIN_VERTICAL_PX + SESSION_THREAD_ASK_USER_SHELL_HEIGHT_PX,
    shellHeight: SESSION_THREAD_ASK_USER_SHELL_HEIGHT_PX,
    tabsHeight: SESSION_THREAD_ASK_USER_TABS_HEIGHT_PX,
    panelHeight: SESSION_THREAD_ASK_USER_PANEL_HEIGHT_PX,
    statusHeight: SESSION_THREAD_ASK_USER_STATUS_HEIGHT_PX,
    actionsHeight: SESSION_THREAD_ASK_USER_ACTIONS_HEIGHT_PX,
    hintHeight: SESSION_THREAD_ASK_USER_HINT_HEIGHT_PX,
    cardGap: SESSION_THREAD_ASK_USER_CARD_GAP_PX,
    cardPadding: SESSION_THREAD_ASK_USER_CARD_PADDING_PX,
  };
}

export function hasAskUserQuestions(input: unknown): boolean {
  return normalizeAskUserQuestions(input).length > 0;
}
