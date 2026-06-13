import { describe, expectTypeOf, it } from "vitest";
import type {
  AskUserQuestionAnswerState,
  ThreadItem,
  WorkbenchListItem,
  WorkbenchThreadView,
  WorkbenchTurnHeader,
} from "./index";

describe("transcriptTypes", () => {
  it("keeps tool groups, headers, and group items aligned", () => {
    expectTypeOf<Extract<ThreadItem, { kind: "tool_group" }>["tools"]>().toEqualTypeOf<
      Array<Extract<ThreadItem, { kind: "tool" }>>
    >();
    expectTypeOf<Extract<WorkbenchListItem, { kind: "turn_header" }>["header"]>().toEqualTypeOf<WorkbenchTurnHeader>();
    expectTypeOf<WorkbenchThreadView["groups"][number]["items"]>().toEqualTypeOf<ThreadItem[]>();
  });

  it("preserves ask-user answer maps as string records", () => {
    expectTypeOf<AskUserQuestionAnswerState["answers"]>().toEqualTypeOf<Record<string, string>>();
  });
});
