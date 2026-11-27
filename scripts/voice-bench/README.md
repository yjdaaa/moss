# 真小智“文字直通”自动化联调脚本

面向 AI 代理（Claude、Antigravity 等）和本机开发者：跳过麦克风与语音识别，把测试语料直接送进已连上的真小智会话，完整运行后半段链路（同音纠偏、判停、快答/大模型、工具与门禁、副屏推送、TTS 与播放泵），并自动留档完整过程记录。

## 前提条件

1. 在 MOSS 桌面端主窗口打开 **语音中心 → 服务 → 内置语音服务**，确认内置语音服务正在运行且处于 **对话模式**。
2. 打开 **语音中心 → 记录与诊断 → AI 联调与测试**，勾选 **“允许本机 AI 调试注入”** 并完成二次确认。开启后：
   - MOSS 仅在 `127.0.0.1` 开放受限调试入口（默认端口 `18009`，被占用时自动顺延并记录在 `<runtime>/debug/voice-debug.json`）；
   - 在运行目录生成 `<runtime>/debug/voice-debug.token`（权限 `0600`），关闭开关或开启满 24 小时自动作废；
   - 所有模拟轮次强制带模拟标记，工具执行入口硬拦截发送雕刻、移动、回零、解锁、倍率、家居控制、本机 Agent 与固件/唤醒词推送，且绝不写入长期记忆或人物资料。
3. 唤醒真小智（喊“你好小智”或按对话键），或开启常连模式让小智在线。

## 运行方式

```bash
node scripts/voice-bench/live-xiaozhi-debug.mjs
```

### 常用参数

| 参数 | 说明 | 默认值 |
| --- | --- | --- |
| `--corpus <path>` | 指定语料 JSON 文件 | `scripts/voice-bench/live-corpus.json` |
| `--device <device_id>` | 指定目标小智设备编号 | 自动选第一台在线设备 |
| `--runtime-dir <path>` | 指定 MOSS 运行目录（用于读取令牌、过程记录与写出归档） | 系统默认运行目录 |
| `--url http://127.0.0.1:<port>` | 显式指定本机调试入口地址（仅接受回环地址） | 读取 `<runtime>/debug/voice-debug.json` 或 `http://127.0.0.1:18009` |
| `--token-file <path>` | 显式指定令牌文件路径 | `<runtime>/debug/voice-debug.token` |
| `--out-dir <path>` | 显式指定归档输出目录 | `<runtime>/voice-bench/<run-id>/` |
| `--turn-timeout-ms <ms>` | 单轮等待上限（超时记失败并继续下一轮） | `90000` |

## 默认语料覆盖

`scripts/voice-bench/live-corpus.json` 包含 8 轮联调场景：
1. 状态快答（验证 `fast_read`）；
2. 同音错字纠偏（“三毫米段木” → “椴木”）；
3. 生成木牌雕刻预览（只生成预览，不发送）；
4. 多轮追问改参数（验证模拟会话内多轮上下文可用）；
5. 调设备自身音量与屏幕亮度（验证无风险设备控制放行）；
6. 故意要求“直接开始雕刻”（验证 VA2 模拟门禁拦截并如实告知模型）；
7. 停止快路径（仅在确认无活跃激光任务时运行，验证 `fast_stop`）；
8. “退下”（验证 `fast_exit` 收尾）。

## 输出与退出码

每次运行在 `<runtime>/voice-bench/<run-id>/` 生成 5 个已脱敏文件（不含 API 密钥、调试令牌、设备 token 与本机绝对路径）：

- `turns.jsonl`：每轮一行完整明细（注入与纠偏文字、路径、阶段耗时、工具与参数、门禁拒绝、回复全文与已播文本、播放欠载与卡顿时长）；
- `timeline.jsonl`：本次运行时间窗内的 `ProcessTrace` 机器可读片段；
- `timeline.txt`：本次运行时间窗内的 `ProcessTrace` 中文时间线片段；
- `report.md`：人类与 AI 均可直接阅读的逐轮诊断表；
- `summary.json`：通过/失败/跳过计数、各阶段 P50/P90 耗时、失败轮次列表与门禁命中数。

脚本退出前会在标准输出打印归档目录路径。
- 退出码 `0`：全部执行轮次通过；
- 退出码非 `0`：没有在线设备（输出“请先唤醒小智（喊‘你好小智’或按对话键），或开启常连模式”）、令牌无效/过期，或存在失败/超时轮次。

---

# 识别（ASR）离线测评与候选词产出脚本（VB3）

用本机真实小智录音语料量化各识别模型在激光工作室场景下的字错率（CER）、命令词召回与误报、数字与单位准确率、材料/术语准确率、首中间/最终延迟以及噪声退化，并自动产出热词、同音纠错与数字单位规范化候选。

## 前提条件

1. 在 MOSS 桌面端主窗口打开 **语音中心 → 记录与诊断 → 对话记录与排障**。
2. 勾选 **“同时保存完整过程记录与原始录音（按音源打标，供离线识别测评复用）”**（录音按日期落盘 `<runtime>/logs/process-trace/<日期>/voice-files/`，并在时间线与 `.label.json` 显式区分 `live_mic` / `e2e_playback` / `digital_inject` / `text_inject` 4 种音源；兼容独立语料目录 `<runtime>/asr-corpus/<日期>/`）。
3. 对着真实小智说测试语料，随后在 **“对话记录与排障”** 逐条点击 **“▶ 听录音”**、**“✓ 识别正确”** 或 **“纠偏此句 → 加入同音纠偏”** 保存真值标注，生成与对应录音同名的 `<录音前缀>.label.json`。

## 运行方式

```bash
node scripts/voice-bench/asr-bench.mjs
```

### 常用参数

| 参数 | 说明 | 默认值 |
| --- | --- | --- |
| `--corpus-dir <path>` | 显式指定单一已标注语料目录（传入时仅扫描该目录） | 同时扫描 `<runtime>/asr-corpus` 与 `<runtime>/logs/process-trace`（自动读取自定义过程记录目录） |
| `--runtime-dir <path>` | MOSS 运行目录（用于发现已安装本地模型、云端配置与写出归档） | 系统默认运行目录 |
| `--noise-levels <list>` | 噪声档位（复用 `chain_clips.py`，经 `python3 -I` 混噪）：`0`/`clean` 安静、`888`/`fan`/`machine` 排风扇气泵持续噪声、`999`/`pump`/`burst` 气泵咔嗒敲击突发噪声 | `0,888,999` |
| `--models <list>` | 指定要测的模型 ID（逗号分隔），如 `sense_voice,qwen3_asr_0_6b,cloud` | 自动测试全部已安装本地 ASR 模型及已配置的云端 ASR |
| `--helper <path>` | `moss-voice-local` 可执行文件路径 | 自动探测 |
| `--out-dir <path>` | 显式指定归档输出目录 | `<runtime>/voice-bench/asr-<run-id>/` |

## 输出与安全规则

每次运行在 `<runtime>/voice-bench/asr-<run-id>/` 生成 3 个脱敏文件（绝不含 API 密钥、令牌与本机绝对路径）：

- `results.jsonl`：每条语料 × 每模型 × 每噪声档位一行（原文、识别结果、CER、首中间结果延迟、最终延迟）；
- `report.md`：按模型与噪声汇总的 CER/退化/延迟表、命令词（停/停止/退下）召回与误报表、数字单位与材料术语准确率、拦截说明以及错得最多的前 30 个片段；
- `suggestions.json`：三类候选（`hotword_candidates`、`homophone_correction_candidates`、`number_unit_normalization_candidates`）以及被命令词安全规则拦截的 `discarded_candidates`。

**安全规则**：
- 同音纠错候选严禁把任何内容映射成停止/退出等命令词，也不允许改写命令词本身；违规候选直接丢弃并在报告里注明。
- 所有候选只写入 `suggestions.json`，脚本绝不自动修改代码或 `settings.json`；是否采纳由人或 AI 复核并补测试决定。
- 若语料中包含不同音源（`live_mic` 小智实时说话、`e2e_playback` 整体测试外放录音、`digital_inject` 数字录音直通），报告会按 `input_source` 分组展示指标，避免外放回采、数字直通与现场真人原声混淆。
- 脚本退出前打印归档目录路径；未找到任何已标注语料时提示先开采集与标注，并返回非 `0` 状态码。

---

# 真小智整体测试脚本（VC2 / VC3 / VD1 / VD3：真人录音外放 → 真麦克风 → 全链路 → 4 环节归因）

一条命令跑完“电脑喇叭外放真人测试录音 → 真小智麦克风拾音 → MOSS 全链路处理 → 小智扬声器播报”，自动按 **拾音判停 / 识别纠偏 / 意图工具 / 合成播放** 四个环节归因并按说话人分组统计，定位问题出在哪一段并给出对应代码位置或参数调整建议。

## 前提条件

1. 在 MOSS 桌面端主窗口打开 **语音中心 → 记录与诊断 → AI 联调与测试**：
   - 在 **“录制测试语料”** 中至少录完一位说话人的全部语料（使用电脑麦克风录制 16 kHz 单声道 WAV，存放在本机 `<runtime>/voice-bench/recordings/<speaker>/<corpus-id>.{wav,json}`，支持多位说话人标签，不进诊断包、不用于声纹）；
   - 勾选 **“允许本机 AI 调试注入”** 并完成二次确认；
   - （可选推荐）在下方 **“对话记录与排障”** 勾选 **“同时保存完整过程记录与原始录音”**，整体测试会自动为外放录音回写 `input_source: "e2e_playback"` 与 `label_source: "e2e-playback"`（带说话人标签）。
2. 唤醒真小智（喊“你好小智”或按对话键），或开启常连模式让设备保持在线。
3. 将小智摆放在距离电脑喇叭 **30–50 cm** 处，调至适中音量（避免过小拾不到音或过大爆音）。
4. 确认本机播放器可用：macOS 用 `afplay`、Windows 用 PowerShell `System.Media.SoundPlayer`、Linux 用 `paplay` 或 `aplay`。

## 声音来源规则（VD1 / VD3：只用真人录音）

- **绝不使用合成声音**：已彻底移除本地 TTS 与系统朗读（`say` / `System.Speech` / `espeak`），不保留回退开关。
- **默认素材目录**：`<runtime>/voice-bench/recordings/<speaker>/<corpus-id>.wav`（电脑麦克风直录真人语音）。未指定 `--speaker` 时按已录制说话人轮流覆盖各句；传入 `--speaker <标签>` 时只使用该说话人。
- **缺录音处理**：
  - 某句语料没有可用的真人录音时，脚本跳过该句，并在终端输出与 `report.md` 的 **“缺少真人录音的语料（已跳过）”** 列出清单，提示前往 App 录制；
  - 所有语料都没有真人录音时，脚本不打开测试时间窗，直接提示前往 App 录制并以非 `0` 状态码退出。
- **设备录音隔离**：VB 的 `<runtime>/asr-corpus` 是经小智麦克风采集的录音，再次外放会被麦克风染色两次，默认不作为外放素材；仅在显式传入 `--include-device-corpus` 时补充使用，并在报告与汇总中明确标为 **“二次录制”**。

## 运行方式

```bash
node scripts/voice-bench/xiaozhi-e2e.mjs
```

### 常用参数

| 参数 | 说明 | 默认值 |
| --- | --- | --- |
| `--corpus <path>` | 指定整体测试语料 JSON 文件 | `scripts/voice-bench/e2e-corpus.json` |
| `--device <device_id>` | 指定目标小智设备编号 | 自动选第一台在线设备 |
| `--speaker <标签>` | 只使用指定说话人的真人录音（例如 `我`、`家人 A`） | 全部已录制说话人轮流 |
| `--recordings-dir <path>` | 指定真人录音目录 | `<runtime>/voice-bench/recordings` |
| `--include-device-corpus` | 显式允许使用 `<runtime>/asr-corpus` 手工标注录音补充（报告标明“二次录制”） | 关闭 |
| `--max-minutes <1..60>` | 测试时间窗最长锁定时长（分钟） | `30`（硬上限 `60`） |
| `--noise <fan\|pump\|burst>` | 外放期间叠加低音量循环背景噪声（经 `python3 -I` 复用 `chain_clips.py`） | 关闭 |
| `--noise-gain <0..1>` | 背景噪声增益 | `0.22` |
| `--compare-digital` | 在时间窗内用同一份真人录音追加服务端数字 PCM 对照注入，逐句量化麦克风与房间 CER 损失 | 关闭 |
| `--turn-timeout-ms <ms>` | 单句等待上限（超时记失败并继续下一句） | `90000` |
| `--quiet-ms <ms>` | 外放前/后要求设备处于 `listening` 且安静的时长 | `1000` |
| `--runtime-dir <path>` | 指定 MOSS 运行目录 | 系统默认运行目录 |
| `--url http://127.0.0.1:<port>` | 显式指定本机调试入口地址（仅回环） | 自动读取 `<runtime>/debug/voice-debug.json` |
| `--token-file <path>` | 显式指定调试令牌文件 | `<runtime>/debug/voice-debug.token` |
| `--out-dir <path>` | 显式指定归档输出目录 | `<runtime>/voice-bench/e2e-<run-id>/` |
| `--skip-calibration` | 跳过开跑前外放校准（仅调试脚本用，不建议在真机跳过） | 关闭 |

## 安全锁、校准与串行执行

- **测试时间窗（VC1）**：开跑前自动调用 `POST /debug/v1/bench-window` 锁定该设备，期间包括真实麦克风拾音在内的**所有轮次**一律走 `simulated = true`（`sim_guard` 拦截物理/外部工具、有加工任务时不发 `0x18`、不写长期记忆、对话记录标“模拟（整体测试）”）；桌面“已连接设备”显示“整体测试中（物理动作已锁定）”与“立即结束测试”按钮。无论正常结束还是异常中断，脚本都在 `finally` 中关闭时间窗、停掉背景噪声并恢复 `ProcessTrace` 原状。
- **开跑前校准**：选用第一条可用真人录音外放校准，读取该设备的 `recent_peak_level` 与识别结果；若识别不到或电平明显过低/过高，打印“把小智放到离喇叭 30–50 cm、调高或调低电脑音量”并以非 `0` 状态码退出。
- **严格串行**：只在设备处于 `listening` 且连续安静 1 秒后开始外放；外放结束后等待 `thinking` → `speaking` → 回到 `listening` 且安静 1 秒再放下一句，确保外放与小智播报零重叠。

## 输出归档与自动语料标注

每次运行在 `<runtime>/voice-bench/e2e-<run-id>/` 生成 6 个全脱敏文件（不含密钥、令牌与本机绝对路径）：

- `turns.jsonl`：每轮一行 4 环节归因（拾音判停：外放/服务端时刻、首字被吞、抢话、判停延迟；识别纠偏：原文/纠偏文本与双 CER、可选数字对照损益；意图工具：路径、工具与预期命中、门禁拦截；合成播放：外放结束到首声/播完耗时、首包、欠载卡顿）及说话人标签；
- `timeline.jsonl` / `timeline.txt`：本次时间窗内的 `ProcessTrace` 片段；
- `report.md`：总览表、**按说话人分组统计表**、**缺少真人录音的语料清单**、4 个环节各自最差项置顶诊断表，以及按严重程度排序的“建议优先调整的环节”（附对应代码位置与参数名，如 `PREROLL_MS`、`BASE_SILENCE_MS`、`conversation.rs` 纠错表、热词等）；
- `summary.json`：各环节通过率、按说话人指标 `by_speaker`、缺失语料 `missing_turns`、P50/P90 耗时、平均 CER 与数字对照差值；
- `audio-manifest.json`：外放音频清单（仅文件名、说话人标签、来源类型与时长，绝不复制用户原始录音）。

若开启了完整过程记录（默认开跑期间自动打开）或独立的 `asr-corpus` 采集，外放经真麦克风保存的每条录音都会按对应录音文件名自动生成 `<录音前缀>.label.json`（不覆盖已有标注，正确文字 = 外放原句，`input_source: "e2e_playback"`、`label_source: "e2e-playback"` 及说话人信息）；`--compare-digital` 数字对照注入绝不写入 `asr-corpus`。

## 语料编写规范

编辑自定义整体测试语料（或扩展 `scripts/voice-bench/e2e-corpus.json`）时必须遵守以下规则：
1. **禁止编写触发真实发送、续雕、走边框、移头、回零、解锁、实时倍率、家居控制、本机 Agent 或固件推送的句子**——唯一例外是专门验证安全门禁的条目，该条目必须显式声明 `"expect_simulation_gate": true` 且仅能在测试时间窗打开的前提下运行；
2. 停止快路径测试句（如“停”）必须标记 `"require_idle_laser": true`，确保存在活跃激光任务时自动跳过；
3. 句中停顿抢话测试句使用 `segments: [{ text, pause_ms_after }]` 精确标注停顿时间（例如 `700` ms），App 内录制引导会自动向朗读者显示“这里停顿约 0.7 秒”；
4. 每句可填写 `expect_route`、`expect_tool`、`key_terms`（关键术语验收词）与 `tags`；**严禁通过删改语料或放宽预期来让失败的测试“看起来通过”**。
