#!/usr/bin/env node
// crate 分层门禁：依赖只允许向下，两个业务域之间互不依赖（规则见 docs/developer/architecture.md §2）。
// 直接解析各 Cargo.toml，不需要 cargo。新增 crate 必须先登记到 LAYERS，否则检查失败。
// 用法：node scripts/check-crate-layers.mjs
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

// rank 越小越底层；domain 只对第 2 层有意义。
export const LAYERS = {
  // 0 边界与通用库：只放类型、协议和无业务的客户端，谁都可以依赖，自身不依赖仓库内任何 crate
  "laser-contracts": { rank: 0 },
  "moss-xiaozhi-proto": { rank: 0 },
  "moss-voice-local-proto": { rank: 0 },
  "moss-mcp-client": { rank: 0 },
  // 1 共享运行时：运行目录、配置、SQLite 与各类存储、密钥（激光和语音共用）
  "laser-runtime": { rank: 1 },
  // 2 业务域：同域内可互相依赖，跨域禁止
  "laser-core": { rank: 2, domain: "laser" },
  "laser-device": { rank: 2, domain: "laser" },
  "laser-tools": { rank: 2, domain: "laser" },
  "laser-mcp": { rank: 2, domain: "laser" },
  "laser-api": { rank: 2, domain: "laser" },
  "moss-voice": { rank: 2, domain: "voice" },
  "moss-voice-providers": { rank: 2, domain: "voice" },
  "moss-voice-server": { rank: 2, domain: "voice" },
  "moss-usb-provision": { rank: 2, domain: "voice" },
  // 3 编排与跨域扩展：启动组装，以及同时用到激光审批与语音设置的扩展（彼此不互相依赖）
  "laser-host": { rank: 3 },
  "moss-agent": { rank: 3 },
  "moss-home": { rank: 3 },
  // 4 应用壳：只做入口与平台胶水，任何 crate 都不得依赖它们
  "moss-host": { rank: 4 },
  "moss-desktop": { rank: 4 },
  "moss-voice-local": { rank: 4 },
};

// 根 workspace 之外仍要检查的独立工作区（GPL helper）。
const EXTRA_MANIFESTS = ["apps/moss-voice-local/Cargo.toml"];

export function workspaceManifests(root = REPO_ROOT) {
  const text = readFileSync(join(root, "Cargo.toml"), "utf8");
  const block = text.match(/^members\s*=\s*\[([\s\S]*?)\]/m);
  if (!block) throw new Error("根 Cargo.toml 缺少 [workspace] members");
  const members = [...block[1].matchAll(/"([^"]+)"/g)].map((m) => `${m[1]}/Cargo.toml`);
  return [...members, ...EXTRA_MANIFESTS];
}

/** 解析单个 Cargo.toml：包名，以及仓库内 crate 的正式依赖 / 仅测试依赖。 */
export function parseManifest(text, known) {
  let section = "";
  let name = null;
  const normal = new Set();
  const dev = new Set();
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.replace(/#.*$/, "").trim();
    if (!line) continue;
    const header = line.match(/^\[([^\]]+)\]$/);
    if (header) {
      section = header[1].trim();
      // [dependencies.foo] 这种表头形式
      const table = section.match(/(?:^|\.)(dependencies|dev-dependencies|build-dependencies)\.([A-Za-z0-9_-]+)$/);
      if (table && known.has(table[2])) (table[1] === "dev-dependencies" ? dev : normal).add(table[2]);
      continue;
    }
    if (section === "package") {
      const n = line.match(/^name\s*=\s*"([^"]+)"/);
      if (n) name = n[1];
      continue;
    }
    const kind = section.match(/(?:^|\.)(dependencies|dev-dependencies|build-dependencies)$/);
    if (!kind) continue;
    const dep = line.match(/^([A-Za-z0-9_-]+)(?:\.[A-Za-z0-9_-]+)?\s*=/);
    if (dep && known.has(dep[1])) (kind[1] === "dev-dependencies" ? dev : normal).add(dep[1]);
  }
  return { name, normal: [...normal], dev: [...dev] };
}

/** 判定一条依赖边是否合规；返回违规原因，合规返回 null。 */
export function edgeViolation(from, to, devOnly, layers = LAYERS) {
  const a = layers[from];
  const b = layers[to];
  if (!a) return `${from} 未登记分层`;
  if (!b) return `${to} 未登记分层`;
  if (from === to) return null;
  if (b.rank === 4) return "应用壳不能被依赖";
  // 测试依赖只需不依赖应用壳（测试可以借用其他域的实现做集成）
  if (devOnly) return null;
  if (b.rank > a.rank) return `向上依赖（第 ${a.rank} 层 → 第 ${b.rank} 层）`;
  if (b.rank === a.rank && a.rank !== 2) return `同层依赖（第 ${a.rank} 层）`;
  if (a.rank === 2 && b.rank === 2 && a.domain !== b.domain) return `跨业务域（${a.domain} → ${b.domain}）`;
  return null;
}

export function checkLayers(root = REPO_ROOT, layers = LAYERS) {
  const known = new Set(Object.keys(layers));
  const violations = [];
  const seen = [];
  for (const rel of workspaceManifests(root)) {
    const parsed = parseManifest(readFileSync(join(root, rel), "utf8"), known);
    if (!parsed.name) {
      violations.push({ manifest: rel, reason: "无法读取包名" });
      continue;
    }
    seen.push(parsed.name);
    if (!layers[parsed.name]) {
      violations.push({ manifest: rel, from: parsed.name, reason: `${parsed.name} 未登记分层` });
      continue;
    }
    for (const [list, devOnly] of [[parsed.normal, false], [parsed.dev, true]]) {
      for (const to of list) {
        const reason = edgeViolation(parsed.name, to, devOnly, layers);
        if (reason) violations.push({ manifest: rel, from: parsed.name, to, dev: devOnly, reason });
      }
    }
  }
  // 登记了却已不存在的 crate 也要报，避免表格过期
  for (const name of known) {
    if (!seen.includes(name)) violations.push({ from: name, reason: `${name} 已登记但不在工作区中` });
  }
  return violations;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const violations = checkLayers();
  if (violations.length === 0) {
    console.log("crate 分层检查通过");
  } else {
    for (const v of violations) {
      const edge = v.to ? `${v.from} → ${v.to}${v.dev ? "（dev）" : ""}` : v.from ?? v.manifest;
      console.error(`✗ ${edge}：${v.reason}${v.manifest ? `  [${v.manifest}]` : ""}`);
    }
    process.exitCode = 1;
  }
}
