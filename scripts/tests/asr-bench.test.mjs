import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import {
  NO_LABELED_CORPUS_MESSAGE,
  classifyCommand,
  computeCer,
  isSafeHomophoneCandidate,
  runAsrBench,
} from "../voice-bench/asr-bench.mjs";

const execFileAsync = promisify(execFile);
const SCRIPT_PATH = fileURLToPath(new URL("../voice-bench/asr-bench.mjs", import.meta.url));

/** 写入最小合法的 16 kHz 单声道 16-bit PCM WAV 合成音频（0.25 秒正弦波）。 */
function writeSyntheticWav16k(filePath, freqHz = 440, durationSec = 0.25) {
  const sampleRate = 16_000;
  const numSamples = Math.floor(sampleRate * durationSec);
  const dataSize = numSamples * 2;
  const buf = Buffer.alloc(44 + dataSize);
  buf.write("RIFF", 0, "ascii");
  buf.writeUInt32LE(36 + dataSize, 4);
  buf.write("WAVE", 8, "ascii");
  buf.write("fmt ", 12, "ascii");
  buf.writeUInt32LE(16, 16); // PCM chunk size
  buf.writeUInt16LE(1, 20); // PCM format
  buf.writeUInt16LE(1, 22); // mono
  buf.writeUInt32LE(sampleRate, 24);
  buf.writeUInt32LE(sampleRate * 2, 28);
  buf.writeUInt16LE(2, 32);
  buf.writeUInt16LE(16, 34);
  buf.write("data", 36, "ascii");
  buf.writeUInt32LE(dataSize, 40);
  for (let i = 0; i < numSamples; i += 1) {
    const val = Math.round(Math.sin((2 * Math.PI * freqHz * i) / sampleRate) * 6000);
    buf.writeInt16LE(val, 44 + i * 2);
  }
  writeFileSync(filePath, buf);
}

test("VB3/VB4 字错率计算与命令词安全保护规则单元测试（已知样例精确断言）", () => {
  // 1. 已知样例 CER：三毫米椴木刻字 (7字) vs 三毫米段木刻字 (1 处替换 -> 1/7)
  const cer1 = computeCer("三毫米椴木刻字。", "三毫米段木刻字");
  assert.equal(cer1.edits, 1);
  assert.equal(cer1.refLen, 7);
  assert.ok(Math.abs(cer1.cer - 1 / 7) < 1e-9);

  // 2. 相同文本忽略标点 CER = 0
  const cer0 = computeCer("帮我看看激光机状态！", "帮我看看激光机状态");
  assert.equal(cer0.edits, 0);
  assert.equal(cer0.cer, 0);

  // 3. 命令词分类与安全规则
  assert.equal(classifyCommand("停"), "stop");
  assert.equal(classifyCommand("停止加工"), null);
  assert.equal(classifyCommand("退下"), "exit");
  assert.equal(classifyCommand("帮我刻椴木"), null);

  assert.equal(isSafeHomophoneCandidate("段木", "椴木").allowed, true);
  assert.equal(isSafeHomophoneCandidate("听", "停").allowed, false, "严禁把任何词映射成停止命令");
  assert.equal(isSafeHomophoneCandidate("跪下", "退下").allowed, false, "严禁把任何词映射成退出命令");
  assert.equal(isSafeHomophoneCandidate("停", "听").allowed, false, "严禁改写命令词本身");
  assert.equal(isSafeHomophoneCandidate("退下", "推下").allowed, false, "严禁改写退出命令词本身");
  assert.equal(isSafeHomophoneCandidate("木头", "M3 S1000").allowed, false, "严禁包含 G-code");
});

test("VB3/VB4 假识别器 + 合成语料 + 真实混噪跑通全流程：CER 准确、拦截违规命令词候选、归档齐全且脱敏", async () => {
  const runtimeDir = mkdtempSync(join(tmpdir(), "moss-vb3-pass-"));
  const corpusDay = join(runtimeDir, "asr-corpus", "2026-11-27");
  mkdirSync(corpusDay, { recursive: true });

  const fakeSecret = ["sk", "FakeAsrBenchSecretKey0123456789ABCD"].join("-");
  const fixtures = [
    {
      turn_id: "clip-wood-1",
      ref: "3 mm 椴木功率调到 60%，速度 400",
      tags: ["材料名", "数字"],
    },
    {
      turn_id: "clip-wood-2",
      ref: "三毫米椴木刻字",
      tags: ["材料名"],
    },
    {
      turn_id: "clip-stop-1",
      ref: "停",
      tags: ["命令词"],
    },
    {
      turn_id: "clip-exit-1",
      ref: "退下",
      tags: ["命令词"],
    },
    {
      turn_id: "clip-false-stop",
      ref: "今天听一下音乐",
      tags: ["有噪声"],
    },
  ];

  for (const item of fixtures) {
    writeSyntheticWav16k(join(corpusDay, `${item.turn_id}.wav`));
    writeFileSync(
      join(corpusDay, `${item.turn_id}.json`),
      JSON.stringify({
        turn_id: item.turn_id,
        date: "2026-11-27",
        simulated: false,
        raw_text: item.ref,
      }),
      "utf8",
    );
    writeFileSync(
      join(corpusDay, `${item.turn_id}.label.json`),
      JSON.stringify({
        turn_id: item.turn_id,
        date: "2026-11-27",
        reference_text: item.ref,
        verified_correct: false,
        tags: item.tags,
      }),
      "utf8",
    );
  }

  // 假识别器：对不同片段与噪声档位返回可控结果（含故意制造的命令词错读与敏感泄漏文本以验证拦截与脱敏）
  const fakeRecognizer = {
    id: "fake_sense_voice",
    label: "假 SenseVoice",
    secret: fakeSecret,
    async transcribe(wavPath, { sample, noiseLevel }) {
      assert.ok(existsSync(wavPath), `混噪后 WAV 必须存在：${wavPath}`);
      if (sample.turn_id === "clip-wood-1") {
        return {
          text: "三毫米段木功率调到百分之六十，速度四百",
          firstInterimMs: noiseLevel === 0 ? 120 : 180,
          finalMs: noiseLevel === 0 ? 310 : 420,
        };
      }
      if (sample.turn_id === "clip-wood-2") {
        // 故意在异常文本中附带本机路径与伪密钥，验证脱敏彻底清除
        return {
          text: noiseLevel === 0 ? "三毫米段木刻字" : `三毫米段木刻字 api_key=${fakeSecret} ${runtimeDir}`,
          firstInterimMs: 110,
          finalMs: 290,
        };
      }
      if (sample.turn_id === "clip-stop-1") {
        // 安静档命中“停”，持续机器噪声档漏听成“听”（验证命令词召回统计 + 禁止生成 `听->停` 纠错候选）
        return {
          text: noiseLevel === 0 ? "停" : "听",
          firstInterimMs: 90,
          finalMs: 210,
        };
      }
      if (sample.turn_id === "clip-exit-1") {
        // 听成“跪下”，验证禁止生成 `跪下->退下` 同音纠错候选
        return {
          text: "跪下",
          firstInterimMs: 100,
          finalMs: 230,
        };
      }
      return {
        text: noiseLevel === 888 ? "停" : "今天听一下音乐",
        firstInterimMs: 130,
        finalMs: 260,
      };
    },
  };

  try {
    const res = await runAsrBench({
      runtimeDir,
      noiseLevels: [0, 888],
      recognizers: [fakeRecognizer],
    });
    assert.equal(res.exitCode, 0);
    assert.ok(res.archiveDir.startsWith(join(runtimeDir, "voice-bench", "asr-")));

    const expectedFiles = ["results.jsonl", "report.md", "suggestions.json"];
    for (const name of expectedFiles) {
      const full = join(res.archiveDir, name);
      assert.ok(existsSync(full), `缺少归档文件：${name}`);
      const content = readFileSync(full, "utf8");
      assert.ok(content.trim().length > 0, `归档文件不应为空：${name}`);
      assert.ok(!content.includes(fakeSecret), `${name} 绝不能泄露 API 密钥`);
      assert.ok(!content.includes(runtimeDir), `${name} 绝不能泄露本机绝对路径`);
    }

    // 校验已知样例 CER 精确结果
    const resultRows = readFileSync(join(res.archiveDir, "results.jsonl"), "utf8")
      .trim()
      .split("\n")
      .map((l) => JSON.parse(l));
    assert.equal(resultRows.length, fixtures.length * 2);
    const wood2Clean = resultRows.find((r) => r.turn_id === "clip-wood-2" && r.noise_level === 0);
    assert.equal(wood2Clean.edits, 1);
    assert.equal(wood2Clean.ref_len, 7);
    assert.equal(wood2Clean.cer, Number((1 / 7).toFixed(4)));

    // 校验候选产出与命令词保护规则
    const suggestions = JSON.parse(readFileSync(join(res.archiveDir, "suggestions.json"), "utf8"));
    assert.equal(suggestions.auto_applied, false, "候选绝不能自动生效");
    assert.ok(
      suggestions.hotword_candidates.some((c) => c.term === "椴木" && c.miss_count >= 2),
      "反复识别错的领域词“椴木”应进入热词候选",
    );
    assert.ok(
      suggestions.homophone_correction_candidates.some((c) => c.wrong === "段木" && c.right === "椴木"),
      "安全领域同音错词“段木=椴木”应进入同音纠错候选",
    );
    assert.ok(
      suggestions.number_unit_normalization_candidates.some((c) => c.raw === "四百" && c.normalized === "400"),
      "口语数字“四百 -> 400”应进入数字单位规范化候选",
    );
    assert.ok(
      suggestions.number_unit_normalization_candidates.some((c) => c.raw === "三毫米" && c.normalized === "3 mm"),
      "口语单位“三毫米 -> 3 mm”应进入数字单位规范化候选",
    );

    // 核心安全断言：任何涉及 停 / 退下 的违规候选绝不能出现在 homophone_correction_candidates，必须进 discarded_candidates 且写入 report.md
    for (const cand of suggestions.homophone_correction_candidates) {
      assert.equal(
        isSafeHomophoneCandidate(cand.wrong, cand.right).allowed,
        true,
        `违规候选泄漏进同音纠错建议：${cand.wrong} -> ${cand.right}`,
      );
    }
    assert.ok(
      suggestions.discarded_candidates.some((d) => d.right === "停" || d.right === "退下"),
      "违规命令词映射候选必须记录在 discarded_candidates 中",
    );
    const report = readFileSync(join(res.archiveDir, "report.md"), "utf8");
    assert.ok(report.includes("命令词保护已拦截并丢弃违规同音候选"), "report.md 须注明被拦截丢弃的违规候选");
    assert.ok(report.includes("命令词召回与误报"), "report.md 须含命令词召回与误报表");
    assert.ok(report.includes("错得最多的前 30 个片段"), "report.md 须含错得最多的前 30 个片段");
  } finally {
    rmSync(runtimeDir, { recursive: true, force: true });
  }
});

test("VB3/VB4 无已标注语料时提示先采集并标注，返回非 0 状态码", async () => {
  const emptyDir = mkdtempSync(join(tmpdir(), "moss-vb3-empty-"));
  try {
    let failed = null;
    try {
      await execFileAsync(process.execPath, [SCRIPT_PATH, "--runtime-dir", emptyDir], {
        encoding: "utf8",
      });
    } catch (error) {
      failed = error;
    }
    assert.ok(failed, "没有已标注语料时必须返回非 0 退出码");
    assert.notEqual(failed.code, 0);
    assert.ok(
      String(failed.stderr || "").includes(NO_LABELED_CORPUS_MESSAGE),
      `stderr 应提示先开采集与标注，实际：${failed.stderr}`,
    );
  } finally {
    rmSync(emptyDir, { recursive: true, force: true });
  }
});
