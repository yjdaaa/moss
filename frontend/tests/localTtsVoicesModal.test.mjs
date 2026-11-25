import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const TEST_DIR = path.dirname(fileURLToPath(import.meta.url));
const SRC_DIR = path.resolve(TEST_DIR, "../src");

test("语音中心本地合成模型包名称可点击打开语言包与本机缓存音色试听弹窗", () => {
  const apiSrc = fs.readFileSync(path.join(SRC_DIR, "voiceApi.js"), "utf8");
  assert.match(
    apiSrc,
    /localTtsVoices:\s*\(modelId\)\s*=>\s*invokeVoice\("voice_builtin_models_list",\s*\{\s*section:\s*"local_tts",\s*input:\s*\{\s*model_id:\s*modelId\s*\}\s*\}\)/,
    "voiceApi 须通过已授权的 voice_builtin_models_list('local_tts') 读取语言包与缓存状态",
  );
  assert.match(
    apiSrc,
    /localTtsPreview:\s*\(modelId,\s*speaker,\s*speed\)\s*=>\s*invokeVoice\("voice_builtin_models_test",\s*\{\s*section:\s*"local_tts",\s*input:\s*speed === undefined \? \{\s*model_id:\s*modelId,\s*speaker\s*\}\s*:\s*\{\s*model_id:\s*modelId,\s*speaker,\s*speed\s*\}\s*\}\)/,
    "voiceApi 须通过已授权的 voice_builtin_models_test('local_tts') 读取或生成本机试听 WAV",
  );

  // LocalModelsGroup 实现在 voice-builtin/ 下（2026-11-13 从 VoiceBuiltinCard.jsx 原样搬出）
  const cardSrc = fs.readFileSync(path.join(SRC_DIR, "components/voice-builtin/LocalModelsGroup.jsx"), "utf8");
  assert.match(
    cardSrc,
    /className="voice-tts-model-trigger"[\s\S]*?onClick=\{\(\)\s*=>\s*setTtsVoicesModalModel\(model\)\}/,
    "合成模型包表格中的模型名称须可点击并打开音色试听弹窗",
  );
  assert.match(
    cardSrc,
    /<LocalTtsVoicesModal[\s\S]*?visible=\{Boolean\(ttsVoicesModalModel\)\}[\s\S]*?onApplyVoice=\{applyVoiceFromModal\}/,
    "LocalModelsGroup 须挂载 LocalTtsVoicesModal 并接入一键选用音色回调",
  );

  // n129：没有的功能不显示——单音色模型不渲染音色栏、不支持调语速的模型不渲染语速框，“查看语言包与试听”挪到试听那一行
  const settingsSrc = fs.readFileSync(path.join(SRC_DIR, "components/voice-builtin/LocalTtsSettings.jsx"), "utf8");
  assert.match(settingsSrc, /\{voices && \(\s*<label>/, "只有多音色模型才渲染音色栏");
  assert.match(settingsSrc, /\{!speedFixed && \(\s*<label>/, "支持调语速的模型才渲染语速框");
  assert.match(settingsSrc, /\{!voices && \([\s\S]*?查看语言包与试听/, "单音色模型在试听那一行保留语言包入口");
  assert.doesNotMatch(settingsSrc, /只有一个音色/, "不再用一行灰字占位");

  const modalSrc = fs.readFileSync(path.join(SRC_DIR, "components/LocalTtsVoicesModal.jsx"), "utf8");
  assert.match(modalSrc, /支持的语言包/, "弹窗须展示支持的语言包区块");
  assert.match(modalSrc, /本机已缓存/, "弹窗须标注已缓存在本机磁盘的音色");
  assert.match(modalSrc, /选用此音色/, "弹窗须支持直接一键选用音色");
});
