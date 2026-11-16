#!/usr/bin/env node
// 仓库静态规范检查：PowerShell ASCII、Rust 整文件 allow、敏感文件与本机路径、提示词字数预算、提交信息格式。
// 用法：
//   node scripts/check-repo-hygiene.mjs
//   node scripts/check-repo-hygiene.mjs --staged
//   node scripts/check-repo-hygiene.mjs --commit-msg <file>
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import os from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const PROMPT_DOC_PATH = "docs/integration/xiaozhi-prompts/latest.md";
export const CONSOLE_PROMPT_LIMIT = 2000;
export const KB_PROMPT_LIMIT = 200;

const BINARY_EXT_RE =
  /\.(png|jpg|jpeg|gif|webp|ico|svg|woff2?|ttf|otf|bin|wav|mp3|ogg|onnx|pdf|zip|gz|tar|exe|dll|so|dylib)$/i;

const SYNTHETIC_TEST_USERS = new Set([
  "alice",
  "Alice",
  "bob",
  "demo",
  "someone",
  "me",
  "user",
  "old-user",
  "u",
  "Shared",
  "example",
]);

const USER_DIR_RE = /(?:^|[^A-Za-z0-9_.-])(\/Users\/([A-Za-z0-9._-]+)|[A-Za-z]:\\\\?Users\\\\?([A-Za-z0-9._-]+))/g;
const SECRET_RE =
  /\b(sk-[A-Za-z0-9_-]{24,}|ghp_[A-Za-z0-9]{24,}|github_pat_[A-Za-z0-9_]{24,}|AKIA[0-9A-Z]{16})\b|(dpapi-user-v1|keychain-user-v1):[A-Za-z0-9+/=_-]{24,}/;
const COMMIT_HEADER_RE = /^(feat|fix|refactor|docs|test|build|chore)!?: (\S.*)$/;

function normalizeRelPath(relPath) {
  return relPath.replace(/\\/g, "/");
}

function isSyntheticTestFile(relPath) {
  const norm = normalizeRelPath(relPath);
  if (norm.includes("/tests/") || norm.includes(".test.") || /(^|\/)tests?\//.test(norm)) {
    return true;
  }
  return /^(crates|apps)\/.*\.rs$/.test(norm);
}

/** 校验 PowerShell 脚本仅含 ASCII 字符（兼容 Windows PowerShell 5.1）。 */
export function checkPs1Ascii(relPath, text) {
  const violations = [];
  const lines = text.split(/\r?\n/);
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    for (let j = 0; j < line.length; j += 1) {
      const code = line.charCodeAt(j);
      if (code !== 9 && (code < 0x20 || code > 0x7e)) {
        violations.push({
          file: relPath,
          line: i + 1,
          reason: "PowerShell 脚本须保持纯 ASCII（兼容 Windows PowerShell 5.1）",
        });
        break;
      }
    }
  }
  return violations;
}

/** 禁止 Rust 源码整文件 #![allow(clippy::...)]。 */
export function checkRustInnerAllow(relPath, text) {
  const norm = normalizeRelPath(relPath);
  if (norm.startsWith("apps/desktop/src-tauri/gen/")) return [];
  if (!/^(crates|apps)\/.*\.rs$/.test(norm)) return [];
  const violations = [];
  const stripped = text
    .split(/\r?\n/)
    .map((line) => line.split("//")[0])
    .join("\n");
  const innerAllowRe = /#!\[allow\(([\s\S]*?)\)\]/g;
  let m;
  while ((m = innerAllowRe.exec(stripped)) !== null) {
    if (/\bclippy::/.test(m[1])) {
      const line = stripped.slice(0, m.index).split("\n").length;
      violations.push({
        file: norm,
        line,
        reason: "禁止整文件 #![allow(clippy::...)]，确需豁免请用带 reason 的 #[expect(...)]",
      });
    }
  }
  return violations;
}

/** 检查待跟踪/暂存路径是否误纳了 .env、运行目录或私钥文件。 */
export function checkTrackedPathPolicy(relPath) {
  const norm = normalizeRelPath(relPath);
  const base = norm.split("/").pop() ?? norm;
  const violations = [];
  if ((base === ".env" || base.startsWith(".env.")) && base !== ".env.example" && !base.endsWith(".env.example")) {
    violations.push({
      file: norm,
      reason: "禁止提交或跟踪真实 .env 文件（仅允许 .env.example）",
    });
  }
  if (norm === ".runtime" || norm.startsWith(".runtime/") || norm.includes("/.runtime/")) {
    violations.push({
      file: norm,
      reason: "禁止提交或跟踪 .runtime 运行目录文件",
    });
  }
  if (/\.(pem|key|p12|pfx)$/i.test(base)) {
    violations.push({
      file: norm,
      reason: "禁止提交或跟踪私钥/证书文件",
    });
  }
  return violations;
}

function escapeRegExp(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** 检查文本内容中是否泄漏当前机器主目录、真实本机用户路径或高置信度凭据串。 */
export function checkSensitiveContent(relPath, text, { homeDir = os.homedir() } = {}) {
  const norm = normalizeRelPath(relPath);
  if (BINARY_EXT_RE.test(norm)) return [];

  const violations = [];
  const allowSyntheticUsers = isSyntheticTestFile(norm);
  const skipGenericUserDir = norm.startsWith("tests/golden/");
  const normalizedHome = homeDir ? normalizeRelPath(homeDir).replace(/\/+$/, "") : "";
  const homePattern =
    normalizedHome.length >= 6
      ? new RegExp(`(?:^|[^A-Za-z0-9_.-])${escapeRegExp(normalizedHome)}(?:$|[\\\\/\\s"'\`])`)
      : null;
  const winHome = normalizedHome.replace(/\//g, "\\");
  const winHomePattern =
    winHome.length >= 6 && winHome !== normalizedHome
      ? new RegExp(`(?:^|[^A-Za-z0-9_.-])${escapeRegExp(winHome)}(?:$|[\\\\/\\s"'\`])`)
      : null;
  const escapedWinHome = winHome.replace(/\\/g, "\\\\");
  const escapedWinHomePattern =
    escapedWinHome.length >= 6 && escapedWinHome !== winHome && escapedWinHome !== normalizedHome
      ? new RegExp(`(?:^|[^A-Za-z0-9_.-])${escapeRegExp(escapedWinHome)}(?:$|[\\\\/\\s"'\`])`)
      : null;

  const lines = text.split(/\r?\n/);
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    if (
      (homePattern && homePattern.test(line)) ||
      (winHomePattern && winHomePattern.test(line)) ||
      (escapedWinHomePattern && escapedWinHomePattern.test(line))
    ) {
      violations.push({
        file: norm,
        line: i + 1,
        reason: "禁止写入当前本机绝对路径（请改用相对路径或 <path> 占位）",
      });
      continue;
    }

    if (!skipGenericUserDir) {
      USER_DIR_RE.lastIndex = 0;
      let m;
      while ((m = USER_DIR_RE.exec(line)) !== null) {
        const user = m[2] || m[3];
        if (!allowSyntheticUsers || !SYNTHETIC_TEST_USERS.has(user)) {
          violations.push({
            file: norm,
            line: i + 1,
            reason: `禁止写入本机用户目录路径（${m[1]}，请改用 <path> 占位）`,
          });
          break;
        }
      }
    }

    if (SECRET_RE.test(line)) {
      violations.push({
        file: norm,
        line: i + 1,
        reason: "疑似包含真实密钥、令牌或已封存凭据密文",
      });
    }
  }
  return violations;
}

/** 校验小智控制台提示词与知识库正文 Unicode 码点数上限。 */
export function checkPromptBudget(text, relPath = PROMPT_DOC_PATH) {
  const violations = [];
  const specs = [
    { heading: "Console Prompt", limit: CONSOLE_PROMPT_LIMIT },
    { heading: "Knowledge Base Text", limit: KB_PROMPT_LIMIT },
  ];
  for (const { heading, limit } of specs) {
    const re = new RegExp(`## ${heading}\\s+\`\`\`text\\r?\\n([\\s\\S]*?)\\r?\\n\`\`\``);
    const match = text.match(re);
    if (!match) {
      violations.push({
        file: relPath,
        reason: `缺少 ## ${heading} 的 text 代码块`,
      });
      continue;
    }
    const count = [...match[1]].length;
    if (count > limit) {
      violations.push({
        file: relPath,
        reason: `${heading} 正文 ${count} 字符，超过上限 ${limit}`,
      });
    }
  }
  return violations;
}

/** 校验提交标题格式：<类型>: <中文结论>。合规返回 null，否则返回原因字符串。 */
export function checkCommitMessage(rawText) {
  const lines = rawText
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && !line.startsWith("#"));
  if (lines.length === 0) {
    return "提交信息不能为空；格式为 <类型>: <中文结论>（示例：fix: 修复串口重连状态同步）";
  }
  const header = lines[0];
  if (/^(Merge |Revert "|fixup! |squash! )/.test(header)) {
    return null;
  }
  const match = header.match(COMMIT_HEADER_RE);
  if (!match) {
    return "提交标题须符合 <类型>: <中文结论>（类型取 feat/fix/refactor/docs/test/build/chore，可加 !，冒号后保留空格）";
  }
  if (!/[\u4e00-\u9fff]/.test(match[2])) {
    return "提交标题冒号后须使用中文结论（至少包含中文描述）";
  }
  return null;
}

function listRepoFiles(root, stagedOnly) {
  const args = stagedOnly
    ? ["diff", "--cached", "--name-only", "-z", "--diff-filter=ACMR"]
    : ["ls-files", "-z", "--cached", "--others", "--exclude-standard"];
  const raw = execFileSync("git", args, { cwd: root, encoding: "utf8" });
  return raw.split("\0").filter(Boolean);
}

/** 执行完整仓库规范检查并返回违规列表。 */
export function checkRepoHygiene({ root = REPO_ROOT, files = null, stagedOnly = false, homeDir = os.homedir() } = {}) {
  const targetFiles = files ?? listRepoFiles(root, stagedOnly);
  const violations = [];
  let checkedPrompt = false;

  for (const rawPath of targetFiles) {
    const relPath = normalizeRelPath(rawPath);
    violations.push(...checkTrackedPathPolicy(relPath));
    if (BINARY_EXT_RE.test(relPath)) continue;

    let text;
    if (stagedOnly) {
      try {
        const objType = execFileSync("git", ["cat-file", "-t", `:${relPath}`], { cwd: root, encoding: "utf8" }).trim();
        if (objType !== "blob") continue;
        text = execFileSync("git", ["cat-file", "blob", `:${relPath}`], {
          cwd: root,
          encoding: "utf8",
          maxBuffer: 64 * 1024 * 1024,
        });
      } catch {
        continue;
      }
    } else {
      const absPath = join(root, relPath);
      if (!existsSync(absPath)) continue;
      try {
        if (!statSync(absPath).isFile()) continue;
      } catch {
        continue;
      }
      text = readFileSync(absPath, "utf8");
    }

    if (relPath.endsWith(".ps1")) {
      violations.push(...checkPs1Ascii(relPath, text));
    }
    violations.push(...checkRustInnerAllow(relPath, text));
    violations.push(...checkSensitiveContent(relPath, text, { homeDir }));
    if (relPath === PROMPT_DOC_PATH) {
      violations.push(...checkPromptBudget(text, relPath));
      checkedPrompt = true;
    }
  }

  if (!stagedOnly && !files && !checkedPrompt) {
    const promptAbs = join(root, PROMPT_DOC_PATH);
    if (existsSync(promptAbs)) {
      violations.push(...checkPromptBudget(readFileSync(promptAbs, "utf8"), PROMPT_DOC_PATH));
    }
  }
  return violations;
}

function runCli() {
  const args = process.argv.slice(2);
  if (args[0] === "--commit-msg") {
    const msgFile = args[1];
    if (!msgFile) {
      console.error("用法：node scripts/check-repo-hygiene.mjs --commit-msg <file>");
      process.exitCode = 2;
      return;
    }
    const err = checkCommitMessage(readFileSync(msgFile, "utf8"));
    if (err) {
      console.error(`✗ ${err}`);
      process.exitCode = 1;
    }
    return;
  }

  const stagedOnly = args.includes("--staged");
  const violations = checkRepoHygiene({ stagedOnly });
  if (violations.length === 0) {
    console.log(stagedOnly ? "暂存区仓库规范检查通过" : "仓库规范检查通过");
    return;
  }
  for (const v of violations) {
    const loc = v.line ? `${v.file}:${v.line}` : v.file;
    console.error(`✗ ${loc}：${v.reason}`);
  }
  process.exitCode = 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  runCli();
}
