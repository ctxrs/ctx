import { Linter } from "eslint";
import { describe, expect, it } from "vitest";
import tseslint from "typescript-eslint";
import noRawAnchorHrefRule from "../../eslint/rules/no-raw-anchor-href.js";

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
              "no-raw-anchor-href": noRawAnchorHrefRule,
            },
          },
        },
        rules: {
          "ctx-web/no-raw-anchor-href": "error",
        },
      },
    ],
    { filename },
  );
};

describe("no-raw-anchor-href", () => {
  it("flags raw external anchors", () => {
    const messages = runRule(`export function Demo() { return <a href="https://example.com">Docs</a>; }`);

    expect(messages).toHaveLength(1);
    expect(messages[0]?.ruleId).toBe("ctx-web/no-raw-anchor-href");
  });

  it("flags raw anchors with dynamic href values", () => {
    const messages = runRule(`
      export function Demo({ href }: { href: string }) {
        return <a href={href}>Docs</a>;
      }
    `);

    expect(messages).toHaveLength(1);
  });

  it("allows internal ctx links and explicit escape hatches", () => {
    const messages = runRule(`
      export function Demo() {
        return (
          <>
            <a href="ctx://open?path=%2Ftmp%2Fdemo">Open</a>
            <a href={"https://example.com"} data-allow-raw-anchor>Docs</a>
          </>
        );
      }
    `);

    expect(messages).toHaveLength(0);
  });

  it("ignores the ExternalLink implementation file", () => {
    const messages = runRule(
      `export function ExternalLink() { return <a href="https://example.com">Docs</a>; }`,
      "src/components/ExternalLink.tsx",
    );

    expect(messages).toHaveLength(0);
  });
});
