import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AskUserQuestionCard } from "./AskUserQuestionCard";

function readShellChildClassNames(container: HTMLElement): string[] {
  const card = container.querySelector(".askq-card");
  if (!(card instanceof HTMLElement)) {
    throw new Error("Expected ask-user card");
  }
  return Array.from(card.children).map((child) =>
    child instanceof HTMLElement ? child.className : String(child.nodeName).toLowerCase(),
  );
}

describe("AskUserQuestionCard", () => {
  it("keeps a fixed shell structure while switching tabs and editing the other field", () => {
    const { container } = render(
      <AskUserQuestionCard
        input={{
          questions: [
            {
              header: "Priority",
              question: "Which option should I choose?",
              options: [{ label: "Fast", description: "Get it done quickly." }],
              allowOther: true,
            },
          ],
        }}
        readOnly={false}
        active={true}
        onSubmit={vi.fn(async () => undefined)}
        onCancel={vi.fn(async () => undefined)}
      />,
    );

    const initialStructure = readShellChildClassNames(container);
    expect(initialStructure).toEqual([
      "askq-tabs",
      "askq-panel-viewport",
      "askq-status-slot",
      "askq-actions",
      "askq-hint",
    ]);

    const otherOptionLabel = screen.getAllByText("Type something.")[0];
    const otherOption = otherOptionLabel?.closest(".askq-option");
    if (!(otherOption instanceof HTMLElement)) {
      throw new Error("Expected other option");
    }
    fireEvent.click(otherOption);
    const otherInput = screen.getByPlaceholderText("Type something");
    fireEvent.change(otherInput, {
      target: { value: "A custom answer" },
    });
    fireEvent.click(screen.getByRole("tab", { name: "Submit" }));

    expect(readShellChildClassNames(container)).toEqual(initialStructure);
    expect(container.querySelector(".askq-panel-viewport")).not.toBeNull();
    expect(container.querySelector(".askq-status-slot")).not.toBeNull();
  });
});
