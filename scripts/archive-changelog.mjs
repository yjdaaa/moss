#!/usr/bin/env node
// 把 CHANGELOG.md 里已发布版本的段落移到 docs/changelog/<主版本.次版本>.md（新版本在前），根文件只留 [Unreleased]。
// 用法：
//   node scripts/archive-changelog.mjs            # 执行归档
//   node scripts/archive-changelog.mjs --dry-run  # 只打印将移动的版本
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const ARCHIVE_DIR = "docs/changelog";
const VERSION_HEADING_RE = /^## \[(\d+)\.(\d+)\.\d+[^\]]*\]/;

/** 按 `## ` 标题切分：返回 { head, unreleased, released: [{ heading, series, body }] }（纯函数）。 */
export function splitChangelog(text) {
  const lines = text.split("\n");
  const firstSection = lines.findIndex((l) => l.startsWith("## "));
  const head = (firstSection < 0 ? lines : lines.slice(0, firstSection)).join("\n");
  const sections = [];
  if (firstSection >= 0) {
    for (let i = firstSection; i < lines.length; i += 1) {
      if (lines[i].startsWith("## ")) sections.push({ heading: lines[i], lines: [] });
      sections.at(-1).lines.push(lines[i]);
    }
  }
  let unreleased = "";
  const released = [];
  for (const s of sections) {
    const body = s.lines.join("\n").replace(/\n+$/, "");
    const m = s.heading.match(VERSION_HEADING_RE);
    if (m) {
      released.push({ heading: s.heading, series: `${m[1]}.${m[2]}`, body });
    } else {
      unreleased += `${body}\n\n`;
    }
  }
  return { head: head.replace(/\n+$/, ""), unreleased: unreleased.replace(/\n+$/, ""), released };
}

/** 把新归档的段落插到归档文件标题之后、已有版本之前（纯函数）。 */
export function mergeArchive(existing, series, sections) {
  const header = `# MOSS ${series}.x 更新记录（归档）\n\n已发布版本的历史记录，新版本在前。未发布的改动见根目录 [\`CHANGELOG.md\`](../../CHANGELOG.md)。`;
  const old = existing ? existing.slice(existing.indexOf("\n## ") >= 0 ? existing.indexOf("\n## ") + 1 : existing.length) : "";
  const known = new Set(old.split("\n").filter((l) => VERSION_HEADING_RE.test(l)));
  const fresh = sections.filter((s) => !known.has(s.heading)).map((s) => s.body);
  return `${[header, ...fresh, old.replace(/\n+$/, "")].filter(Boolean).join("\n\n")}\n`;
}

/** 执行归档，返回移动的版本标题列表。 */
export function archiveChangelog(root = REPO_ROOT, { dryRun = false } = {}) {
  const changelogPath = join(root, "CHANGELOG.md");
  const { head, unreleased, released } = splitChangelog(readFileSync(changelogPath, "utf8"));
  if (released.length === 0) return [];
  if (!dryRun) {
    const bySeries = new Map();
    for (const s of released) bySeries.set(s.series, [...(bySeries.get(s.series) ?? []), s]);
    mkdirSync(join(root, ARCHIVE_DIR), { recursive: true });
    for (const [series, sections] of bySeries) {
      const target = join(root, ARCHIVE_DIR, `${series}.md`);
      const existing = existsSync(target) ? readFileSync(target, "utf8") : "";
      writeFileSync(target, mergeArchive(existing, series, sections));
    }
    writeFileSync(changelogPath, `${[head, unreleased].filter(Boolean).join("\n\n")}\n`);
  }
  return released.map((s) => s.heading);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const dryRun = process.argv.includes("--dry-run");
  const moved = archiveChangelog(REPO_ROOT, { dryRun });
  if (moved.length === 0) {
    console.log("CHANGELOG.md 里没有已发布版本，无需归档");
  } else {
    console.log(`${dryRun ? "将归档" : "已归档"} ${moved.length} 个版本到 ${ARCHIVE_DIR}/：${moved.map((h) => h.slice(3)).join("、")}`);
  }
}
