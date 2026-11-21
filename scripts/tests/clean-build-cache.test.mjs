// clean-build-cache.sh 孤儿与失效 .rcgu.o 清理回归测试（仅 Unix）。
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, utimesSync, writeFileSync } from "node:fs";
import os from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const SOURCE_SCRIPT = join(REPO_ROOT, "scripts", "clean-build-cache.sh");
const IS_WIN = process.platform === "win32";

function setupFakeRepo() {
  const root = mkdtempSync(join(os.tmpdir(), "moss-clean-cache-"));
  mkdirSync(join(root, "scripts"), { recursive: true });
  mkdirSync(join(root, "crates", "demo-crate"), { recursive: true });
  mkdirSync(join(root, "target", "debug", "deps"), { recursive: true });
  copyFileSync(SOURCE_SCRIPT, join(root, "scripts", "clean-build-cache.sh"));
  writeFileSync(join(root, "crates", "demo-crate", "Cargo.toml"), '[package]\nname = "demo-crate"\n', "utf8");
  return root;
}

function runCleanup(root, args = [], env = process.env) {
  return spawnSync("bash", [join(root, "scripts", "clean-build-cache.sh"), ...args], {
    cwd: root,
    encoding: "utf8",
    env,
  });
}

test(
  "clean-build-cache.sh 同趟识别并清理孤儿与旧哈希 .rcgu.o，同时保留存活产物",
  { skip: IS_WIN },
  () => {
    const root = setupFakeRepo();
    try {
      const deps = join(root, "target", "debug", "deps");
      const nowSec = Math.floor(Date.now() / 1000);
      const oldSec = nowSec - 3600;

      // 1. 本仓库 crate：保留最新哈希 aaaa...，步骤 5 淘汰旧哈希 bbbb...，步骤 6 应同趟清理 bbbb... 的 .rcgu.o
      const liveRlib = join(deps, "libdemo_crate-aaaaaaaaaaaaaaaa.rlib");
      const liveRcgu = join(deps, "demo_crate-aaaaaaaaaaaaaaaa.cgu1.rcgu.o");
      const staleRlib = join(deps, "libdemo_crate-bbbbbbbbbbbbbbbb.rlib");
      const staleRcgu = join(deps, "demo_crate-bbbbbbbbbbbbbbbb.cgu2.rcgu.o");
      writeFileSync(liveRlib, "new");
      writeFileSync(liveRcgu, "new-o");
      writeFileSync(staleRlib, "old");
      writeFileSync(staleRcgu, "old-o");
      utimesSync(staleRlib, oldSec, oldSec);
      utimesSync(staleRcgu, oldSec, oldSec);

      // 2. 彻底孤儿（无对应 .rlib/.rmeta/可执行文件）
      const orphanRcgu = join(deps, "third_party-cccccccccccccccc.cgu1.rcgu.o");
      writeFileSync(orphanRcgu, "orphan");

      // 3. 第三方存活 .rmeta 与带连字符测试可执行文件对应的 .rcgu.o 均应保留
      const liveThirdRmeta = join(deps, "libthird_party-dddddddddddddddd.rmeta");
      const liveThirdRcgu = join(deps, "third_party-dddddddddddddddd.cgu1.rcgu.o");
      const liveBin = join(deps, "my-tool-eeeeeeeeeeeeeeee");
      const liveBinRcgu = join(deps, "my-tool-eeeeeeeeeeeeeeee.cgu1.rcgu.o");
      writeFileSync(liveThirdRmeta, "rmeta");
      writeFileSync(liveThirdRcgu, "third-o");
      writeFileSync(liveBin, "bin");
      writeFileSync(liveBinRcgu, "bin-o");

      // --dry-run 应与真实清理口径一致，且不删任何文件
      const dry = runCleanup(root, ["--dry-run"]);
      assert.equal(dry.status, 0, dry.stderr);
      assert.match(dry.stdout, /\(2 orphan \.rcgu\.o files\)/);
      assert.ok(existsSync(staleRlib));
      assert.ok(existsSync(staleRcgu));
      assert.ok(existsSync(orphanRcgu));

      // 真实清理
      const real = runCleanup(root);
      assert.equal(real.status, 0, real.stderr);
      assert.ok(!existsSync(staleRlib), "旧哈希 rlib 应被清理");
      assert.ok(!existsSync(staleRcgu), "旧哈希对应的 .rcgu.o 应同趟清理");
      assert.ok(!existsSync(orphanRcgu), "孤儿 .rcgu.o 应被清理");
      assert.ok(existsSync(liveRlib), "最新 rlib 应保留");
      assert.ok(existsSync(liveRcgu), "最新 .rcgu.o 应保留");
      assert.ok(existsSync(liveThirdRcgu), "存活第三方 .rcgu.o 应保留");
      assert.ok(existsSync(liveBinRcgu), "存活带连字符可执行文件 .rcgu.o 应保留");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "clean-build-cache.sh 按静态库归档成员保留存活无哈希 .rcgu.o，清理失效与超期孤儿",
  { skip: IS_WIN },
  () => {
    const arCheck = spawnSync("ar", ["--version"], { encoding: "utf8" });
    // macOS BSD ar does not accept --version (exits non-zero) while still installed; check PATH via which
    const hasAr = arCheck.error === undefined && spawnSync("which", ["ar"]).status === 0;
    if (!hasAr) return;

    const root = setupFakeRepo();
    try {
      const deps = join(root, "target", "debug", "deps");
      const liveObjName = "moss_desktop_lib.live01.rcgu.o";
      const deadObjName = "moss_desktop_lib.dead02.rcgu.o";
      const liveObj = join(deps, liveObjName);
      const deadObj = join(deps, deadObjName);
      writeFileSync(liveObj, "live-member");
      writeFileSync(deadObj, "dead-member");

      // 用真实 ar rcS 打包进 libmoss_desktop_lib.a（大写 S 跳过 ranlib 符号表，兼容 macOS 与 Linux）
      const arRes = spawnSync("ar", ["rcS", join(deps, "libmoss_desktop_lib.a"), liveObjName], {
        cwd: deps,
        encoding: "utf8",
      });
      assert.equal(arRes.status, 0, arRes.stderr);

      // 无对应归档的无哈希 .rcgu.o：未满 3 天保守保留，超过 3 天清理
      const freshNoOwner = join(deps, "standalone.fresh01.rcgu.o");
      const staleNoOwner = join(deps, "standalone.stale02.rcgu.o");
      writeFileSync(freshNoOwner, "fresh");
      writeFileSync(staleNoOwner, "stale");
      const fiveDaysAgo = Math.floor(Date.now() / 1000) - 5 * 86400;
      utimesSync(staleNoOwner, fiveDaysAgo, fiveDaysAgo);

      const res = runCleanup(root);
      assert.equal(res.status, 0, res.stderr);
      assert.ok(existsSync(liveObj), "归档内引用的无哈希 .rcgu.o 应保留");
      assert.ok(!existsSync(deadObj), "不在归档内的旧无哈希 .rcgu.o 应清理");
      assert.ok(existsSync(freshNoOwner), "无归档但 3 天内的 .rcgu.o 应保守保留");
      assert.ok(!existsSync(staleNoOwner), "无归档且超过 3 天的 .rcgu.o 应清理");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "clean-build-cache.sh --dry-run 不重复统计超期增量会话，且不沿软链删外部文件、不误删独立工作区 moss-voice-local/target",
  { skip: IS_WIN },
  () => {
    const root = setupFakeRepo();
    const externalDir = mkdtempSync(join(os.tmpdir(), "moss-clean-ext-"));
    try {
      const fiveDaysAgo = Math.floor(Date.now() / 1000) - 5 * 86400;
      const inc = join(root, "target", "debug", "incremental");
      const expiredCrate = join(inc, "demo-abc123");
      const sessA = join(expiredCrate, "s-old");
      const sessB = join(expiredCrate, "s-new");
      mkdirSync(sessA, { recursive: true });
      mkdirSync(sessB, { recursive: true });
      writeFileSync(join(sessA, "a.bin"), Buffer.alloc(512 * 1024, 1));
      writeFileSync(join(sessB, "b.bin"), Buffer.alloc(512 * 1024, 2));
      utimesSync(sessA, fiveDaysAgo - 60, fiveDaysAgo - 60);
      utimesSync(sessB, fiveDaysAgo, fiveDaysAgo);
      utimesSync(expiredCrate, fiveDaysAgo, fiveDaysAgo);

      // 目录软链指向仓库外目录（含中间段软链 crates/ext-link/target），绝对不能穿透删除外部文件
      const extSecret = join(externalDir, "keep.txt");
      writeFileSync(extSecret, "outside-repo", "utf8");
      const extTargetFile = join(externalDir, "target", "debug", "outside.bin");
      mkdirSync(dirname(extTargetFile), { recursive: true });
      writeFileSync(extTargetFile, "outside-target", "utf8");
      symlinkSync(externalDir, join(root, "crates", "ext-link"));
      const liveCrate = join(inc, "live-def456");
      const liveSess = join(liveCrate, "s-current");
      mkdirSync(liveSess, { recursive: true });
      symlinkSync(externalDir, join(liveCrate, "s-link-ext"));

      // 独立 GPL 工作区 apps/moss-voice-local/target 必须保留（含别名软链也不穿透删除）；误生子 crate target 必须删除
      const helperTargetFile = join(root, "apps", "moss-voice-local", "target", "debug", "sherpa.cache");
      const strayCrateTarget = join(root, "crates", "demo-crate", "target");
      const strayAppTarget = join(root, "apps", "other-app", "target");
      mkdirSync(dirname(helperTargetFile), { recursive: true });
      writeFileSync(helperTargetFile, "keep-helper-cache");
      symlinkSync(join(root, "apps", "moss-voice-local"), join(root, "apps", "alias-helper"));
      mkdirSync(join(strayCrateTarget, "debug"), { recursive: true });
      mkdirSync(join(strayAppTarget, "debug"), { recursive: true });

      const dry = runCleanup(root, ["--dry-run"]);
      assert.equal(dry.status, 0, dry.stderr);
      assert.match(dry.stdout, /\[dry-run\] delete 1\.0 MB: .*demo-abc123$/m);
      assert.doesNotMatch(dry.stdout, /demo-abc123\/s-old/, "--dry-run 不得重复进入已标删的超期目录计费");
      assert.match(dry.stdout, /Would reclaim 1\.0 MB from /);
      assert.doesNotMatch(dry.stdout, /apps\/moss-voice-local\/target/);
      assert.doesNotMatch(dry.stdout, /ext-link\/target/);
      assert.doesNotMatch(dry.stdout, /alias-helper\/target/);

      const real = runCleanup(root);
      assert.equal(real.status, 0, real.stderr);
      assert.ok(!existsSync(expiredCrate), "超期增量目录应被清理");
      assert.ok(existsSync(extSecret), "外部软链目标目录中的文件绝不能被删除");
      assert.ok(existsSync(extTargetFile), "中间段软链指向的外部 target/ 绝不能被删除");
      assert.ok(existsSync(helperTargetFile), "独立工作区 apps/moss-voice-local/target 应保留");
      assert.ok(!existsSync(strayCrateTarget), "子 crate 误生 target/ 应被清理");
      assert.ok(!existsSync(strayAppTarget), "其他 app 误生 target/ 应被清理");
    } finally {
      rmSync(root, { recursive: true, force: true });
      rmSync(externalDir, { recursive: true, force: true });
    }
  },
);

test(
  "clean-build-cache.sh 在 macOS 扫描 .dylib OSO 引用、容忍非 UTF-8 符号且缺少 nm 时 fail-closed",
  { skip: process.platform !== "darwin" },
  () => {
    const root = setupFakeRepo();
    const fakeBin = mkdtempSync(join(os.tmpdir(), "moss-fake-bin-"));
    try {
      const debugDir = join(root, "target", "debug");
      const deps = join(debugDir, "deps");
      const dylib = join(debugDir, "libmoss_desktop_lib.dylib");
      // Mach-O 64-bit little-endian magic (0xfeedfacf -> cf fa ed fe)
      writeFileSync(dylib, Buffer.from([0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0]));

      const keptByDylib = join(deps, "kept-1111111111111111.cgu1.rcgu.o");
      const unrefOrphan = join(deps, "dead-2222222222222222.cgu1.rcgu.o");
      writeFileSync(keptByDylib, "kept-obj");
      writeFileSync(unrefOrphan, "dead-obj");

      // 1. 构造输出包含非 UTF-8 字节与 OSO 引用的桩 nm，验证 .dylib OSO 保护与容错解码
      const stubNm = join(fakeBin, "nm");
      writeFileSync(
        stubNm,
        [
          "#!/usr/bin/env bash",
          "printf '0000000000000000 - 00 0000   SO \\377\\376badutf8\\n'",
          `printf '0000000000000000 - 00 0000   OSO ${keptByDylib}\\n'`,
          "",
        ].join("\n"),
        { mode: 0o755 },
      );
      const resWithNm = runCleanup(root, [], { ...process.env, PATH: `${fakeBin}:${process.env.PATH}` });
      assert.equal(resWithNm.status, 0, resWithNm.stderr);
      assert.ok(existsSync(keptByDylib), ".dylib OSO 引用的 .rcgu.o 必须保留");
      assert.ok(!existsSync(unrefOrphan), "未被任何二进制或 .dylib 引用的孤儿 .rcgu.o 应被清理");

      // 2. 构造不含 nm 的受限 PATH，验证 macOS 缺 nm 时 fail-closed 不删孤儿 .rcgu.o
      const noNmBin = mkdtempSync(join(os.tmpdir(), "moss-no-nm-"));
      try {
        for (const tool of ["bash", "sh", "python3", "dirname", "pwd"]) {
          const which = spawnSync("which", [tool], { encoding: "utf8" });
          if (which.status === 0 && which.stdout.trim()) {
            symlinkSync(which.stdout.trim(), join(noNmBin, tool));
          }
        }
        const guardedOrphan = join(deps, "guarded-3333333333333333.cgu1.rcgu.o");
        writeFileSync(guardedOrphan, "must-stay-without-nm");
        const resNoNm = runCleanup(root, [], { ...process.env, PATH: noNmBin });
        assert.equal(resNoNm.status, 0, resNoNm.stderr);
        assert.ok(existsSync(guardedOrphan), "macOS 缺少 nm 时必须 fail-closed 保留 .rcgu.o");
      } finally {
        rmSync(noNmBin, { recursive: true, force: true });
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
      rmSync(fakeBin, { recursive: true, force: true });
    }
  },
);
