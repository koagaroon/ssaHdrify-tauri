import js from "@eslint/js";
import globals from "globals";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import tseslint from "typescript-eslint";
import { defineConfig, globalIgnores } from "eslint/config";

export default defineConfig([
  globalIgnores(["dist", "src-tauri"]),
  {
    files: ["**/*.{ts,tsx}"],
    extends: [
      js.configs.recommended,
      tseslint.configs.recommended,
      reactHooks.configs.flat.recommended,
      reactRefresh.configs.vite,
    ],
    languageOptions: {
      // Aligned with tsconfig.app.json `target: es2023` — the TS parser
      // already accepts 2023 syntax, so keeping ESLint at 2020 produced a
      // mismatched signal about what the codebase is actually compiled as.
      ecmaVersion: 2023,
      globals: globals.browser,
    },
    rules: {
      // ESLint 10 + react-hooks 7 introduced this rule as an error. Five
      // sites in this codebase intentionally call setState inside useEffect
      // to clear transient UI status (e.g. last-action result, preview list,
      // error banner) when a dependency context changes — typically when
      // the user picks a different subtitle file. The textbook React-19
      // alternative is "remount via `key` prop on a wrapper component", but
      // for a status banner that's a heavier refactor than the pattern
      // warrants. The extra cascading render is one frame of work on a
      // tiny piece of state, not a perf concern. Keep the pattern; relax
      // the rule.
      //
      // Audit recipe — to find every set-state-in-effect site (and confirm
      // none are accidental new growth), grep:
      //
      //   rg -nU 'useEffect\([^)]*\{[^}]*\bset[A-Z]\w*\(' src --multiline
      //
      // The five legitimate sites all clear transient banners or preview
      // lists. Any new match should be reviewed for a `key`-based remount
      // alternative before being accepted.
      "react-hooks/set-state-in-effect": "off",
    },
  },
]);
