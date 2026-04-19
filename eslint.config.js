import js from '@eslint/js'
import globals from 'globals'
import reactHooks from 'eslint-plugin-react-hooks'
import reactRefresh from 'eslint-plugin-react-refresh'
import tseslint from 'typescript-eslint'
import { defineConfig, globalIgnores } from 'eslint/config'

export default defineConfig([
  globalIgnores(['dist', 'src-tauri']),
  {
    files: ['**/*.{ts,tsx}'],
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
  },
])
