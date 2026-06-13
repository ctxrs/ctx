import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";
import tseslint from "typescript-eslint";
import noRawAnchorHrefRule from "./eslint/rules/no-raw-anchor-href.js";
import noRawTextInputRule from "./eslint/rules/no-raw-text-input.js";

const webFiles = ["src/**/*.{ts,tsx}"];

export default tseslint.config(
  {
    ignores: ["coverage/**", "dist/**", "node_modules/**", "public/**"],
    linterOptions: {
      reportUnusedDisableDirectives: "off",
    },
  },
  ...tseslint.configs.recommended,
  {
    files: webFiles,
    languageOptions: {
      globals: {
        ...globals.browser,
      },
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
          "no-raw-text-input": noRawTextInputRule,
        },
      },
      "react-hooks": reactHooks,
    },
    rules: {
      "@typescript-eslint/no-unused-vars": "off",
      "@typescript-eslint/prefer-as-const": "off",
      "no-unused-vars": "off",
      "prefer-const": "off",
      "constructor-super": "error",
      "ctx-web/no-raw-anchor-href": "error",
      "ctx-web/no-raw-text-input": "error",
      "getter-return": "error",
      "no-async-promise-executor": "error",
      "no-constant-binary-expression": "error",
      "no-debugger": "error",
      "no-dupe-args": "error",
      "no-dupe-keys": "error",
      "no-unreachable": "error",
      "no-unsafe-finally": "error",
      "react-hooks/rules-of-hooks": "error",
      "use-isnan": "error",
      "valid-typeof": "error",
    },
  },
  {
    files: ["src/**/*.test.{ts,tsx}"],
    rules: {
      "ctx-web/no-raw-text-input": "off",
    },
  },
);
