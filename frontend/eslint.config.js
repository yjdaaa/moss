// 前端静态检查（2026-11-07）：只启用能抓真错误的规则，不管排版。
// `npm run lint` 在 CI 与本机验证中执行，error 级别会让 CI 失败。
import js from "@eslint/js";
import globals from "globals";
import react from "eslint-plugin-react";
import reactHooks from "eslint-plugin-react-hooks";

export default [
  { ignores: ["dist/**", "node_modules/**", "public/**", "*.html"] },
  js.configs.recommended,
  {
    files: ["src/**/*.{js,jsx}", "tests/**/*.mjs", "*.{js,mjs}"],
    languageOptions: {
      ecmaVersion: "latest",
      sourceType: "module",
      parserOptions: { ecmaFeatures: { jsx: true } },
      // __MOSS_FRONTEND_VERSION__ 由 vite.config.mjs 的 define 在构建时注入
      globals: { ...globals.browser, ...globals.node, __MOSS_FRONTEND_VERSION__: "readonly" },
    },
    plugins: { react, "react-hooks": reactHooks },
    settings: { react: { version: "18.3" } },
    rules: {
      // JSX 里用到的组件与变量算“已使用”
      "react/jsx-uses-vars": "error",
      "react/jsx-uses-react": "error",
      // Hooks 只能在组件顶层调用：违反就是真 bug
      "react-hooks/rules-of-hooks": "error",
      // 依赖漏写只提示，逐步清理
      "react-hooks/exhaustive-deps": "warn",
      "no-unused-vars": ["error", { args: "none", caughtErrors: "none", varsIgnorePattern: "^_", ignoreRestSiblings: true }],
      "no-empty": ["error", { allowEmptyCatch: true }],
      // 语音文本清洗有意匹配 \x00-\x1f 控制字符
      "no-control-regex": "off",
    },
  },
];
