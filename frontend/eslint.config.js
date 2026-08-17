import js from "@eslint/js";
import globals from "globals";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";

export default tseslint.config(
  { ignores: ["dist", "node_modules", ".next"] },
  {
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2022,
      globals: { ...globals.browser, ...globals.node },
    },
    plugins: { "react-hooks": reactHooks },
    rules: {
      ...reactHooks.configs.recommended.rules,

      // Rules introduced in eslint-plugin-react-hooks v7. They currently fire
      // only on chart and WebSocket code carried over unchanged from the
      // Next.js app, which was never linted at all (the old project had no
      // ESLint setup). Reworking those effects is a real behavioural change to
      // the live-updating charts and does not belong in an authentication
      // migration, so they are surfaced as warnings for a follow-up rather
      // than either failing the build or being silently switched off.
      "react-hooks/set-state-in-effect": "warn",
      "react-hooks/refs": "warn",
      "react-hooks/purity": "warn",

      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      // Auth code must never reach for `any`: the shapes here are the ones
      // worth being precise about.
      "@typescript-eslint/no-explicit-any": "error",
      "no-restricted-globals": [
        "error",
        {
          name: "localStorage",
          message:
            "Tokens live in HttpOnly cookies. Nothing auth-related may be stored in the browser.",
        },
        {
          name: "sessionStorage",
          message:
            "Tokens live in HttpOnly cookies. Nothing auth-related may be stored in the browser.",
        },
      ],
    },
  },
);
