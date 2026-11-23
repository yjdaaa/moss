// CHANGELOG 归档脚本测试：在临时目录里移动已发布版本，根文件只留 [Unreleased]。
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { archiveChangelog, splitChangelog } from "../archive-changelog.mjs";

const SAMPLE = [
  "# Changelog",
  "",
  "说明行。",
  "",
  "## [Unreleased]",
  "",
  "### 新改动",
  "",
  "- **现象**：x",
  "",
  "## [0.3.12] - 2026-11-19",
  "",
  "### 三点十二",
  "",
  "## [0.3.11] - 2026-11-07",
  "",
  "### 三点十一",
  "",
  "## [0.2.0] - 2026-11-03",
  "",
  "### 二点零",
  "",
].join("\n");

test("splitChangelog 区分未发布段与已发布版本并按次版本分组", () => {
  const { head, unreleased, released } = splitChangelog(SAMPLE);
  assert.equal(head, "# Changelog\n\n说明行。");
  assert.match(unreleased, /^## \[Unreleased\]\n\n### 新改动/);
  assert.deepEqual(released.map((s) => s.series), ["0.3", "0.3", "0.2"]);
});

test("archiveChangelog 移走已发布版本、保留未发布段，重复运行不重复写入", () => {
  const root = mkdtempSync(join(tmpdir(), "moss-changelog-"));
  try {
    writeFileSync(join(root, "CHANGELOG.md"), SAMPLE);
    mkdirSync(join(root, "docs", "changelog"), { recursive: true });
    // 归档文件里已有更早的 0.3.10，新归档的版本插在它前面
    writeFileSync(join(root, "docs", "changelog", "0.3.md"), "# 旧标题\n\n## [0.3.10] - 2026-11-07\n\n### 三点十\n");

    const moved = archiveChangelog(root);
    assert.equal(moved.length, 3);
    const main = readFileSync(join(root, "CHANGELOG.md"), "utf8");
    assert.match(main, /## \[Unreleased\]/);
    assert.doesNotMatch(main, /## \[0\./);

    const series03 = readFileSync(join(root, "docs", "changelog", "0.3.md"), "utf8");
    const order = ["## [0.3.12]", "## [0.3.11]", "## [0.3.10]"].map((h) => series03.indexOf(h));
    assert.ok(order.every((i, k) => i > 0 && (k === 0 || i > order[k - 1])), series03);
    assert.match(readFileSync(join(root, "docs", "changelog", "0.2.md"), "utf8"), /### 二点零/);

    // 再写回同样的已发布段重跑：归档文件不出现重复版本
    writeFileSync(join(root, "CHANGELOG.md"), SAMPLE);
    archiveChangelog(root);
    const again = readFileSync(join(root, "docs", "changelog", "0.3.md"), "utf8");
    assert.equal(again.split("## [0.3.12]").length, 2);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("--dry-run 不改文件", () => {
  const root = mkdtempSync(join(tmpdir(), "moss-changelog-"));
  try {
    writeFileSync(join(root, "CHANGELOG.md"), SAMPLE);
    assert.equal(archiveChangelog(root, { dryRun: true }).length, 3);
    assert.equal(readFileSync(join(root, "CHANGELOG.md"), "utf8"), SAMPLE);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
