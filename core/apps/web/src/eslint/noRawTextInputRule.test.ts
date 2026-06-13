import { Linter } from "eslint";
import { describe, expect, it } from "vitest";
import tseslint from "typescript-eslint";
import noRawTextInputRule from "../../eslint/rules/no-raw-text-input.js";

const runRule = (code: string, filename = "src/Foo.tsx") => {
  const linter = new Linter({ configType: "flat" });

  return linter.verify(
    code,
    [
      {
        files: ["**/*.{ts,tsx}"],
        languageOptions: {
          parser: tseslint.parser,
          parserOptions: {
            ecmaFeatures: {
              jsx: true,
            },
            ecmaVersion: "latest",
            sourceType: "module",
          },
        },
        plugins: {
          "ctx-web": {
            rules: {
              "no-raw-text-input": noRawTextInputRule,
            },
          },
        },
        rules: {
          "ctx-web/no-raw-text-input": "error",
        },
      },
    ],
    { filename },
  );
};

describe("no-raw-text-input", () => {
  it("flags raw text inputs and textareas", () => {
    const messages = runRule(`
      export function Demo() {
        return (
          <>
            <input />
            <input type="password" />
            <textarea />
          </>
        );
      }
    `);

    expect(messages).toHaveLength(3);
    expect(messages.every((message) => message.ruleId === "ctx-web/no-raw-text-input")).toBe(true);
  });

  it("allows non-text raw inputs", () => {
    const messages = runRule(`
      export function Demo() {
        return (
          <>
            <input type="checkbox" />
            <input type="radio" />
            <input type="file" />
          </>
        );
      }
    `);

    expect(messages).toHaveLength(0);
  });

  it("flags dynamic raw input types by default", () => {
    const messages = runRule(`
      export function Demo({ type }: { type: string }) {
        return <input type={type} />;
      }
    `);

    expect(messages).toHaveLength(1);
  });

  it("allows static ternaries that only produce non-text raw input types", () => {
    const messages = runRule(`
      export function Demo({ multi }: { multi: boolean }) {
        return <input type={multi ? "checkbox" : "radio"} />;
      }
    `);

    expect(messages).toHaveLength(0);
  });

  it("allows explicit escape hatches and the shared primitive implementation file", () => {
    const escapedMessages = runRule(`
      export function Demo() {
        return (
          <>
            <input data-allow-raw-text-input />
            <textarea data-allow-raw-text-input={true} />
          </>
        );
      }
    `);
    const implementationMessages = runRule(
      `export function Demo() { return <input />; }`,
      String.raw`C:\repo\src\components\ui\text-input.tsx`,
    );

    expect(escapedMessages).toHaveLength(0);
    expect(implementationMessages).toHaveLength(0);
  });

  it("does not treat false-valued escape hatches as opt-ins", () => {
    const messages = runRule(`
      export function Demo() {
        return (
          <>
            <input data-allow-raw-text-input={false} />
            <textarea data-allow-raw-text-input={false} />
          </>
        );
      }
    `);

    expect(messages).toHaveLength(2);
  });
});
