export type AskUserQuestionOption = { label: string; description?: string; isOther?: boolean };

export type AskUserQuestionItem = {
  header: string;
  question: string;
  options: AskUserQuestionOption[];
  multiSelect: boolean;
  otherLabel?: string;
};

export const DEFAULT_OTHER_LABEL = "Type something.";

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

function readNonEmptyString(...values: Array<unknown>): string | null {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return null;
}

function normalizeOptions(raw: unknown): AskUserQuestionOption[] {
  if (!Array.isArray(raw)) return [];
  return raw
    .map((value) => {
      if (typeof value === "string") return { label: value };
      if (!value || typeof value !== "object") return null;
      const record = asRecord(value);
      const label = typeof record.label === "string" ? record.label : "";
      if (!label.trim()) return null;
      const description = typeof record.description === "string" ? record.description : undefined;
      return { label, description };
    })
    .filter((option: AskUserQuestionOption | null): option is AskUserQuestionOption => Boolean(option));
}

function extractOtherLabel(
  question: Record<string, unknown>,
  options: AskUserQuestionOption[],
): {
  options: AskUserQuestionOption[];
  otherLabel?: string;
} {
  const allowOther = Boolean(question.allowOther ?? question.allow_other ?? question.allowOtherOption);
  const explicitLabel = readNonEmptyString(
    question.otherOptionLabel,
    question.other_option_label,
    question.otherLabel,
  );
  let otherLabel = explicitLabel ?? undefined;

  const nextOptions: AskUserQuestionOption[] = [];
  for (const option of options) {
    const label = option.label.trim();
    const normalized = label.toLowerCase();
    if (!otherLabel && (normalized === "other" || normalized === "type something" || normalized === "type something.")) {
      otherLabel = label;
      continue;
    }
    nextOptions.push(option);
  }

  if (!otherLabel && allowOther) otherLabel = DEFAULT_OTHER_LABEL;
  return { options: nextOptions, otherLabel };
}

export function normalizeAskUserQuestions(input: unknown): AskUserQuestionItem[] {
  const record = asRecord(input);
  const nested = asRecord(record.input);
  const rawQuestions: unknown[] = Array.isArray(record.questions)
    ? record.questions
    : Array.isArray(nested.questions)
      ? nested.questions
      : Array.isArray(input)
        ? input
        : [];
  const normalized: AskUserQuestionItem[] = [];

  rawQuestions.forEach((questionValue, index) => {
    const questionRecord = asRecord(questionValue);
    const question = typeof questionRecord.question === "string" ? questionRecord.question : "";
    if (!question.trim()) return;
    const header = typeof questionRecord.header === "string" ? questionRecord.header : `Question ${index + 1}`;
    const multiSelect = Boolean(questionRecord.multiSelect ?? questionRecord.multi_select);
    const { options, otherLabel } = extractOtherLabel(questionRecord, normalizeOptions(questionRecord.options));
    normalized.push({
      header,
      question,
      options,
      multiSelect,
      otherLabel,
    });
  });

  return normalized;
}

export function splitAskUserQuestionAnswerParts(answer: string, multiSelect: boolean): string[] {
  if (!answer.trim()) return [];
  if (!multiSelect) return [answer.trim()];
  return answer
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean);
}

export function deriveAskUserQuestionSelectionState(
  questions: readonly AskUserQuestionItem[],
  answers?: Record<string, string>,
): {
  selectedByQuestion: Record<string, Set<string>>;
  otherByQuestion: Record<string, string>;
} {
  const selectedByQuestion: Record<string, Set<string>> = {};
  const otherByQuestion: Record<string, string> = {};
  if (!answers) return { selectedByQuestion, otherByQuestion };

  for (const question of questions) {
    const answer = String(answers[question.question] ?? "").trim();
    if (!answer) continue;
    const optionLabels = new Set(question.options.map((option) => option.label));
    const selected = new Set<string>();
    const otherParts: string[] = [];
    const parts = splitAskUserQuestionAnswerParts(answer, question.multiSelect);
    for (const part of parts) {
      if (optionLabels.has(part)) selected.add(part);
      else otherParts.push(part);
    }
    if (otherParts.length > 0) {
      otherByQuestion[question.question] = question.multiSelect ? otherParts.join(", ") : otherParts[0]!;
      if (question.otherLabel) selected.add(question.otherLabel);
    }
    if (selected.size > 0) selectedByQuestion[question.question] = selected;
  }

  return { selectedByQuestion, otherByQuestion };
}

export function buildAskUserQuestionAnswersFromState(
  questions: readonly AskUserQuestionItem[],
  selectedByQuestion: Record<string, Set<string>>,
  otherByQuestion: Record<string, string>,
): Record<string, string> {
  const answers: Record<string, string> = {};
  for (const question of questions) {
    const other = (otherByQuestion[question.question] ?? "").trim();
    const selected = selectedByQuestion[question.question];
    const selectedValues = selected ? [...selected] : [];
    const trimmedSelected = question.otherLabel
      ? selectedValues.filter((value) => value !== question.otherLabel)
      : selectedValues;

    if (question.multiSelect) {
      const combined = [...trimmedSelected];
      if (other) combined.push(other);
      if (combined.length > 0) answers[question.question] = combined.join(", ");
      continue;
    }

    if (other) {
      answers[question.question] = other;
      continue;
    }
    if (trimmedSelected.length > 0) answers[question.question] = trimmedSelected[0]!;
  }
  return answers;
}
