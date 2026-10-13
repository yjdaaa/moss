import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

// This frontend currently uses Vite's classic JSX transform. JSX components
// that only import useState/useEffect fail at runtime with "React is not
// defined" and may unmount the entire desktop React application.
const sourceRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../src");
function walk(folder) {
  return readdirSync(folder, { withFileTypes: true }).flatMap((entry) => {
    const target = path.join(folder, entry.name);
    if (entry.isDirectory()) return walk(target);
    return entry.isFile() && entry.name.endsWith(".jsx") ? [target] : [];
  });
}
test("every JSX module imports React for the classic JSX runtime", () => {
  const missing = walk(sourceRoot).filter((filename) => {
    const source = readFileSync(filename, "utf8");
    const hasJsx = /<(?:(?:[A-Za-z][A-Za-z0-9.]*)|>)(?:\s|\/?>|\w)/.test(source);
    const hasReact = /^\s*import\s+React\s*(?:,|from\s*["']react["'])/m.test(source)
      || /^\s*import\s+\*\s+as\s+React\s+from\s*["']react["']/m.test(source);
    return hasJsx && !hasReact;
  });
  assert.deepEqual(missing.map((file) => path.relative(sourceRoot, file)).sort(), [],
    "JSX without a React binding will make its page black at runtime");
});
