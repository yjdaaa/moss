// 按页面源码断言的辅助函数（不是测试文件）。
// 2026-11-16 起 pages/HomePage.jsx、DrawPage.jsx、MaterialLabPage.jsx 的大块界面原样搬到
// components/workbench/、components/draw/、components/material-lab/：读页面时按 import 顺序把这些分片
// （含分片之间的相互引用）接在页面源码后面，断言字符串保持不变。
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SRC_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../src");
const PART_DIRS = ["components/workbench", "components/draw", "components/material-lab"].map(
  (dir) => path.join(SRC_ROOT, dir) + path.sep,
);

/** 读 `src/` 下的页面源码，并按 import 顺序拼上它从页面分片目录引入的文件。 */
export function readPageSource(relPath) {
  const seen = new Set();
  const read = (file) => {
    if (seen.has(file)) return [];
    seen.add(file);
    const source = readFileSync(file, "utf8");
    const parts = [...source.matchAll(/^import\s[^;]*?from\s*["'](\.{1,2}\/[^"']+)["'];?\s*$/gm)]
      .map((match) => path.resolve(path.dirname(file), match[1]))
      .filter((target) => PART_DIRS.some((dir) => target.startsWith(dir)));
    return [source, ...parts.flatMap(read)];
  };
  return read(path.join(SRC_ROOT, relPath)).join("\n");
}
